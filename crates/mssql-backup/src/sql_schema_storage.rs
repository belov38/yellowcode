use std::{collections::HashMap, io::Read};

use dtfile::{Source, TableHead};
use ibschema::{logical::LogicalSchema, values::ColumnValue};
use onecd::Numeric;

use crate::sql_catalog::SqlColumn;

pub const EMPTY_NEWGEN: &[u8] = b"\xef\xbb\xbf{0,\r\n{0}\r\n}";

pub const EXPECTED_STATUS: i64 = 100;

pub const EXTENSION_SCHEMA_ID: u32 = 1;

const REQUIRED_COLUMNS: [&str; 5] = [
    "SCHEMAID",
    "STATUS",
    "CURRENTSCHEMA",
    "NEWGENCREATED",
    "NEWGENDROPPED",
];

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_json_bytes: usize,
    pub max_schema_bytes: usize,
    pub max_rows: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_json_bytes: 64 * 1024 * 1024,
            max_schema_bytes: 16 * 1024 * 1024,
            max_rows: 64,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL schema storage I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SQL schema storage: {0}")]
    Invalid(&'static str),
    #[error("invalid SQL schema storage JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid SQL schema storage bytes: {0}")]
    Hex(#[from] crate::sql_json_row::Error),
    #[error("invalid extension logical schema: {0}")]
    Schema(#[from] ibschema::Error),
    #[error("unsupported SQL schema storage catalog: {0}")]
    Catalog(&'static str),
    #[error("unsupported extension storage: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    pub schema_id: i64,
    pub status: i64,
    pub current_schema: Vec<u8>,
    pub new_gen_created: Vec<u8>,
    pub new_gen_dropped: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStorage {
    pub schema_id: u32,
    pub schema: LogicalSchema,
    pub rows: Vec<Vec<ColumnValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStorageState {
    MainOnly,
    WithExtension(ExtensionStorage),
}

fn fold_name(s: &str) -> Result<String, Error> {
    if s.is_empty() || s.len() > 128 || !s.is_ascii() || s.chars().any(char::is_control) {
        return Err(Error::Catalog("invalid or non-ASCII column name"));
    }
    Ok(s.to_ascii_lowercase())
}

fn is_int_or_n10(column: &SqlColumn) -> bool {
    if column.nullable {
        return false;
    }
    let ty = column.sql_type.to_ascii_lowercase();
    (ty == "int" && column.max_length == 4)
        || (ty == "numeric" && column.precision == 10 && column.scale == 0)
}

fn is_binary_lob(column: &SqlColumn) -> bool {
    let ty = column.sql_type.to_ascii_lowercase();
    (ty == "varbinary" && column.max_length == -1) || (ty == "image" && column.max_length == 16)
}

pub fn validate_catalog(actual: &[SqlColumn]) -> Result<Vec<SqlColumn>, Error> {
    let head = TableHead::system(&Source::SchemaStorage)
        .ok_or(Error::Catalog("missing SchemaStorage layout"))?;
    if head.columns.len() != REQUIRED_COLUMNS.len() {
        return Err(Error::Catalog("unexpected SchemaStorage layout"));
    }
    let mut by_name = HashMap::new();
    for column in actual {
        if by_name.insert(fold_name(&column.name)?, column).is_some() {
            return Err(Error::Catalog("ambiguous SQL column name"));
        }
    }
    let mut ordered = Vec::with_capacity(REQUIRED_COLUMNS.len());
    for expected in REQUIRED_COLUMNS {
        let key = expected.to_ascii_lowercase();
        let physical = by_name
            .remove(&key)
            .ok_or(Error::Catalog("missing SQL column"))?;
        let ok = match expected {
            "SCHEMAID" | "STATUS" => is_int_or_n10(physical),
            _ => is_binary_lob(physical),
        };
        if !ok {
            return Err(Error::Catalog("incompatible SQL column"));
        }
        ordered.push(physical.clone());
    }
    if !by_name.is_empty() {
        return Err(Error::Catalog("unexpected SQL column"));
    }
    Ok(ordered)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRow {
    #[serde(rename = "SCHEMAID")]
    schema_id: String,
    #[serde(rename = "STATUS")]
    status: String,
    #[serde(rename = "CURRENTSCHEMA")]
    current_schema: String,
    #[serde(rename = "NEWGENCREATED")]
    new_gen_created: String,
    #[serde(rename = "NEWGENDROPPED")]
    new_gen_dropped: String,
}

fn parse_i64(text: &str) -> Result<i64, Error> {
    text.parse::<i64>()
        .map_err(|_| Error::Invalid("invalid integer value"))
}

fn decode_hex_bounded(text: &str, max_bytes: usize) -> Result<Vec<u8>, Error> {
    if text
        .strip_prefix("0x")
        .is_some_and(|hex| hex.len() / 2 > max_bytes)
    {
        return Err(Error::Invalid("binary value exceeds limit"));
    }
    Ok(crate::sql_json_row::binary(text)?)
}

pub fn read_schema_storage(input: impl Read, limits: Limits) -> Result<Vec<RawRow>, Error> {
    let cap = u64::try_from(
        limits
            .max_json_bytes
            .checked_add(1)
            .ok_or(Error::Invalid("size overflow"))?,
    )
    .map_err(|_| Error::Invalid("size overflow"))?;
    let mut bytes = Vec::new();
    input.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_json_bytes {
        return Err(Error::Invalid("size limit exceeded"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    let wire_rows: Vec<WireRow> = serde_json::from_slice(&bytes)?;
    if wire_rows.len() > limits.max_rows {
        return Err(Error::Invalid("row count exceeds limit"));
    }
    let mut rows = Vec::with_capacity(wire_rows.len());
    for wire in wire_rows {
        rows.push(RawRow {
            schema_id: parse_i64(&wire.schema_id)?,
            status: parse_i64(&wire.status)?,
            current_schema: decode_hex_bounded(&wire.current_schema, limits.max_schema_bytes)?,
            new_gen_created: decode_hex_bounded(&wire.new_gen_created, limits.max_schema_bytes)?,
            new_gen_dropped: decode_hex_bounded(&wire.new_gen_dropped, limits.max_schema_bytes)?,
        });
    }
    Ok(rows)
}

pub fn validate_schema_storage(
    mut rows: Vec<RawRow>,
    main_dbschema_bytes: &[u8],
) -> Result<SchemaStorageState, Error> {
    if rows.is_empty() {
        return Err(Error::Unsupported("SchemaStorage has no rows".to_owned()));
    }
    rows.sort_by_key(|row| row.schema_id);
    for pair in rows.windows(2) {
        if let [first, second] = pair
            && first.schema_id == second.schema_id
        {
            return Err(Error::Unsupported(
                "SchemaStorage has duplicate SchemaID".to_owned(),
            ));
        }
    }
    let bad_ids: Vec<String> = rows
        .iter()
        .filter(|row| row.schema_id != 0 && row.schema_id != i64::from(EXTENSION_SCHEMA_ID))
        .map(|row| row.schema_id.to_string())
        .collect();
    if !bad_ids.is_empty() {
        return Err(Error::Unsupported(format!(
            "SchemaStorage has unsupported SchemaID {}; only SchemaID 0 and 1 are supported",
            bad_ids.join(", ")
        )));
    }
    let has_main = rows.iter().any(|row| row.schema_id == 0);
    if !has_main {
        return Err(Error::Unsupported(
            "SchemaStorage has no main row SchemaID 0".to_owned(),
        ));
    }
    for row in &rows {
        if row.status != EXPECTED_STATUS {
            return Err(Error::Unsupported(format!(
                "SchemaStorage SchemaID {} has Status other than 100",
                row.schema_id
            )));
        }
        if row.new_gen_created != EMPTY_NEWGEN || row.new_gen_dropped != EMPTY_NEWGEN {
            return Err(Error::Unsupported(format!(
                "SchemaStorage SchemaID {} has non-empty NewGenCreated or NewGenDropped",
                row.schema_id
            )));
        }
        if row.schema_id == 0 && row.current_schema != main_dbschema_bytes {
            return Err(Error::Unsupported(
                "SchemaStorage SchemaID 0 CurrentSchema differs from DBSchema".to_owned(),
            ));
        }
    }
    if rows.len() == 1 {
        return Ok(SchemaStorageState::MainOnly);
    }
    let ext_row = rows
        .iter()
        .find(|row| row.schema_id == i64::from(EXTENSION_SCHEMA_ID))
        .ok_or_else(|| {
            Error::Unsupported("SchemaStorage has no extension row SchemaID 1".to_owned())
        })?;
    let schema = dtfile::reader::extension_schema(&ext_row.current_schema)?;
    let dt_rows = rows
        .into_iter()
        .map(|row| {
            vec![
                ColumnValue::Number(Numeric::from_i64(row.schema_id)),
                ColumnValue::Number(Numeric::from_i64(row.status)),
                ColumnValue::Bytes(row.current_schema),
                ColumnValue::Bytes(row.new_gen_created),
                ColumnValue::Bytes(row.new_gen_dropped),
            ]
        })
        .collect();
    Ok(SchemaStorageState::WithExtension(ExtensionStorage {
        schema_id: EXTENSION_SCHEMA_ID,
        schema,
        rows: dt_rows,
    }))
}
