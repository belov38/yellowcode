use crate::columns::{ColRole, ColType, Column, Part};
use crate::fields::{GUID_LE_FIELDS, is_version, ref_number};
use crate::logical::TypeCode;
use crate::storage::{FieldId, FieldInfo, TableInfo};
use crate::values::{ColumnValue, RefId, Uuid16, type_code};
use onecd::{DateTime, Numeric};

#[derive(Clone, Copy, Debug)]
pub enum ColumnView<'a> {
    Null,
    Bool(bool),
    Number(&'a Numeric),
    Str(&'a str),
    DateTime(DateTime),
    Bytes(&'a [u8]),
}
impl<'a> From<&'a ColumnValue> for ColumnView<'a> {
    fn from(value: &'a ColumnValue) -> Self {
        match value {
            ColumnValue::Null => Self::Null,
            ColumnValue::Bool(value) => Self::Bool(*value),
            ColumnValue::Number(value) => Self::Number(value),
            ColumnValue::Str(value) => Self::Str(value),
            ColumnValue::DateTime(value) => Self::DateTime(*value),
            ColumnValue::Bytes(value) => Self::Bytes(value),
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum FieldNumber<'a> {
    Decimal(&'a Numeric),
    Unsigned(u32),
}
#[derive(Clone, Copy, Debug)]
pub enum FieldView<'a> {
    Null,
    Undefined,
    Bool(bool),
    Number(FieldNumber<'a>),
    Date(DateTime),
    String(&'a str),
    Uuid(Uuid16),
    Ref { table: u32, id: RefId },
    Storage(&'a [u8]),
    Binary(&'a [u8]),
    RowVersion([u32; 4]),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldDecodeError {
    NoField,
    RowVersion,
    InvalidShape,
    ValueCount,
    MissingColumn,
    TypeMismatch,
    HalfNullReference,
    InvalidTypeCode,
    UnknownReferenceType,
}
impl std::fmt::Display for FieldDecodeError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::NoField => "no such field",
            Self::RowVersion => "the row version is read by the store",
            Self::InvalidShape => "invalid field column layout",
            Self::ValueCount => "field column count mismatch",
            Self::MissingColumn => "missing field column",
            Self::TypeMismatch => "column value does not match field type",
            Self::HalfNullReference => "half-NULL reference pair",
            Self::InvalidTypeCode => "invalid composite type code",
            Self::UnknownReferenceType => "reference type without a table number",
        })
    }
}
impl std::error::Error for FieldDecodeError {}
type Result<T> = std::result::Result<T, FieldDecodeError>;

fn bytes(value: ColumnView<'_>) -> Result<&[u8]> {
    match value {
        ColumnView::Bytes(bytes) => Ok(bytes),
        _ => Err(FieldDecodeError::TypeMismatch),
    }
}
fn bytes16(value: ColumnView<'_>) -> Result<[u8; 16]> {
    bytes(value)?
        .try_into()
        .map_err(|_| FieldDecodeError::TypeMismatch)
}
fn be4(value: ColumnView<'_>) -> Result<u32> {
    <[u8; 4]>::try_from(bytes(value)?)
        .map(u32::from_be_bytes)
        .map_err(|_| FieldDecodeError::TypeMismatch)
}
fn type_table(field: &FieldInfo) -> Option<u32> {
    field
        .types
        .iter()
        .find(|ty| ty.code == TypeCode::R)
        .and_then(|ty| ref_number(&ty.ref_table))
}

pub fn decode_field_view<'a>(
    table: &TableInfo,
    field: FieldId,
    values: &[ColumnView<'a>],
) -> Result<FieldView<'a>> {
    let field = table.field(field).ok_or(FieldDecodeError::NoField)?;
    if is_version(field) {
        return Err(FieldDecodeError::RowVersion);
    }
    let meta = table
        .columns
        .get(field.columns.clone())
        .filter(|columns| !columns.is_empty())
        .ok_or(FieldDecodeError::InvalidShape)?;
    if meta.len() != values.len() {
        return Err(FieldDecodeError::ValueCount);
    }
    match meta.first().map(|column| column.part) {
        Some(Part::TRef) if meta.len() == 2 => pair(values),
        Some(Part::TRef) => Err(FieldDecodeError::InvalidShape),
        Some(Part::Type) => composite(field, meta, values),
        _ if meta.len() == 1 => single(field, meta, values),
        _ => Err(FieldDecodeError::InvalidShape),
    }
}
fn single<'a>(
    field: &FieldInfo,
    meta: &[Column],
    values: &[ColumnView<'a>],
) -> Result<FieldView<'a>> {
    let column = meta.first().ok_or(FieldDecodeError::MissingColumn)?;
    let value = values
        .first()
        .copied()
        .ok_or(FieldDecodeError::MissingColumn)?;
    if matches!(value, ColumnView::Null) {
        return Ok(FieldView::Null);
    }
    match (column.role, value) {
        (ColRole::Ref, value) => {
            let id = RefId(bytes16(value)?);
            Ok(match type_table(field) {
                Some(table) => FieldView::Ref { table, id },
                None => FieldView::Uuid(id.to_uuid()),
            })
        }
        (ColRole::Bool, ColumnView::Bool(value)) => Ok(FieldView::Bool(value)),
        (ColRole::Number, ColumnView::Number(value)) => {
            Ok(FieldView::Number(FieldNumber::Decimal(value)))
        }
        (ColRole::DateTime, ColumnView::DateTime(value)) => Ok(FieldView::Date(value)),
        (ColRole::Str, ColumnView::Str(value)) => Ok(FieldView::String(value)),
        (ColRole::KeyField, value) => Ok(FieldView::Number(FieldNumber::Unsigned(be4(value)?))),
        (ColRole::Binary, ColumnView::Bytes(value)) => Ok(match column.ty {
            ColType::B(16) => {
                let value =
                    <[u8; 16]>::try_from(value).map_err(|_| FieldDecodeError::TypeMismatch)?;
                FieldView::Uuid(if GUID_LE_FIELDS.contains(&field.name.as_str()) {
                    Uuid16::from_bytes_le(value)
                } else {
                    RefId(value).to_uuid()
                })
            }
            ColType::I
                if field.name.strip_prefix("Fld").is_some_and(|digits| {
                    !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit())
                }) =>
            {
                FieldView::Storage(value)
            }
            _ => FieldView::Binary(value),
        }),
        _ => Err(FieldDecodeError::TypeMismatch),
    }
}
fn pair<'a>(values: &[ColumnView<'a>]) -> Result<FieldView<'a>> {
    let table = values
        .first()
        .copied()
        .ok_or(FieldDecodeError::MissingColumn)?;
    let id = values
        .get(1)
        .copied()
        .ok_or(FieldDecodeError::MissingColumn)?;
    match (
        matches!(table, ColumnView::Null),
        matches!(id, ColumnView::Null),
    ) {
        (true, true) => Ok(FieldView::Null),
        (false, false) => Ok(FieldView::Ref {
            table: be4(table)?,
            id: RefId(bytes16(id)?),
        }),
        _ => Err(FieldDecodeError::HalfNullReference),
    }
}
fn composite<'a>(
    field: &FieldInfo,
    meta: &[Column],
    values: &[ColumnView<'a>],
) -> Result<FieldView<'a>> {
    let at = |part| {
        meta.iter()
            .position(|column| column.part == part)
            .and_then(|index| values.get(index))
            .copied()
    };
    let code = match at(Part::Type).ok_or(FieldDecodeError::MissingColumn)? {
        ColumnView::Null => return Ok(FieldView::Null),
        ColumnView::Bytes([code]) => *code,
        _ => return Err(FieldDecodeError::InvalidTypeCode),
    };
    let need = |part| at(part).ok_or(FieldDecodeError::MissingColumn);
    match code {
        type_code::UNDEFINED => Ok(FieldView::Undefined),
        type_code::BOOL => match need(Part::L)? {
            ColumnView::Bool(value) => Ok(FieldView::Bool(value)),
            _ => Err(FieldDecodeError::TypeMismatch),
        },
        type_code::NUMBER => match need(Part::N)? {
            ColumnView::Number(value) => Ok(FieldView::Number(FieldNumber::Decimal(value))),
            _ => Err(FieldDecodeError::TypeMismatch),
        },
        type_code::DATE => match need(Part::T)? {
            ColumnView::DateTime(value) => Ok(FieldView::Date(value)),
            _ => Err(FieldDecodeError::TypeMismatch),
        },
        type_code::STRING => match need(Part::S)? {
            ColumnView::Str(value) => Ok(FieldView::String(value)),
            _ => Err(FieldDecodeError::TypeMismatch),
        },
        type_code::UUID => Ok(FieldView::Uuid(RefId(bytes16(need(Part::B)?)?).to_uuid())),
        type_code::REF => Ok(FieldView::Ref {
            table: match at(Part::RTRef) {
                Some(value) => be4(value)?,
                None => type_table(field).ok_or(FieldDecodeError::UnknownReferenceType)?,
            },
            id: RefId(bytes16(need(Part::RRRef)?)?),
        }),
        _ => Err(FieldDecodeError::InvalidTypeCode),
    }
}
