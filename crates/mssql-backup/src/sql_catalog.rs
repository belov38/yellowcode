use std::collections::{HashMap, HashSet};
use std::io::Read;

use ibschema::columns::{ColType, Column};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SqlColumn {
    #[serde(rename = "column_name")]
    pub name: String,
    pub sql_type: String,
    pub max_length: i16,
    pub precision: u8,
    pub scale: u8,
    #[serde(rename = "is_nullable")]
    pub nullable: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported SQL table catalog: {0}")]
pub struct Error(&'static str);

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_json_bytes: usize,
    pub max_columns: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_json_bytes: 4 * 1024 * 1024,
            max_columns: 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("SQL catalog output I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQL catalog output exceeds a limit or has an invalid shape")]
    Invalid,
    #[error("invalid SQL catalog output: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn read_catalog(input: impl Read, limits: Limits) -> Result<Vec<SqlColumn>, ReadError> {
    let cap = u64::try_from(
        limits
            .max_json_bytes
            .checked_add(1)
            .ok_or(ReadError::Invalid)?,
    )
    .map_err(|_| ReadError::Invalid)?;
    let mut bytes = Vec::new();
    input.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_json_bytes {
        return Err(ReadError::Invalid);
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    let columns: Vec<SqlColumn> = serde_json::from_slice(&bytes)?;
    if columns.is_empty() || columns.len() > limits.max_columns {
        return Err(ReadError::Invalid);
    }
    Ok(columns)
}

fn name(s: &str) -> Result<String, Error> {
    if s.is_empty() || s.len() > 128 || !s.is_ascii() || s.chars().any(char::is_control) {
        return Err(Error("invalid or non-ASCII column name"));
    }
    Ok(s.to_ascii_lowercase())
}

fn matches(column: &Column, sql: &SqlColumn, legacy: bool) -> bool {
    if column.nullable != sql.nullable {
        return false;
    }
    let ty = sql.sql_type.to_ascii_lowercase();
    let length = u32::try_from(sql.max_length).ok();
    match column.ty {
        ColType::L => ty == "binary" && length == Some(1),
        ColType::B(n) => ty == "binary" && length == Some(n),
        ColType::VB(n) => ty == "varbinary" && length == Some(n),
        ColType::I => {
            (ty == "varbinary" && sql.max_length == -1)
                || (legacy && ty == "image" && sql.max_length == 16)
        }
        ColType::NC(n) => {
            ty == "nchar" && length.is_some_and(|size| n.checked_mul(2) == Some(size))
        }
        ColType::NVC(n) => {
            ty == "nvarchar" && length.is_some_and(|size| n.checked_mul(2) == Some(size))
        }
        ColType::NT => {
            (ty == "nvarchar" && sql.max_length == -1)
                || (legacy && ty == "ntext" && sql.max_length == 16)
        }
        ColType::DT => {
            (ty == "datetime2" && sql.scale == 0)
                || (legacy
                    && ty == "datetime"
                    && sql.max_length == 8
                    && sql.precision == 23
                    && sql.scale == 3)
        }
        ColType::N(digits, scale) => {
            (ty == "numeric" && u32::from(sql.precision) == digits && u32::from(sql.scale) == scale)
                || (ty == "int" && digits == 10 && scale == 0 && sql.max_length == 4)
                || (ty == "smallint" && digits == 5 && scale == 0 && sql.max_length == 2)
                || (ty == "bigint" && digits == 20 && scale == 0 && sql.max_length == 8)
        }
        ColType::RV => false,
    }
}

pub fn validate_columns(columns: &[Column], actual: &[SqlColumn]) -> Result<(), Error> {
    validate_profile(columns, actual, false)
}

pub fn validate_legacy_columns(columns: &[Column], actual: &[SqlColumn]) -> Result<(), Error> {
    validate_profile(columns, actual, true)
}

fn validate_profile(columns: &[Column], actual: &[SqlColumn], legacy: bool) -> Result<(), Error> {
    let mut sql = HashMap::new();
    for column in actual {
        if sql.insert(name(&column.name)?, column).is_some() {
            return Err(Error("ambiguous SQL column name"));
        }
    }
    let mut expected = HashSet::new();
    for column in columns {
        let key = name(&column.name)?;
        if !expected.insert(key.clone()) {
            return Err(Error("ambiguous 1C column name"));
        }
        let physical = sql.remove(&key).ok_or(Error("missing SQL column"))?;
        if !matches(column, physical, legacy) {
            return Err(Error("incompatible SQL column"));
        }
    }
    for (key, column) in sql {
        if key != "_version"
            || !column.sql_type.eq_ignore_ascii_case("timestamp")
            || column.max_length != 8
            || column.nullable
        {
            return Err(Error("unexpected SQL column"));
        }
    }
    Ok(())
}
