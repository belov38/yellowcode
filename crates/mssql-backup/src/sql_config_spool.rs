use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

use crate::config_cf::{self, CfOptions, CfView, ConfigRow};
use crate::dynamic_update::{self, Summary};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use v8cont::V8Time;

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
    #[error("invalid or unsupported CONFIG staging input: part {part} of a file: {reason}")]
    Part { part: i64, reason: String },
    #[error("CONFIG staging I/O: {0}")]
    Io(#[from] io::Error),
    #[error("CONFIG row stream: {0}")]
    Rows(#[from] crate::sql_rows::Error),
    #[error("writing staged CONFIG as CF: {0}")]
    Cf(#[from] config_cf::Error),
    #[error("{0}")]
    DynamicUpdate(#[from] dynamic_update::Error),
}

fn note(field: &'static str, part: i64, expected: Option<i64>, observed: Option<i64>) {
    if let Some(trace) = bak_trace::Trace::current() {
        trace.refusal(&bak_trace::Refusal {
            class: "config_part",
            field,
            part: Some(part),
            expected,
            observed,
        });
    }
}

fn n(value: u64) -> Option<i64> {
    i64::try_from(value).ok()
}

struct Pending {
    row: ConfigRow,
    creation: V8Time,
    modified: V8Time,
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
    entry_parts: Vec<usize>,
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
    entry_parts: Vec<usize>,
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
            entry_parts: Vec::new(),
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
                let last = i64::try_from(item.parts.saturating_sub(1)).unwrap_or(i64::MAX);
                note("data_size", last, n(item.row.data_size), n(item.bytes));
                return Err(Error::Part {
                    part: i64::try_from(item.parts.saturating_sub(1)).unwrap_or(i64::MAX),
                    reason: format!(
                        "the parts add up to {} bytes, fewer than DataSize ({})",
                        item.bytes, item.row.data_size
                    ),
                });
            }
            bak_trace::Trace::current().inspect(|t| t.config_file(item.parts as u64, item.bytes));
            item.row.part_no = 0;
            item.row.stored_size = item.bytes;
            self.rows.push(item.row);
            self.offsets.push(item.offset);
            self.entry_parts.push(item.parts);
        }
        Ok(())
    }

    pub fn push(&mut self, row: ConfigRow, data: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Invalid("staging previously failed"));
        }
        self.poisoned = true;
        let part = row.part_no;
        self.push_inner(row, data).inspect_err(|error| {
            if let Error::Invalid(reason) = error {
                note(reason, part, None, None);
            }
        })?;
        self.poisoned = false;
        Ok(())
    }

    fn push_inner(&mut self, row: ConfigRow, data: &[u8]) -> Result<(), Error> {
        let part = row.part_no;
        let refuse = |reason: &str| Error::Part {
            part,
            reason: reason.to_owned(),
        };
        if row.name.is_empty()
            || row.name.len() > 128
            || !row.name.is_ascii()
            || row.name.chars().any(char::is_control)
        {
            note("file_name", part, None, None);
            return Err(refuse(
                "FileName is empty, longer than 128 bytes, not ASCII or has control characters",
            ));
        }
        if row.data_size > self.limits.max_file_bytes {
            note(
                "data_size",
                part,
                n(self.limits.max_file_bytes),
                n(row.data_size),
            );
            return Err(refuse("DataSize is above the limit for one CONFIG file"));
        }
        if u64::try_from(data.len()).ok() != Some(row.stored_size) {
            note(
                "stored_size",
                part,
                n(row.stored_size),
                n(data.len() as u64),
            );
            return Err(refuse("the stored size does not match the data received"));
        }
        let creation = config_cf::timestamp(&row.creation, self.year_offset).map_err(|_| {
            note("creation", part, None, None);
            refuse("Creation is not a valid timestamp")
        })?;
        let modified = config_cf::timestamp(&row.modified, self.year_offset).map_err(|_| {
            note("modified", part, None, None);
            refuse("Modified is not a valid timestamp")
        })?;
        let new_file = self
            .pending
            .as_ref()
            .is_none_or(|item| item.row.name != row.name);
        if new_file {
            if let Some(item) = &self.pending
                && item.row.name >= row.name
            {
                return Err(Error::Invalid("files are not ordered by name"));
            }
            self.complete_pending()?;
            if self.rows.len() >= self.limits.max_files {
                return Err(Error::Invalid("too many files in CONFIG"));
            }
            if row.part_no != 0 {
                note("part_no", part, Some(0), Some(row.part_no));
                return Err(refuse("the file does not start with part 0"));
            }
            if row.attributes != 0 {
                note("attributes", part, Some(0), Some(row.attributes));
                return Err(refuse("Attributes is not 0"));
            }
            if !self.names.insert(row.name.to_ascii_lowercase()) {
                return Err(Error::Invalid(
                    "two files have the same name (compared without regard to case)",
                ));
            }
            self.pending = Some(Pending {
                row: row.clone(),
                creation,
                modified,
                offset: self.total,
                bytes: 0,
                parts: 0,
            });
        }
        let item = self
            .pending
            .as_mut()
            .ok_or(Error::Invalid("no current file"))?;
        if item.parts >= self.limits.max_parts {
            note(
                "parts",
                part,
                n(self.limits.max_parts as u64),
                n(item.parts as u64 + 1),
            );
            return Err(refuse(&format!(
                "too many parts for one CONFIG file (the limit is {})",
                self.limits.max_parts
            )));
        }
        let expected = i64::try_from(item.parts).map_err(|_| Error::Invalid("part overflow"))?;
        if part > expected {
            note("part_no", part, Some(expected), Some(part));
            return Err(refuse(&format!("part {expected} is missing")));
        }
        if expected.checked_sub(1) == Some(part) {
            note("part_no", part, Some(expected), Some(part));
            return Err(refuse("PartNo repeats"));
        }
        if part < expected {
            note("part_no", part, Some(expected), Some(part));
            return Err(refuse("PartNo is lower than the part before it"));
        }
        if creation != item.creation {
            note("creation", part, n(item.creation.0), n(creation.0));
            return Err(refuse("Creation differs from part 0"));
        }
        if row.attributes != item.row.attributes {
            note(
                "attributes",
                part,
                Some(item.row.attributes),
                Some(row.attributes),
            );
            return Err(refuse("Attributes differs from part 0"));
        }
        if row.data_size != item.row.data_size {
            note("data_size", part, n(item.row.data_size), n(row.data_size));
            return Err(refuse("DataSize differs from part 0"));
        }
        if modified < item.modified {
            note("modified", part, n(item.modified.0), n(modified.0));
            return Err(refuse(&format!(
                "Modified is earlier than part {}",
                part.saturating_sub(1)
            )));
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
        if bytes > item.row.data_size {
            note("data_size", part, n(item.row.data_size), n(bytes));
            return Err(refuse(&format!(
                "the parts add up to more than DataSize ({})",
                item.row.data_size
            )));
        }
        if total > self.limits.max_total_bytes {
            note(
                "total_bytes",
                part,
                n(self.limits.max_total_bytes),
                n(total),
            );
            return Err(refuse(&format!(
                "the staged CONFIG would be larger than the limit of {} bytes",
                self.limits.max_total_bytes
            )));
        }
        self.file.write_all(data)?;
        item.bytes = bytes;
        item.parts += 1;
        item.modified = modified;
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
        bak_trace::Trace::current().inspect(|t| t.config_done());
        Ok(Spool {
            file: self.file,
            year_offset: self.year_offset,
            rows: self.rows,
            offsets: self.offsets,
            entry_parts: self.entry_parts,
            part_count: self.part_count,
        })
    }
}

struct AppliedData<'a> {
    spool: &'a mut Spool,
    data: Vec<usize>,
}

impl v8cont::DataSource for AppliedData<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let staged = *self
            .data
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch("CONFIG entry index".into()))?;
        v8cont::DataSource::write_data(&mut *self.spool, staged, out)
    }
}

impl Spool {
    pub fn write_cf_stored(&mut self, out: &mut dyn Write) -> Result<(), Error> {
        self.write_cf_with(
            &CfOptions {
                view: CfView::Stored,
            },
            out,
        )
        .map(|_| ())
    }

    pub fn write_cf_with(
        &mut self,
        options: &CfOptions,
        out: &mut dyn Write,
    ) -> Result<Option<Summary>, Error> {
        self.refuse_storage_references()?;
        let applied = match options.view {
            CfView::Stored => None,
            CfView::Applied => self.plan_applied_view()?,
        };
        let Some(applied) = applied else {
            let metadata = self.rows.clone();
            config_cf::write_config(&metadata, self.year_offset, self, out)?;
            return Ok(None);
        };
        let (rows, data) = self.applied_rows(&applied)?;
        let year_offset = self.year_offset;
        config_cf::write_config(
            &rows,
            year_offset,
            &mut AppliedData { spool: self, data },
            out,
        )?;
        Ok(Some(applied.summary))
    }

    fn refuse_storage_references(&mut self) -> Result<(), Error> {
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
        Ok(())
    }

    pub fn plan_applied_view(&mut self) -> Result<Option<dynamic_update::Applied>, Error> {
        if !self
            .rows
            .iter()
            .any(|row| row.name == dynamic_update::MARKER_NAME)
        {
            return Ok(None);
        }
        let Spool {
            file,
            rows,
            offsets,
            entry_parts,
            ..
        } = self;
        let rows: &[ConfigRow] = rows;
        let entries: Vec<dynamic_update::Entry<'_>> = rows
            .iter()
            .zip(entry_parts.iter())
            .map(|(row, &parts)| dynamic_update::Entry {
                name: &row.name,
                parts,
                stored_size: row.stored_size,
            })
            .collect();
        let mut read = |index: usize| -> io::Result<Vec<u8>> {
            let (Some(&offset), Some(row)) = (offsets.get(index), rows.get(index)) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "entry index"));
            };
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = Vec::new();
            (&mut *file).take(row.stored_size).read_to_end(&mut bytes)?;
            if u64::try_from(bytes.len()).ok() != Some(row.stored_size) {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            Ok(bytes)
        };
        Ok(dynamic_update::plan(&entries, &mut read)?)
    }

    fn applied_rows(
        &self,
        applied: &dynamic_update::Applied,
    ) -> Result<(Vec<ConfigRow>, Vec<usize>), Error> {
        let mut rows = Vec::with_capacity(applied.entries.len());
        let mut data = Vec::with_capacity(applied.entries.len());
        for placed in &applied.entries {
            let (Some(meta), Some(source)) =
                (self.rows.get(placed.meta), self.rows.get(placed.data))
            else {
                return Err(Error::Invalid("entry index"));
            };
            let mut row = meta.clone();
            if let Some(name) = &placed.name {
                row.name.clone_from(name);
            }
            row.data_size = source.stored_size;
            row.stored_size = source.stored_size;
            rows.push(row);
            data.push(placed.data);
        }
        Ok((rows, data))
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
