use crate::config_cf::ConfigRow;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    io::{self, BufReader, Read},
};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_json_bytes: usize,
    pub max_rows: usize,
    pub max_stored_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_json_bytes: 128 * 1024 * 1024,
            max_rows: 100_000,
            max_stored_bytes: 96 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL output I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQL output exceeds the configured limit")]
    Limit,
    #[error("invalid SQL configuration output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid SQL configuration parts: {0}")]
    InvalidParts(&'static str),
}

pub fn read_year_offset(input: impl Read) -> Result<i32, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Offset {
        rows: u64,
        year_offset: Option<i32>,
    }
    let mut bytes = Vec::new();
    input.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::Limit);
    }
    let value: Offset = serde_json::from_slice(&bytes)?;
    let offset = value
        .year_offset
        .ok_or(Error::InvalidParts("missing year offset"))?;
    if value.rows != 1 || offset < 0 {
        return Err(Error::InvalidParts("invalid year offset"));
    }
    Ok(offset)
}

pub struct SqlConfig {
    pub year_offset: i32,
    pub rows: Vec<ConfigRow>,
    pub payloads: Vec<Vec<u8>>,
}

pub fn normalize_multipart(config: SqlConfig, limits: Limits) -> Result<SqlConfig, Error> {
    const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_PARTS: i64 = 1024;
    struct Group {
        row: ConfigRow,
        parts: BTreeMap<i64, Vec<u8>>,
    }
    if config.rows.is_empty()
        || config.rows.len() != config.payloads.len()
        || config.rows.len() > limits.max_rows
    {
        return Err(Error::InvalidParts("row and payload counts"));
    }
    let mut names = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut total = 0_u64;
    for (row, bytes) in config.rows.into_iter().zip(config.payloads) {
        if row.name.is_empty()
            || !row.name.is_ascii()
            || row.name.chars().any(char::is_control)
            || row.attributes != 0
            || !(0..MAX_PARTS).contains(&row.part_no)
        {
            return Err(Error::InvalidParts(
                "unsupported name, attributes or part number",
            ));
        }
        let stored = u64::try_from(bytes.len()).map_err(|_| Error::Limit)?;
        if row.stored_size != stored || row.data_size > MAX_FILE_BYTES {
            return Err(Error::InvalidParts("stored or total size"));
        }
        total = total.checked_add(stored).ok_or(Error::Limit)?;
        if total > limits.max_stored_bytes {
            return Err(Error::Limit);
        }
        let key = row.name.to_ascii_lowercase();
        let part = row.part_no;
        if let Some(&index) = names.get(&key) {
            let group: &mut Group = groups
                .get_mut(index)
                .ok_or(Error::InvalidParts("group index"))?;
            if group.row.name != row.name
                || group.row.creation != row.creation
                || group.row.modified != row.modified
                || group.row.attributes != row.attributes
                || group.row.data_size != row.data_size
            {
                return Err(Error::InvalidParts("inconsistent multipart metadata"));
            }
            if group.parts.insert(part, bytes).is_some() {
                return Err(Error::InvalidParts("duplicate part"));
            }
        } else {
            names.insert(key, groups.len());
            let mut parts = BTreeMap::new();
            parts.insert(part, bytes);
            groups.push(Group { row, parts });
        }
    }
    let mut rows = Vec::with_capacity(groups.len());
    let mut payloads = Vec::with_capacity(groups.len());
    for Group { mut row, parts } in groups {
        let size = usize::try_from(row.data_size).map_err(|_| Error::Limit)?;
        let mut data = Vec::new();
        data.try_reserve_exact(size).map_err(|_| Error::Limit)?;
        for (expected, (number, bytes)) in parts.into_iter().enumerate() {
            if i64::try_from(expected).ok() != Some(number)
                || data.len().checked_add(bytes.len()).is_none_or(|n| n > size)
            {
                return Err(Error::InvalidParts("missing part or excessive data"));
            }
            data.extend_from_slice(&bytes);
        }
        if data.len() != size {
            return Err(Error::InvalidParts("incomplete data"));
        }
        row.part_no = 0;
        row.stored_size = row.data_size;
        rows.push(row);
        payloads.push(data);
    }
    Ok(SqlConfig {
        year_offset: config.year_offset,
        rows,
        payloads,
    })
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

struct Rows {
    rows: Vec<ConfigRow>,
    payloads: Vec<Vec<u8>>,
}
struct RowsSeed(Limits);
impl<'de> DeserializeSeed<'de> for RowsSeed {
    type Value = Rows;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct RowsVisitor(Limits);
        impl<'de> Visitor<'de> for RowsVisitor {
            type Value = Rows;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded array of SQL CONFIG rows")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Rows, A::Error> {
                let mut rows = Vec::new();
                let mut payloads = Vec::new();
                let mut total = 0_u64;
                while let Some(row) = sequence.next_element::<WireRow>()? {
                    if rows.len() >= self.0.max_rows {
                        return Err(de::Error::custom("SQL row limit exceeded"));
                    }
                    if row.name.len() > 4096 || row.creation.len() > 27 || row.modified.len() > 27 {
                        return Err(de::Error::custom("SQL metadata field limit exceeded"));
                    }
                    total = total
                        .checked_add(row.stored_size)
                        .ok_or_else(|| de::Error::custom("SQL payload size overflow"))?;
                    if total > self.0.max_stored_bytes {
                        return Err(de::Error::custom("SQL payload limit exceeded"));
                    }
                    let expected_encoded = row
                        .stored_size
                        .checked_add(2)
                        .and_then(|n| n.checked_div(3))
                        .and_then(|n| n.checked_mul(4))
                        .ok_or_else(|| de::Error::custom("SQL payload size overflow"))?;
                    if u64::try_from(row.data.len()).ok() != Some(expected_encoded) {
                        return Err(de::Error::custom("SQL payload length mismatch"));
                    }
                    let payload = STANDARD
                        .decode(row.data.as_bytes())
                        .map_err(|_| de::Error::custom("invalid SQL base64 data"))?;
                    if u64::try_from(payload.len()).ok() != Some(row.stored_size) {
                        return Err(de::Error::custom("SQL stored size mismatch"));
                    }
                    rows.push(ConfigRow {
                        name: row.name,
                        creation: row.creation,
                        modified: row.modified,
                        attributes: row.attributes,
                        part_no: row.part_no,
                        data_size: row.data_size,
                        stored_size: row.stored_size,
                    });
                    payloads.push(payload);
                }
                Ok(Rows { rows, payloads })
            }
        }
        deserializer.deserialize_seq(RowsVisitor(self.0))
    }
}

struct ExportSeed(Limits);
impl<'de> DeserializeSeed<'de> for ExportSeed {
    type Value = SqlConfig;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct ExportVisitor(Limits);
        impl<'de> Visitor<'de> for ExportVisitor {
            type Value = SqlConfig;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a SQL CONFIG export")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<SqlConfig, A::Error> {
                let mut offset = None;
                let mut rows = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "year_offset" if offset.is_none() => offset = Some(map.next_value()?),
                        "rows" if rows.is_none() => {
                            rows = Some(map.next_value_seed(RowsSeed(self.0))?)
                        }
                        "year_offset" | "rows" => {
                            return Err(de::Error::custom("duplicate SQL output field"));
                        }
                        _ => return Err(de::Error::custom("unknown SQL output field")),
                    }
                }
                let offset = offset.ok_or_else(|| de::Error::missing_field("year_offset"))?;
                let rows: Rows = rows.ok_or_else(|| de::Error::missing_field("rows"))?;
                Ok(SqlConfig {
                    year_offset: offset,
                    rows: rows.rows,
                    payloads: rows.payloads,
                })
            }
        }
        deserializer.deserialize_map(ExportVisitor(self.0))
    }
}

struct SqlcmdReader<R> {
    input: BufReader<R>,
    max_raw_bytes: usize,
    raw_bytes: usize,
    over_limit: bool,
    input_error: Option<io::Error>,
}

impl<R: Read> Read for SqlcmdReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let Some(slot) = output.first_mut() else {
            return Ok(0);
        };
        loop {
            let mut byte = [0_u8];
            match self.input.read(&mut byte) {
                Ok(0) => return Ok(0),
                Ok(_) => {}
                Err(error) => {
                    let reported = io::Error::new(error.kind(), error.to_string());
                    self.input_error = Some(error);
                    return Err(reported);
                }
            }
            if self.raw_bytes >= self.max_raw_bytes {
                self.over_limit = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SQL output limit",
                ));
            }
            self.raw_bytes += 1;
            let [value] = byte;
            if !matches!(value, b'\r' | b'\n') {
                *slot = value;
                return Ok(1);
            }
        }
    }
}

pub fn read_sqlcmd_config(input: impl Read, limits: Limits) -> Result<SqlConfig, Error> {
    let cap = limits.max_json_bytes.checked_add(1).ok_or(Error::Limit)?;
    let cap = u64::try_from(cap).map_err(|_| Error::Limit)?;
    let mut reader = SqlcmdReader {
        input: BufReader::new(input.take(cap)),
        max_raw_bytes: limits.max_json_bytes,
        raw_bytes: 0,
        over_limit: false,
        input_error: None,
    };
    let result = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        ExportSeed(limits)
            .deserialize(&mut deserializer)
            .and_then(|result| {
                deserializer.end()?;
                Ok(result)
            })
    };
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if error.is_io() && reader.over_limit {
                Err(Error::Limit)
            } else if error.is_io() {
                match reader.input_error {
                    Some(input_error) => Err(Error::Io(input_error)),
                    None => Err(Error::Json(error)),
                }
            } else {
                Err(Error::Json(error))
            }
        }
    }
}
