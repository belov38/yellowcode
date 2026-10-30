use std::io::Read;

use ibschema::logical::LogicalSchema;

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_json_bytes: usize,
    pub max_schema_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_json_bytes: 32 * 1024 * 1024,
            max_schema_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL schema output I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQL schema output exceeds a limit or has an invalid shape")]
    Invalid,
    #[error("invalid SQL schema output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid SQL schema bytes: {0}")]
    Hex(#[from] crate::sql_json_row::Error),
    #[error("invalid logical schema: {0}")]
    Schema(#[from] ibschema::Error),
}

pub struct SnapshotSchema {
    pub schema: LogicalSchema,
    pub year_offset: i32,
}

pub fn read_schema_with_legacy_indexes(
    input: impl Read,
    limits: Limits,
) -> Result<SnapshotSchema, Error> {
    let (schema, year_offset) = read_wire(input, limits)?;
    Ok(SnapshotSchema {
        schema: LogicalSchema::from_text_legacy_indexes(&schema)?,
        year_offset,
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: String,
    year_offset: i32,
}

pub fn read_schema(input: impl Read, limits: Limits) -> Result<SnapshotSchema, Error> {
    let (schema, year_offset) = read_wire(input, limits)?;
    Ok(SnapshotSchema {
        schema: LogicalSchema::from_text(&schema)?,
        year_offset,
    })
}

fn read_wire(input: impl Read, limits: Limits) -> Result<(Vec<u8>, i32), Error> {
    let cap = u64::try_from(limits.max_json_bytes.checked_add(1).ok_or(Error::Invalid)?)
        .map_err(|_| Error::Invalid)?;
    let mut data = Vec::new();
    input.take(cap).read_to_end(&mut data)?;
    if data.len() > limits.max_json_bytes {
        return Err(Error::Invalid);
    }
    data.retain(|&b| b != b'\r' && b != b'\n');
    let mut rows: Vec<Wire> = serde_json::from_slice(&data)?;
    if rows.len() != 1 {
        return Err(Error::Invalid);
    }
    let row = rows.pop().ok_or(Error::Invalid)?;
    if !(0..=9999).contains(&row.year_offset)
        || row.schema.len().saturating_sub(2) / 2 > limits.max_schema_bytes
    {
        return Err(Error::Invalid);
    }
    let schema = crate::sql_json_row::binary(&row.schema)?;
    if schema.len() > limits.max_schema_bytes {
        return Err(Error::Invalid);
    }
    Ok((schema, row.year_offset))
}
