use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

use crate::config_cf::{self, ConfigRow};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_files: usize,
    pub max_parts: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid or unsupported CONFIG staging input: {0}")]
    Invalid(&'static str),
    #[error("CONFIG staging I/O: {0}")]
    Io(#[from] io::Error),
    #[error("CONFIG row stream: {0}")]
    Rows(#[from] crate::sql_rows::Error),
    #[error("writing staged CONFIG as CF: {0}")]
    Cf(#[from] config_cf::Error),
}

struct Pending {
    row: ConfigRow,
    offset: u64,
    bytes: u64,
    parts: usize,
}

pub struct Builder {
    file: File,
    year_offset: i32,
    limits: Limits,
    rows: Vec<ConfigRow>,
    offsets: Vec<u64>,
    names: HashSet<String>,
    pending: Option<Pending>,
    total: u64,
    part_count: usize,
    poisoned: bool,
}

pub struct Spool {
    file: File,
    year_offset: i32,
    rows: Vec<ConfigRow>,
    offsets: Vec<u64>,
    part_count: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRow {
    name: String,
    creation: String,
    modified: String,
    attributes: i64,
    part_no: i64,
    data_size: u64,
    stored_size: u64,
    data: String,
}

fn decode_name(value: &str) -> Result<String, &'static str> {
    fn digit(byte: u8) -> Result<u8, &'static str> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err("invalid CONFIG filename hex"),
        }
    }
    let hex = value
        .strip_prefix("0x")
        .ok_or("missing CONFIG filename hex prefix")?;
    if hex.is_empty() || hex.len() > 512 || hex.len() % 4 != 0 {
        return Err("invalid CONFIG filename size");
    }
    let mut ascii = Vec::with_capacity(hex.len() / 4);
    let (chunks, remainder) = hex.as_bytes().as_chunks::<4>();
    if !remainder.is_empty() {
        return Err("invalid CONFIG filename hex");
    }
    for &[a, b, c, d] in chunks {
        let low = digit(a)? * 16 + digit(b)?;
        let high = digit(c)? * 16 + digit(d)?;
        if high != 0 || !low.is_ascii() {
            return Err("unsupported non-ASCII CONFIG filename");
        }
        ascii.push(low);
    }
    String::from_utf8(ascii).map_err(|_| "invalid CONFIG filename")
}

pub fn read_framed(
    input: impl Read,
    mut builder: Builder,
    max_row_bytes: usize,
) -> Result<Spool, Error> {
    crate::sql_rows::read_framed_rows(input, max_row_bytes, |bytes| {
        let row: WireRow = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let expected = row
            .stored_size
            .checked_add(2)
            .and_then(|n| n.checked_div(3))
            .and_then(|n| n.checked_mul(4))
            .ok_or("encoded size overflow")?;
        if u64::try_from(row.data.len()).ok() != Some(expected) {
            return Err("encoded size mismatch".into());
        }
        let data = STANDARD
            .decode(row.data.as_bytes())
            .map_err(|_| "invalid base64")?;
        builder
            .push(
                ConfigRow {
                    name: decode_name(&row.name).map_err(str::to_owned)?,
                    creation: row.creation,
                    modified: row.modified,
                    attributes: row.attributes,
                    part_no: row.part_no,
                    data_size: row.data_size,
                    stored_size: row.stored_size,
                },
                &data,
            )
            .map_err(|error| error.to_string())
    })?;
    builder.finish()
}

impl Builder {
    pub fn new(year_offset: i32, limits: Limits) -> Result<Self, Error> {
        Self::new_in(year_offset, limits, &std::env::temp_dir())
    }

    pub fn new_in(
        year_offset: i32,
        limits: Limits,
        directory: &std::path::Path,
    ) -> Result<Self, Error> {
        if year_offset < 0 || limits.max_files == 0 || limits.max_parts == 0 {
            return Err(Error::Invalid("invalid limits or year offset"));
        }
        Ok(Self {
            file: tempfile::tempfile_in(directory)?,
            year_offset,
            limits,
            rows: Vec::new(),
            offsets: Vec::new(),
            names: HashSet::new(),
            pending: None,
            total: 0,
            part_count: 0,
            poisoned: false,
        })
    }

    fn complete_pending(&mut self) -> Result<(), Error> {
        if let Some(mut item) = self.pending.take() {
            if item.bytes != item.row.data_size {
                return Err(Error::Invalid("incomplete file"));
            }
            item.row.part_no = 0;
            item.row.stored_size = item.bytes;
            self.rows.push(item.row);
            self.offsets.push(item.offset);
        }
        Ok(())
    }

    pub fn push(&mut self, row: ConfigRow, data: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Invalid("staging previously failed"));
        }
        self.poisoned = true;
        self.push_inner(row, data)?;
        self.poisoned = false;
        Ok(())
    }

    fn push_inner(&mut self, row: ConfigRow, data: &[u8]) -> Result<(), Error> {
        if row.name.is_empty()
            || row.name.len() > 128
            || !row.name.is_ascii()
            || row.name.chars().any(char::is_control)
            || row.attributes != 0
            || row.data_size > self.limits.max_file_bytes
            || u64::try_from(data.len()).ok() != Some(row.stored_size)
        {
            return Err(Error::Invalid("unsupported file metadata"));
        }
        config_cf::timestamp(&row.creation, self.year_offset)
            .and_then(|_| config_cf::timestamp(&row.modified, self.year_offset))
            .map_err(|_| Error::Invalid("invalid file timestamp"))?;
        let new_file = self
            .pending
            .as_ref()
            .is_none_or(|item| item.row.name != row.name);
        if new_file {
            if let Some(item) = &self.pending
                && item.row.name >= row.name
            {
                return Err(Error::Invalid("files are not ordered"));
            }
            self.complete_pending()?;
            if self.rows.len() >= self.limits.max_files
                || row.part_no != 0
                || !self.names.insert(row.name.to_ascii_lowercase())
            {
                return Err(Error::Invalid("duplicate, excessive, or missing file"));
            }
            self.pending = Some(Pending {
                row: row.clone(),
                offset: self.total,
                bytes: 0,
                parts: 0,
            });
        }
        let item = self
            .pending
            .as_mut()
            .ok_or(Error::Invalid("missing file"))?;
        if item.row.name != row.name
            || item.row.creation != row.creation
            || item.row.modified != row.modified
            || item.row.attributes != row.attributes
            || item.row.data_size != row.data_size
            || item.parts >= self.limits.max_parts
            || i64::try_from(item.parts).ok() != Some(row.part_no)
        {
            return Err(Error::Invalid("inconsistent file part"));
        }
        let bytes = item
            .bytes
            .checked_add(row.stored_size)
            .ok_or(Error::Invalid("file size overflow"))?;
        let total = self
            .total
            .checked_add(row.stored_size)
            .ok_or(Error::Invalid("staging size overflow"))?;
        let part_count = self
            .part_count
            .checked_add(1)
            .ok_or(Error::Invalid("part count overflow"))?;
        if bytes > item.row.data_size || total > self.limits.max_total_bytes {
            return Err(Error::Invalid("staging size limit"));
        }
        self.file.write_all(data)?;
        item.bytes = bytes;
        item.parts += 1;
        self.total = total;
        self.part_count = part_count;
        Ok(())
    }

    pub fn finish(mut self) -> Result<Spool, Error> {
        if self.poisoned {
            return Err(Error::Invalid("staging previously failed"));
        }
        self.complete_pending()?;
        if !self.names.contains("root") {
            return Err(Error::Invalid("CONFIG has no root"));
        }
        self.file.flush()?;
        Ok(Spool {
            file: self.file,
            year_offset: self.year_offset,
            rows: self.rows,
            offsets: self.offsets,
            part_count: self.part_count,
        })
    }
}

impl Spool {
    pub fn write_cf(&mut self, out: &mut dyn Write) -> Result<(), Error> {
        for index in 0..self.rows.len() {
            if self
                .rows
                .get(index)
                .is_some_and(|row| row.stored_size == 64)
            {
                let mut prefix = [0_u8; 8];
                self.reader(index)?.read_exact(&mut prefix)?;
                if &prefix == b"STORHDR\0" {
                    return Err(Error::Invalid(
                        "unresolved SQL binary storage reference in CONFIG",
                    ));
                }
            }
        }
        let metadata = self.rows.clone();
        config_cf::write_config(&metadata, self.year_offset, self, out)?;
        Ok(())
    }

    pub fn reader(&mut self, index: usize) -> Result<io::Take<&mut File>, Error> {
        let offset = *self
            .offsets
            .get(index)
            .ok_or(Error::Invalid("entry index"))?;
        let length = self
            .rows
            .get(index)
            .ok_or(Error::Invalid("entry index"))?
            .stored_size;
        self.file.seek(SeekFrom::Start(offset))?;
        Ok((&mut self.file).take(length))
    }

    pub fn part_count(&self) -> usize {
        self.part_count
    }
    pub fn rows(&self) -> &[ConfigRow] {
        &self.rows
    }
    pub fn year_offset(&self) -> i32 {
        self.year_offset
    }
}

impl v8cont::DataSource for Spool {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let offset = *self
            .offsets
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch("CONFIG entry index".into()))?;
        let length = self
            .rows
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch("CONFIG entry index".into()))?
            .stored_size;
        self.file.seek(SeekFrom::Start(offset))?;
        let copied = io::copy(&mut (&mut self.file).take(length), out)?;
        if copied != length {
            return Err(v8cont::Error::LayoutMismatch(
                "truncated CONFIG staging file".into(),
            ));
        }
        Ok(())
    }
}
