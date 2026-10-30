use std::{collections::HashMap, fmt};

use ibschema::{
    columns::{ColType, Column},
    values::ColumnValue,
};
use serde::{
    Deserialize,
    de::{MapAccess, Visitor},
};

use crate::sql_values::{SqlCell, convert_cell};

const MAX_ROW_BYTES: usize = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SQL row: {0}")]
    Invalid(&'static str),
    #[error("invalid SQL row JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid SQL cell: {0}")]
    Cell(#[from] crate::sql_values::Error),
    #[error("invalid SQL binary storage: {0}")]
    Storage(#[from] crate::sql_blob::Error),
    #[error("SQL binary storage read failed: {0}")]
    StorageRead(String),
}

struct Fields(HashMap<String, Option<String>>);

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = Fields;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique string or null fields")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
                let mut fields = HashMap::new();
                while let Some((key, value)) = map.next_entry::<String, Option<String>>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate field"));
                    }
                }
                Ok(Fields(fields))
            }
        }
        deserializer.deserialize_map(FieldsVisitor)
    }
}

pub(crate) fn binary(text: &str) -> Result<Vec<u8>, Error> {
    let hex = text
        .strip_prefix("0x")
        .ok_or(Error::Invalid("binary prefix"))?
        .as_bytes();
    if hex.len() % 2 != 0 || hex.len() / 2 > MAX_BINARY_BYTES {
        return Err(Error::Invalid("binary length"));
    }
    hex.as_chunks::<2>()
        .0
        .iter()
        .map(|&[high, low]| {
            let digit = |b: u8| {
                (b as char)
                    .to_digit(16)
                    .ok_or(Error::Invalid("binary hexadecimal digit"))
            };
            Ok(((digit(high)? << 4) | digit(low)?) as u8)
        })
        .collect()
}

fn utf16_text(text: &str) -> Result<String, Error> {
    let bytes = binary(text)?;
    let (units, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(Error::Invalid("odd UTF-16 byte length"));
    }
    let units: Vec<u16> = units.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
    String::from_utf16(&units).map_err(|_| Error::Invalid("invalid UTF-16"))
}

pub fn convert_json_row(
    columns: &[Column],
    input: &[u8],
    year_offset: i32,
) -> Result<Vec<ColumnValue>, Error> {
    convert_inner(columns, input, year_offset, None)
}

pub fn convert_json_row_with_resolver(
    columns: &[Column],
    input: &[u8],
    year_offset: i32,
    mut resolver: impl FnMut(
        &crate::sql_blob::StorageRef,
    ) -> Result<Vec<crate::sql_blob::BlobPart>, String>,
) -> Result<Vec<ColumnValue>, Error> {
    convert_inner(columns, input, year_offset, Some(&mut resolver))
}

type Resolver<'a> =
    dyn FnMut(&crate::sql_blob::StorageRef) -> Result<Vec<crate::sql_blob::BlobPart>, String> + 'a;

fn convert_inner(
    columns: &[Column],
    input: &[u8],
    year_offset: i32,
    mut resolver: Option<&mut Resolver<'_>>,
) -> Result<Vec<ColumnValue>, Error> {
    if year_offset < 0 {
        return Err(Error::Invalid("negative year offset"));
    }
    if input.len() > MAX_ROW_BYTES {
        return Err(Error::Invalid("row size limit exceeded"));
    }
    let Fields(mut fields) = serde_json::from_slice(input)?;
    let mut output = Vec::with_capacity(columns.len());
    for column in columns {
        if column.ty == ColType::RV {
            return Err(Error::Invalid("rowversion is not a DT data column"));
        }
        let value = fields
            .remove(&column.name)
            .ok_or(Error::Invalid("missing or duplicate schema column"))?;
        let cell = match value {
            None => SqlCell::Null,
            Some(text) => match column.ty {
                ColType::I => {
                    let data = binary(&text)?;
                    if let Some(resolve) = resolver.as_deref_mut()
                        && let Some(reference) = crate::sql_blob::parse_reference(&data)?
                    {
                        let parts = resolve(&reference).map_err(Error::StorageRead)?;
                        let payload = crate::sql_blob::resolve_single_part(&reference, parts)?;
                        let value = ColumnValue::Bytes(payload);
                        column
                            .check(&value)
                            .map_err(crate::sql_values::Error::Column)?;
                        output.push(value);
                        continue;
                    }
                    SqlCell::Binary(data)
                }
                ColType::L | ColType::B(_) | ColType::VB(_) => SqlCell::Binary(binary(&text)?),
                ColType::NC(_) | ColType::NVC(_) | ColType::NT => SqlCell::Text(utf16_text(&text)?),
                ColType::N(_, _) => SqlCell::Decimal(text),
                ColType::DT => SqlCell::Date(text),
                ColType::RV => return Err(Error::Invalid("rowversion is not a DT data column")),
            },
        };
        output.push(convert_cell(column, cell, year_offset)?);
    }
    if !fields.is_empty() {
        return Err(Error::Invalid("unexpected SQL column"));
    }
    Ok(output)
}
