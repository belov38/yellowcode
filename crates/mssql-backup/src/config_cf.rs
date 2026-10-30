use std::{collections::HashSet, io::Write};
use v8cont::{DataSource, EntrySpec, Format, V8Time, WriteOptions};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SQL configuration: {0}")]
    Invalid(&'static str),
    #[error("unsupported SQL configuration: {0}")]
    Unsupported(&'static str),
    #[error("configuration container: {0}")]
    Container(#[from] v8cont::Error),
}

#[derive(Debug, Clone)]
pub struct ConfigRow {
    pub name: String,
    pub creation: String,
    pub modified: String,
    pub attributes: i64,
    pub part_no: i64,
    pub data_size: u64,
    pub stored_size: u64,
}

fn number(bytes: &[u8], start: usize, end: usize) -> Result<u32, Error> {
    let digits = bytes
        .get(start..end)
        .ok_or(Error::Invalid("truncated timestamp"))?;
    digits.iter().try_fold(0_u32, |value, &digit| {
        if !digit.is_ascii_digit() {
            return Err(Error::Invalid("invalid timestamp digit"));
        }
        Ok(value * 10 + u32::from(digit - b'0'))
    })
}

pub(crate) fn timestamp(value: &str, year_offset: i32) -> Result<V8Time, Error> {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(Error::Invalid("timestamp must be an ISO civil date"));
    }
    let year = i32::try_from(number(bytes, 0, 4)?)
        .map_err(|_| Error::Invalid("invalid timestamp year"))?
        .checked_sub(year_offset)
        .ok_or(Error::Invalid("timestamp year overflow"))?;
    let base = V8Time::from_civil(
        year,
        number(bytes, 5, 7)?,
        number(bytes, 8, 10)?,
        number(bytes, 11, 13)?,
        number(bytes, 14, 16)?,
        number(bytes, 17, 19)?,
    )
    .ok_or(Error::Invalid("invalid timestamp date"))?;
    let fraction = if bytes.len() == 19 {
        0
    } else {
        if bytes.get(19) != Some(&b'.') || !(21..=27).contains(&bytes.len()) {
            return Err(Error::Invalid("invalid timestamp fraction"));
        }
        let digits = bytes
            .get(20..)
            .ok_or(Error::Invalid("invalid timestamp fraction"))?;
        let mut ticks = 0_u32;
        for (index, &digit) in digits.iter().enumerate() {
            if !digit.is_ascii_digit() {
                return Err(Error::Invalid("invalid timestamp fraction"));
            }
            if index < 4 {
                ticks = ticks * 10 + u32::from(digit - b'0');
            } else if digit != b'0' {
                return Err(Error::Unsupported("timestamp finer than 100 microseconds"));
            }
        }
        for _ in digits.len()..4 {
            ticks *= 10;
        }
        ticks
    };
    Ok(V8Time(
        base.0
            .checked_add(u64::from(fraction))
            .ok_or(Error::Invalid("timestamp overflow"))?,
    ))
}

struct SortedData<'a> {
    source: &'a mut dyn DataSource,
    indices: &'a [usize],
}

impl DataSource for SortedData<'_> {
    fn write_data(&mut self, index: usize, out: &mut dyn Write) -> v8cont::Result<()> {
        let original = self
            .indices
            .get(index)
            .ok_or_else(|| v8cont::Error::LayoutMismatch("SQL CONFIG entry index".into()))?;
        self.source.write_data(*original, out)
    }
}

pub fn write_config(
    rows: &[ConfigRow],
    year_offset: i32,
    data: &mut dyn DataSource,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if rows.is_empty() {
        return Err(Error::Invalid("CONFIG is empty"));
    }
    if !rows.iter().any(|row| row.name.eq_ignore_ascii_case("root")) {
        return Err(Error::Invalid("CONFIG has no root entry"));
    }
    if year_offset < 0 {
        return Err(Error::Unsupported("negative year offset"));
    }
    let mut names = HashSet::new();
    let mut entries = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        if row.name.is_empty() || !row.name.is_ascii() || row.name.as_bytes().contains(&0) {
            return Err(Error::Unsupported(
                "empty, non-ASCII, or NUL-containing entry name",
            ));
        }
        if !names.insert(row.name.to_ascii_lowercase()) {
            return Err(Error::Invalid("duplicate entry name"));
        }
        if row.part_no != 0 || row.attributes != 0 || row.data_size != row.stored_size {
            return Err(Error::Unsupported(
                "multipart, attributed, or size-mismatched CONFIG entry",
            ));
        }
        entries.push((
            index,
            EntrySpec {
                name: row.name.clone(),
                ctime: timestamp(&row.creation, year_offset)?,
                mtime: timestamp(&row.modified, year_offset)?,
                attrs: 0,
                data_len: Some(row.stored_size),
            },
        ));
    }
    entries.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    let (indices, specs): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
    let mut sorted = SortedData {
        source: data,
        indices: &indices,
    };
    v8cont::write_container(
        out,
        &specs,
        &mut sorted,
        &WriteOptions::top_level(Format::F16),
    )?;
    Ok(())
}
