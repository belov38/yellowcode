use ibschema::{
    columns::{ColType, Column},
    values::ColumnValue,
};
use onecd::{DateTime, Numeric};

const MAX_VALUE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DECIMAL_BYTES: usize = 128;

#[derive(Debug)]
pub enum SqlCell {
    Null,
    Binary(Vec<u8>),
    Text(String),
    Decimal(String),
    Date(String),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported SQL column value: {0}")]
    Unsupported(&'static str),
    #[error("SQL value violates the 1C column: {0}")]
    Column(String),
}

pub fn convert_cell(
    column: &Column,
    cell: SqlCell,
    year_offset: i32,
) -> Result<ColumnValue, Error> {
    if year_offset < 0 {
        return Err(Error::Unsupported("negative year offset"));
    }
    let converted = match (column.ty, cell) {
        (_, SqlCell::Null) => ColumnValue::Null,
        (ColType::L, SqlCell::Binary(bytes)) => match bytes.as_slice() {
            [0] => ColumnValue::Bool(false),
            [1] => ColumnValue::Bool(true),
            _ => return Err(Error::Unsupported("boolean is not a one-byte zero or one")),
        },
        (ColType::B(_) | ColType::VB(_) | ColType::I, SqlCell::Binary(bytes)) => {
            if bytes.len() > MAX_VALUE_BYTES {
                return Err(Error::Unsupported("binary value limit exceeded"));
            }
            if column.ty == ColType::I && bytes.len() == 64 && bytes.starts_with(b"STORHDR\0") {
                return Err(Error::Unsupported(
                    "unresolved SQL binary storage reference",
                ));
            }
            ColumnValue::Bytes(bytes)
        }
        (ColType::NC(_) | ColType::NVC(_) | ColType::NT, SqlCell::Text(text)) => {
            if text.len() > MAX_VALUE_BYTES {
                return Err(Error::Unsupported("text value limit exceeded"));
            }
            ColumnValue::Str(text)
        }
        (ColType::N(_, _), SqlCell::Decimal(text)) => {
            if text.len() > MAX_DECIMAL_BYTES {
                return Err(Error::Unsupported("decimal value limit exceeded"));
            }
            ColumnValue::Number(Numeric::parse(&text).ok_or(Error::Unsupported("invalid decimal"))?)
        }
        (ColType::DT, SqlCell::Date(text)) => {
            let time = crate::config_cf::timestamp(&text, year_offset)
                .map_err(|_| Error::Unsupported("invalid SQL date"))?;
            let (year, month, day, hour, minute, second, fraction) = time.to_civil();
            if fraction != 0 {
                return Err(Error::Unsupported("fractional seconds in data column"));
            }
            let year =
                u16::try_from(year).map_err(|_| Error::Unsupported("date year out of range"))?;
            let month =
                u8::try_from(month).map_err(|_| Error::Unsupported("date month out of range"))?;
            let day = u8::try_from(day).map_err(|_| Error::Unsupported("date day out of range"))?;
            let hour =
                u8::try_from(hour).map_err(|_| Error::Unsupported("date hour out of range"))?;
            let minute =
                u8::try_from(minute).map_err(|_| Error::Unsupported("date minute out of range"))?;
            let second =
                u8::try_from(second).map_err(|_| Error::Unsupported("date second out of range"))?;
            ColumnValue::DateTime(
                DateTime::new(year, month, day, hour, minute, second)
                    .ok_or(Error::Unsupported("invalid date"))?,
            )
        }
        _ => return Err(Error::Unsupported("SQL and 1C types do not match")),
    };
    column.check(&converted).map_err(Error::Column)?;
    Ok(converted)
}
