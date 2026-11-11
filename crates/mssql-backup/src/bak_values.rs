// Derived from bcdb 0.2.1, src/BcDb.Core/Values.cs, src/BcDb.Core/Lob.cs and src/BcDb.Core/Catalog.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use crate::{
    bak_bytes::{u8_at, u32_at, u64_at},
    bak_read::Error,
    bak_record::{Cell, RecordId},
};

const SECONDS_PER_DAY: u64 = 86_400;
const DAYS_TO_1900: i64 = 693_595;
const MAX_DATE_DAYS: u64 = 3_652_058;
const DATETIME_DAYS: std::ops::RangeInclusive<i32> = -53_690..=2_958_463;
const TICKS_PER_DAY: u32 = 25_920_000;
const TICKS_PER_SECOND: u32 = 300;
const TEXT_POINTER_LEN: usize = 16;
const ROOT_HEADER_LEN: usize = 12;
const ROOT_LINK_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlType {
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Bit,
    Real,
    Float,
    Numeric { precision: u8, scale: u8 },
    Money,
    SmallMoney,
    Char,
    VarChar,
    NChar,
    NVarChar,
    Text,
    NText,
    Binary,
    VarBinary,
    Image,
    UniqueIdentifier,
    Date,
    Time(u8),
    SmallDateTime,
    DateTime,
    DateTime2(u8),
    Unsupported(&'static str),
}

impl SqlType {
    pub fn name(&self) -> &'static str {
        match self {
            SqlType::TinyInt => "tinyint",
            SqlType::SmallInt => "smallint",
            SqlType::Int => "int",
            SqlType::BigInt => "bigint",
            SqlType::Bit => "bit",
            SqlType::Real => "real",
            SqlType::Float => "float",
            SqlType::Numeric { .. } => "numeric",
            SqlType::Money => "money",
            SqlType::SmallMoney => "smallmoney",
            SqlType::Char => "char",
            SqlType::VarChar => "varchar",
            SqlType::NChar => "nchar",
            SqlType::NVarChar => "nvarchar",
            SqlType::Text => "text",
            SqlType::NText => "ntext",
            SqlType::Binary => "binary",
            SqlType::VarBinary => "varbinary",
            SqlType::Image => "image",
            SqlType::UniqueIdentifier => "uniqueidentifier",
            SqlType::Date => "date",
            SqlType::Time(_) => "time",
            SqlType::SmallDateTime => "smalldatetime",
            SqlType::DateTime => "datetime",
            SqlType::DateTime2(_) => "datetime2",
            SqlType::Unsupported(name) => name,
        }
    }

    pub fn storage_width(&self, max_len: i16) -> Option<u16> {
        match self {
            SqlType::TinyInt => Some(1),
            SqlType::SmallInt => Some(2),
            SqlType::Int | SqlType::Real | SqlType::SmallMoney | SqlType::SmallDateTime => Some(4),
            SqlType::BigInt | SqlType::Float | SqlType::Money | SqlType::DateTime => Some(8),
            SqlType::UniqueIdentifier => Some(16),
            SqlType::Date => Some(3),
            SqlType::Time(scale) => Some(time_width(*scale)),
            SqlType::DateTime2(scale) => Some(time_width(*scale) + 3),
            SqlType::Numeric { precision, .. } => Some(numeric_width(*precision)),
            SqlType::Char | SqlType::NChar | SqlType::Binary => {
                u16::try_from(max_len).ok().filter(|width| *width > 0)
            }
            SqlType::Bit
            | SqlType::VarChar
            | SqlType::NVarChar
            | SqlType::Text
            | SqlType::NText
            | SqlType::VarBinary
            | SqlType::Image
            | SqlType::Unsupported(_) => None,
        }
    }

    pub fn may_be_off_row(&self) -> bool {
        matches!(
            self,
            SqlType::VarChar
                | SqlType::NVarChar
                | SqlType::VarBinary
                | SqlType::Image
                | SqlType::Text
                | SqlType::NText
        )
    }
}

fn time_width(scale: u8) -> u16 {
    match scale {
        0..=2 => 3,
        3..=4 => 4,
        _ => 5,
    }
}

fn numeric_width(precision: u8) -> u16 {
    match precision {
        0..=9 => 5,
        10..=19 => 9,
        20..=28 => 13,
        _ => 17,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnType {
    pub ty: SqlType,
    pub max_len: i16,
}

pub fn column_type(xtype: u8, ti: u32) -> Result<ColumnType, Error> {
    let b1 = u8::try_from((ti >> 8) & 0xff).unwrap_or(0);
    let b2 = u8::try_from((ti >> 16) & 0xff).unwrap_or(0);
    let length = (ti >> 8) & 0xffff;
    let fixed = |ty: SqlType| ColumnType {
        ty,
        max_len: ty
            .storage_width(0)
            .and_then(|width| i16::try_from(width).ok())
            .unwrap_or(1),
    };
    let sized = |ty: SqlType| -> Result<ColumnType, Error> {
        let max_len = if length == 0 {
            -1
        } else {
            i16::try_from(length).map_err(|_| {
                Error::Inconsistent("the declared length of a column does not fit 16 bits")
            })?
        };
        Ok(ColumnType { ty, max_len })
    };
    let scaled = |make: fn(u8) -> SqlType| -> Result<ColumnType, Error> {
        if b1 > 7 {
            return Err(Error::Inconsistent("a time scale above 7"));
        }
        Ok(fixed(make(b1)))
    };
    let unsupported = |name: &'static str| ColumnType {
        ty: SqlType::Unsupported(name),
        max_len: 0,
    };
    match xtype {
        48 => Ok(fixed(SqlType::TinyInt)),
        52 => Ok(fixed(SqlType::SmallInt)),
        56 => Ok(fixed(SqlType::Int)),
        127 => Ok(fixed(SqlType::BigInt)),
        104 => Ok(ColumnType {
            ty: SqlType::Bit,
            max_len: 1,
        }),
        59 => Ok(fixed(SqlType::Real)),
        62 => Ok(fixed(SqlType::Float)),
        60 => Ok(fixed(SqlType::Money)),
        122 => Ok(fixed(SqlType::SmallMoney)),
        106 | 108 => {
            if !(1..=38).contains(&b1) || b2 > b1 {
                return Err(Error::Inconsistent(
                    "a numeric column with an impossible precision or scale",
                ));
            }
            Ok(fixed(SqlType::Numeric {
                precision: b1,
                scale: b2,
            }))
        }
        167 => sized(SqlType::VarChar),
        175 => sized(SqlType::Char),
        231 => sized(SqlType::NVarChar),
        239 => sized(SqlType::NChar),
        165 => sized(SqlType::VarBinary),
        173 => sized(SqlType::Binary),
        34 => sized(SqlType::Image),
        35 => sized(SqlType::Text),
        99 => sized(SqlType::NText),
        36 => Ok(fixed(SqlType::UniqueIdentifier)),
        40 => Ok(fixed(SqlType::Date)),
        41 => scaled(SqlType::Time),
        42 => scaled(SqlType::DateTime2),
        58 => Ok(fixed(SqlType::SmallDateTime)),
        61 => Ok(fixed(SqlType::DateTime)),
        43 => Ok(unsupported("datetimeoffset")),
        98 => Ok(unsupported("sql_variant")),
        241 => Ok(unsupported("xml")),
        189 => Ok(unsupported("timestamp")),
        240 => Ok(unsupported(
            "a CLR type (hierarchyid, geometry or geography)",
        )),
        _ => Ok(unsupported("an unknown system type")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: SqlType,
    pub nullable: bool,
    pub max_len: i16,
    pub column_id: u32,
    pub default_for_absent: bool,
    pub refused: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LobLink {
    pub cumulative_size: u32,
    pub record: RecordId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LobRoot {
    TextPointer {
        timestamp: u64,
        record: RecordId,
    },
    Inline {
        overflow: bool,
        level: u8,
        update_sequence: u32,
        timestamp: u32,
        links: Vec<LobLink>,
    },
}

impl LobRoot {
    pub fn parse(bytes: &[u8]) -> Result<LobRoot, Error> {
        let cut = || Error::Inconsistent("an off-row pointer is cut off");
        if bytes.len() == TEXT_POINTER_LEN {
            return Ok(LobRoot::TextPointer {
                timestamp: u64_at(bytes, 0).ok_or_else(cut)?,
                record: RecordId::from_bytes(bytes, 8).ok_or_else(cut)?,
            });
        }
        let kind = u8_at(bytes, 0);
        let links_len = bytes.len().checked_sub(ROOT_HEADER_LEN);
        let inline = links_len.is_some_and(|len| len >= ROOT_LINK_LEN && len % ROOT_LINK_LEN == 0)
            && matches!(kind, Some(2 | 4));
        if !inline {
            return Err(Error::Unsupported("an off-row pointer of an unknown shape"));
        }
        let mut links = Vec::new();
        let mut previous = 0_u32;
        for at in (ROOT_HEADER_LEN..bytes.len()).step_by(ROOT_LINK_LEN) {
            let cumulative_size = u32_at(bytes, at).ok_or_else(cut)?;
            if cumulative_size < previous {
                return Err(Error::Inconsistent(
                    "the sizes of the links of a LOB root shrink",
                ));
            }
            previous = cumulative_size;
            links.push(LobLink {
                cumulative_size,
                record: RecordId::from_bytes(bytes, at + 4).ok_or_else(cut)?,
            });
        }
        Ok(LobRoot::Inline {
            overflow: kind == Some(2),
            level: u8_at(bytes, 2).ok_or_else(cut)?,
            update_sequence: u32_at(bytes, 4).ok_or_else(cut)?,
            timestamp: u32_at(bytes, 8).ok_or_else(cut)?,
            links,
        })
    }

    pub fn declared_size(&self) -> Option<u64> {
        match self {
            LobRoot::TextPointer { .. } => None,
            LobRoot::Inline { links, .. } => links.last().map(|l| u64::from(l.cumulative_size)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    I64(i64),
    F32(f32),
    F64(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Guid(String),
    DateTime(String),
    LobRef(LobRoot),
}

fn bad(column: &ColumnDef, what: &'static str) -> Error {
    Error::BadValue {
        column: column.name.clone(),
        what,
    }
}

fn unsupported(column: &ColumnDef, what: &'static str) -> Error {
    Error::UnsupportedValue {
        column: column.name.clone(),
        what,
    }
}

pub fn decode_value(cell: Cell<'_>, column: &ColumnDef) -> Result<Value, Error> {
    if let SqlType::Unsupported(name) = column.ty {
        return match cell {
            Cell::Null => Ok(Value::Null),
            Cell::Absent if column.nullable => Ok(Value::Null),
            _ => Err(Error::UnsupportedType {
                column: column.name.clone(),
                type_name: name.to_owned(),
            }),
        };
    }
    match cell {
        Cell::Null => Ok(Value::Null),
        Cell::Absent if column.default_for_absent => Err(unsupported(
            column,
            "a column added with a default value (a row written before it holds the default, which is not read)",
        )),
        Cell::Absent if column.nullable => Ok(Value::Null),
        Cell::Absent => Err(unsupported(
            column,
            "a NOT NULL column added after the row was written (its value is not known)",
        )),
        Cell::Bit(bit) => match column.ty {
            SqlType::Bit => Ok(Value::I64(i64::from(bit))),
            _ => Err(bad(column, "a bit cell in a column that is not a bit")),
        },
        Cell::Complex(bytes) => {
            if !column.ty.may_be_off_row() {
                return Err(bad(
                    column,
                    "an off-row pointer in a column that cannot hold one",
                ));
            }
            LobRoot::parse(bytes)
                .map(Value::LobRef)
                .map_err(|error| match error {
                    Error::Unsupported(what) => unsupported(column, what),
                    Error::Inconsistent(what) => bad(column, what),
                    other => other,
                })
        }
        Cell::Bytes(bytes) => stored(bytes, column),
    }
}

fn exact<const N: usize>(bytes: &[u8], column: &ColumnDef) -> Result<[u8; N], Error> {
    <[u8; N]>::try_from(bytes)
        .map_err(|_| bad(column, "a value whose width is not the width of its type"))
}

fn le_uint(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte))
}

fn stored(bytes: &[u8], column: &ColumnDef) -> Result<Value, Error> {
    match column.ty {
        SqlType::TinyInt => Ok(Value::I64(i64::from(exact::<1>(bytes, column)?[0]))),
        SqlType::SmallInt => Ok(Value::I64(i64::from(i16::from_le_bytes(exact(
            bytes, column,
        )?)))),
        SqlType::Int => Ok(Value::I64(i64::from(i32::from_le_bytes(exact(
            bytes, column,
        )?)))),
        SqlType::BigInt => Ok(Value::I64(i64::from_le_bytes(exact(bytes, column)?))),
        SqlType::Bit => Err(bad(column, "stored bytes in a bit column")),
        SqlType::Real => {
            let value = f32::from_le_bytes(exact(bytes, column)?);
            if !value.is_finite() {
                return Err(bad(column, "a real value that is NaN or infinite"));
            }
            Ok(Value::F32(value))
        }
        SqlType::Float => {
            let value = f64::from_le_bytes(exact(bytes, column)?);
            if !value.is_finite() {
                return Err(bad(column, "a float value that is NaN or infinite"));
            }
            Ok(Value::F64(value))
        }
        SqlType::Numeric { precision, scale } => numeric(bytes, precision, scale, column),
        SqlType::Money => {
            let value = i64::from_le_bytes(exact(bytes, column)?);
            Ok(Value::Decimal(format_scaled(
                value < 0,
                u128::from(value.unsigned_abs()),
                4,
            )))
        }
        SqlType::SmallMoney => {
            let value = i32::from_le_bytes(exact(bytes, column)?);
            Ok(Value::Decimal(format_scaled(
                value < 0,
                u128::from(value.unsigned_abs()),
                4,
            )))
        }
        SqlType::Char | SqlType::VarChar => {
            within_length(bytes, column)?;
            if !bytes.is_ascii() {
                return Err(unsupported(
                    column,
                    "a byte above 0x7F in char or varchar data (the code page of the collation is not read)",
                ));
            }
            std::str::from_utf8(bytes)
                .map(|text| Value::Text(text.to_owned()))
                .map_err(|_| bad(column, "char or varchar data that is not text"))
        }
        SqlType::NChar | SqlType::NVarChar => {
            within_length(bytes, column)?;
            utf16(bytes, column).map(Value::Text)
        }
        SqlType::Binary | SqlType::VarBinary => {
            within_length(bytes, column)?;
            Ok(Value::Bytes(bytes.to_vec()))
        }
        SqlType::Image | SqlType::Text | SqlType::NText => Err(unsupported(
            column,
            "text, ntext or image data stored in the row (the text in row option)",
        )),
        SqlType::UniqueIdentifier => Ok(Value::Guid(guid(&exact::<16>(bytes, column)?))),
        SqlType::Date => {
            let days = date_days(bytes, 3, column)?;
            Ok(Value::DateTime(date_text(days)))
        }
        SqlType::Time(scale) => {
            let width = usize::from(time_width(scale));
            let units = time_units(bytes, width, scale, column)?;
            Ok(Value::DateTime(time_text(units, scale)))
        }
        SqlType::SmallDateTime => {
            let raw: [u8; 4] = exact(bytes, column)?;
            let minutes = u32::from(u16::from_le_bytes([raw[0], raw[1]]));
            let days = i64::from(u16::from_le_bytes([raw[2], raw[3]]));
            if minutes >= 1440 {
                return Err(bad(column, "a smalldatetime of 1440 minutes or more"));
            }
            Ok(Value::DateTime(format!(
                "{}T{}",
                date_text(
                    u32::try_from(days + DAYS_TO_1900)
                        .map_err(|_| bad(column, "a smalldatetime date out of range"))?
                ),
                time_of_day(u64::from(minutes) * 60)
            )))
        }
        SqlType::DateTime => datetime(bytes, column),
        SqlType::DateTime2(scale) => {
            let time_bytes = usize::from(time_width(scale));
            if bytes.len() != time_bytes + 3 {
                return Err(bad(
                    column,
                    "a datetime2 value whose width is not the width of its type",
                ));
            }
            let (time, date) = bytes.split_at(time_bytes);
            let units = time_units(time, time_bytes, scale, column)?;
            let days = date_days(date, 3, column)?;
            Ok(Value::DateTime(format!(
                "{}T{}",
                date_text(days),
                time_text(units, scale)
            )))
        }
        SqlType::Unsupported(name) => Err(Error::UnsupportedType {
            column: column.name.clone(),
            type_name: name.to_owned(),
        }),
    }
}

fn within_length(bytes: &[u8], column: &ColumnDef) -> Result<(), Error> {
    match usize::try_from(column.max_len) {
        Ok(max) if bytes.len() > max => Err(bad(column, "a value longer than its column")),
        _ => Ok(()),
    }
}

fn utf16(bytes: &[u8], column: &ColumnDef) -> Result<String, Error> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(bad(
            column,
            "nchar or nvarchar data of an odd number of bytes",
        ));
    }
    char::decode_utf16(pairs.iter().map(|pair| u16::from_le_bytes(*pair)))
        .collect::<Result<String, _>>()
        .map_err(|_| unsupported(column, "an unpaired surrogate in nchar or nvarchar data"))
}

fn numeric(bytes: &[u8], precision: u8, scale: u8, column: &ColumnDef) -> Result<Value, Error> {
    if bytes.len() != usize::from(numeric_width(precision)) {
        return Err(bad(
            column,
            "a numeric value whose width is not the width of its precision",
        ));
    }
    let negative = match u8_at(bytes, 0) {
        Some(1) => false,
        Some(0) => true,
        _ => return Err(bad(column, "a numeric sign byte that is not 0 or 1")),
    };
    let magnitude = bytes
        .get(1..)
        .ok_or_else(|| bad(column, "a numeric value without a magnitude"))?
        .iter()
        .rev()
        .fold(0_u128, |acc, byte| (acc << 8) | u128::from(*byte));
    if magnitude >= 10_u128.pow(u32::from(precision)) {
        return Err(bad(column, "a numeric magnitude beyond its precision"));
    }
    Ok(Value::Decimal(format_scaled(negative, magnitude, scale)))
}

fn format_scaled(negative: bool, magnitude: u128, scale: u8) -> String {
    let digits = magnitude.to_string();
    let scale = usize::from(scale);
    let body = if scale == 0 {
        digits
    } else {
        let padded = if digits.len() <= scale {
            format!("{}{digits}", "0".repeat(scale + 1 - digits.len()))
        } else {
            digits
        };
        match padded.split_at_checked(padded.len() - scale) {
            Some((whole, fraction)) => format!("{whole}.{fraction}"),
            None => padded,
        }
    };
    if negative && magnitude != 0 {
        format!("-{body}")
    } else {
        body
    }
}

fn guid(raw: &[u8; 16]) -> String {
    let [
        a0,
        a1,
        a2,
        a3,
        b0,
        b1,
        c0,
        c1,
        d0,
        d1,
        e0,
        e1,
        e2,
        e3,
        e4,
        e5,
    ] = *raw;
    format!(
        "{:08X}-{:04X}-{:04X}-{d0:02X}{d1:02X}-{e0:02X}{e1:02X}{e2:02X}{e3:02X}{e4:02X}{e5:02X}",
        u32::from_le_bytes([a0, a1, a2, a3]),
        u16::from_le_bytes([b0, b1]),
        u16::from_le_bytes([c0, c1]),
    )
}

fn date_days(bytes: &[u8], width: usize, column: &ColumnDef) -> Result<u32, Error> {
    if bytes.len() != width {
        return Err(bad(
            column,
            "a date value whose width is not the width of its type",
        ));
    }
    let days = le_uint(bytes);
    if days > MAX_DATE_DAYS {
        return Err(bad(column, "a date after 9999-12-31"));
    }
    u32::try_from(days).map_err(|_| bad(column, "a date after 9999-12-31"))
}

fn time_units(bytes: &[u8], width: usize, scale: u8, column: &ColumnDef) -> Result<u64, Error> {
    if bytes.len() != width {
        return Err(bad(
            column,
            "a time value whose width is not the width of its type",
        ));
    }
    let units = le_uint(bytes);
    if units >= SECONDS_PER_DAY * 10_u64.pow(u32::from(scale)) {
        return Err(bad(column, "a time of 24 hours or more"));
    }
    Ok(units)
}

fn date_text(days: u32) -> String {
    let z = i64::from(days) + 306;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn time_of_day(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn time_text(units: u64, scale: u8) -> String {
    let per_second = 10_u64.pow(u32::from(scale));
    let fraction = units % per_second;
    let clock = time_of_day(units / per_second);
    if fraction == 0 {
        clock
    } else {
        format!("{clock}.{fraction:0width$}", width = usize::from(scale))
    }
}

fn datetime(bytes: &[u8], column: &ColumnDef) -> Result<Value, Error> {
    let raw: [u8; 8] = exact(bytes, column)?;
    let ticks = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let days = i32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    if ticks >= TICKS_PER_DAY {
        return Err(bad(column, "a datetime of 24 hours or more"));
    }
    if !DATETIME_DAYS.contains(&days) {
        return Err(bad(column, "a datetime outside 1753-01-01 to 9999-12-31"));
    }
    let since_year_1 = u32::try_from(i64::from(days) + DAYS_TO_1900)
        .map_err(|_| bad(column, "a datetime outside 1753-01-01 to 9999-12-31"))?;
    let seconds = u64::from(ticks / TICKS_PER_SECOND);
    let tick_in_second = ticks % TICKS_PER_SECOND;
    let fraction = if tick_in_second == 0 {
        String::new()
    } else {
        format!(".{:03}", (tick_in_second * 20 + 3) / 6)
    };
    Ok(Value::DateTime(format!(
        "{}T{}{fraction}",
        date_text(since_year_1),
        time_of_day(seconds)
    )))
}
