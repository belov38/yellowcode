pub mod managed;

use crate::error::{Error, Result};
use ibschema::{columns::ColType, values::ColumnValue};
use onecd::{BlobRef, BlobVal, FieldDescr, FieldType, Value};

pub fn col_type_of(f: &FieldDescr) -> Option<ColType> {
    Some(match f.ty {
        FieldType::N => ColType::N(f.length, f.precision),
        FieldType::NC => ColType::NC(f.length),
        FieldType::NVC => ColType::NVC(f.length),
        FieldType::B => ColType::B(f.length),
        FieldType::VB => ColType::VB(f.length),
        FieldType::NT => ColType::NT,
        FieldType::I => ColType::I,
        FieldType::L => ColType::L,
        FieldType::DT => ColType::DT,
        FieldType::RV => ColType::RV,
        FieldType::C | FieldType::VC | FieldType::T | FieldType::D => return None,
    })
}

pub(crate) fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Numeric(_) => "Numeric",
        Value::Char(_) => "Char",
        Value::VarChar(_) => "VarChar",
        Value::Binary(_) => "Binary",
        Value::Text(_) => "Text",
        Value::NText(_) => "NText",
        Value::Image(_) => "Image",
        Value::Bool(_) => "Bool",
        Value::Date(_) => "Date",
        Value::DateTime(_) => "DateTime",
        Value::RowVersion(_) => "RowVersion",
        Value::Invalid(_) => "Invalid",
    }
}

fn column_value_kind(v: &ColumnValue) -> &'static str {
    match v {
        ColumnValue::Null => "Null",
        ColumnValue::Bool(_) => "Bool",
        ColumnValue::Number(_) => "Number",
        ColumnValue::Str(_) => "Str",
        ColumnValue::DateTime(_) => "DateTime",
        ColumnValue::Bytes(_) => "Bytes",
    }
}

fn mismatch(v: &ColumnValue, ty: ColType) -> Error {
    Error::Inconsistent(format!(
        "{} value does not match column type {ty:?}",
        column_value_kind(v)
    ))
}

pub fn to_onecd(v: &ColumnValue, ty: ColType) -> Result<Value> {
    if ty == ColType::RV {
        return Err(Error::Unsupported("row version column value".into()));
    }
    into_onecd(v.clone(), ty)
}

pub fn into_onecd(v: ColumnValue, ty: ColType) -> Result<Value> {
    if ty == ColType::RV {
        return Err(Error::Unsupported("row version column value".into()));
    }
    Ok(match (v, ty) {
        (ColumnValue::Null, _) => Value::Null,
        (ColumnValue::Str(s), ColType::NC(_)) => Value::Char(s),
        (ColumnValue::Str(s), ColType::NVC(_)) => Value::VarChar(s),
        (ColumnValue::Str(s), ColType::NT) => Value::NText(BlobVal::Data(
            s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        )),
        (ColumnValue::Bytes(b), ColType::B(_) | ColType::VB(_)) => Value::Binary(b),
        (ColumnValue::Bytes(b), ColType::I) => Value::Image(BlobVal::Data(b)),
        (ColumnValue::Bool(b), ColType::L) => Value::Bool(b),
        (ColumnValue::Number(n), ColType::N(..)) => Value::Numeric(n),
        (ColumnValue::DateTime(d), ColType::DT) => Value::DateTime(d),
        (v, ty) => return Err(mismatch(&v, ty)),
    })
}

pub const NT_COPY_FACTOR: usize = 6;

fn blob_bytes(
    b: &BlobVal,
    blob: &mut dyn FnMut(BlobRef) -> onecd::Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    match b {
        BlobVal::Data(d) => Ok(d.clone()),
        BlobVal::Ref(r) => Ok(blob(*r)?),
    }
}

fn utf16le(b: &[u8]) -> Result<String> {
    if !b.len().is_multiple_of(2) {
        return Err(Error::Unsupported(
            "odd length of an NT value (lone surrogate in NT value)".into(),
        ));
    }
    let units = b.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes);
    char::decode_utf16(units)
        .collect::<std::result::Result<String, _>>()
        .map_err(|_| Error::Unsupported("lone surrogate in NT value".into()))
}

pub fn from_onecd(
    v: &Value,
    ty: ColType,
    blob: &mut dyn FnMut(BlobRef) -> onecd::Result<Vec<u8>>,
) -> Result<ColumnValue> {
    let bad = || {
        Error::Inconsistent(format!(
            "engine {} value does not match column type {ty:?}",
            value_kind(v)
        ))
    };
    if ty == ColType::RV || matches!(v, Value::RowVersion(_)) {
        return Err(Error::Unsupported("row version column value".into()));
    }
    Ok(match (v, ty) {
        (Value::Null, _) => ColumnValue::Null,
        (Value::Char(s), ColType::NC(_)) => ColumnValue::Str(s.clone()),
        (Value::VarChar(s), ColType::NVC(_)) => ColumnValue::Str(s.clone()),
        (Value::NText(b), ColType::NT) => ColumnValue::Str(utf16le(&blob_bytes(b, blob)?)?),
        (Value::Binary(b), ColType::B(_) | ColType::VB(_)) => ColumnValue::Bytes(b.clone()),
        (Value::Image(b), ColType::I) => ColumnValue::Bytes(blob_bytes(b, blob)?),
        (Value::Bool(b), ColType::L) => ColumnValue::Bool(*b),
        (Value::Numeric(n), ColType::N(..)) => ColumnValue::Number(n.clone()),
        (Value::DateTime(d), ColType::DT) => ColumnValue::DateTime(*d),
        (Value::Invalid(_), _) => {
            return Err(Error::Inconsistent(format!(
                "stored bytes do not decode as {ty:?}"
            )));
        }
        _ => return Err(bad()),
    })
}
