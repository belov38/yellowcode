pub mod managed;

use std::sync::Arc;

use bsl_prim::{BslDate, Number};
use ibschema::columns::ColType;
use ibschema::fields::{FieldValue, GUID_LE_FIELDS, is_version};
use ibschema::logical::TypeCode;
use ibschema::storage::{FieldId, TableInfo};
use ibschema::values::{RefId, Uuid16, datetime_from_ticks, datetime_to_ticks};
use ibschema::{DateTime, Numeric};

use crate::exec_error::ExecError;
use crate::value::{QStr, QValue};

const SMALL_DIGITS: usize = 18;

pub fn number_to_q(n: &Numeric) -> Number {
    let digits = n.digits();
    if digits.len() <= SMALL_DIGITS {
        let mut v: i64 = 0;
        for &d in digits {
            v = v * 10 + i64::from(d.min(9));
        }
        if n.is_negative() {
            v = -v;
        }
        return Number::from_scaled_i64(v, n.scale());
    }
    let int_len = digits.len().saturating_sub(n.scale() as usize);
    let (int, frac) = digits.split_at(int_len);
    let mut text = String::with_capacity(digits.len() + 2);
    if n.is_negative() {
        text.push('-');
    }
    text.extend(int.iter().map(|&d| char::from(b'0' + d.min(9))));
    if !frac.is_empty() {
        text.push('.');
        text.extend(frac.iter().map(|&d| char::from(b'0' + d.min(9))));
    }
    Number::parse(&text).unwrap_or_else(Number::zero)
}

pub fn number_from_q(n: &Number) -> Option<Numeric> {
    Numeric::parse(&n.to_xml_string())
}

pub fn date_to_q(d: &DateTime) -> BslDate {
    i64::try_from(datetime_to_ticks(d))
        .ok()
        .and_then(BslDate::from_ticks)
        .unwrap_or(BslDate::EMPTY)
}

pub fn date_from_q(d: BslDate) -> Option<DateTime> {
    u64::try_from(d.ticks())
        .ok()
        .and_then(|t| datetime_from_ticks(t).ok())
}

pub fn to_q(v: &FieldValue) -> QValue {
    match v {
        FieldValue::Null => QValue::Null,
        FieldValue::Undefined => QValue::Undefined,
        FieldValue::Bool(b) => QValue::Bool(*b),
        FieldValue::Number(n) => QValue::Number(number_to_q(n)),
        FieldValue::Date(d) => QValue::Date(date_to_q(d)),
        FieldValue::String(s) => QValue::String(QStr::from_str(s)),
        FieldValue::Uuid(u) => QValue::Uuid(*u.as_bytes()),
        FieldValue::Ref { table, id } => QValue::Ref {
            table: *table,
            id: id.0,
        },
        FieldValue::Storage(b) => QValue::Storage(Arc::<[u8]>::from(b.as_slice()).into()),
        FieldValue::Binary(b) => QValue::Binary(Arc::<[u8]>::from(b.as_slice()).into()),
        FieldValue::RowVersion(w) => {
            let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_be_bytes()).collect();
            QValue::Binary(Arc::<[u8]>::from(bytes).into())
        }
    }
}

fn binary16(t: &TableInfo, f: FieldId) -> Option<bool> {
    let fi = t.field(f)?;
    if !matches!(fi.types.as_slice(), [ty] if ty.code == TypeCode::B) {
        return None;
    }
    let cols = t.columns.get(fi.columns.clone())?;
    matches!(cols, [c] if c.ty == ColType::B(16))
        .then(|| GUID_LE_FIELDS.contains(&fi.name.as_str()))
}

pub fn data_version(w: &[u32; 4]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&w[2].to_be_bytes());
    b[4..].copy_from_slice(&w[3].to_be_bytes());
    let mut out = String::with_capacity(12);
    for c in b.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, x)| a | u32::from(*x) << (16 - 8 * i));
        for k in 0..4 {
            if k <= c.len() {
                out.push(char::from(
                    ABC.get(((n >> (18 - 6 * k)) & 63) as usize)
                        .copied()
                        .unwrap_or(b'A'),
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn row_to_q(t: &TableInfo, row: &[FieldValue]) -> Vec<QValue> {
    row.iter()
        .enumerate()
        .map(|(i, v)| match v {
            FieldValue::Uuid(u) => {
                let le = u16::try_from(i).ok().and_then(|i| binary16(t, FieldId(i)));
                match le {
                    Some(true) => {
                        QValue::Binary(Arc::<[u8]>::from(u.to_bytes_le().as_slice()).into())
                    }
                    Some(false) => {
                        QValue::Binary(Arc::<[u8]>::from(RefId::from_uuid(*u).0.as_slice()).into())
                    }
                    None => to_q(v),
                }
            }
            FieldValue::RowVersion(w) => QValue::String(QStr::from_str(&data_version(w))),
            _ => to_q(v),
        })
        .collect()
}

fn refuse(t: &TableInfo, f: FieldId, why: &str) -> ExecError {
    let name = t.field(f).map_or("?", |x| x.name.as_str());
    ExecError::Fatal(format!("{}.{name}: {why}", t.logical))
}

pub fn from_q(v: &QValue, t: &TableInfo, f: FieldId) -> Result<FieldValue, ExecError> {
    if t.field(f).is_some_and(is_version) {
        return match v {
            QValue::Binary(b) if b.len() == 16 => {
                let mut w = [0u32; 4];
                for (x, c) in w.iter_mut().zip(b.as_chunks::<4>().0.iter()) {
                    *x = u32::from_be_bytes(*c);
                }
                Ok(FieldValue::RowVersion(w))
            }
            _ => Err(refuse(t, f, "a row version is 16 bytes")),
        };
    }
    if let QValue::Binary(b) = v
        && let Some(le) = binary16(t, f)
        && let Ok(a) = <[u8; 16]>::try_from(&**b)
    {
        return Ok(FieldValue::Uuid(if le {
            Uuid16::from_bytes_le(a)
        } else {
            RefId(a).to_uuid()
        }));
    }
    Ok(match v {
        QValue::Null => FieldValue::Null,
        QValue::Undefined => FieldValue::Undefined,
        QValue::Bool(b) => FieldValue::Bool(*b),
        QValue::Number(n) => {
            FieldValue::Number(number_from_q(n).ok_or_else(|| refuse(t, f, "number"))?)
        }
        QValue::Date(d) => FieldValue::Date(
            date_from_q(*d).ok_or_else(|| refuse(t, f, "a date with a fraction of a second"))?,
        ),
        QValue::String(s) => FieldValue::String(
            String::from_utf16(&s.0)
                .map_err(|_| refuse(t, f, "an unpaired surrogate in a string"))?,
        ),
        QValue::Uuid(u) => FieldValue::Uuid(Uuid16::from_bytes(*u)),
        QValue::Ref { table, id } => FieldValue::Ref {
            table: *table,
            id: RefId(*id),
        },
        QValue::Binary(b) => FieldValue::Binary(b.to_vec()),
        QValue::Storage(b) => FieldValue::Storage(b.to_vec()),
        QValue::Type(_) | QValue::SysEnum(..) | QValue::Nested(_) => {
            return Err(refuse(
                t,
                f,
                "a type, a system enumeration value or a nested table is not stored",
            ));
        }
        QValue::PointInTime(_) | QValue::Boundary(_) => {
            return Err(refuse(t, f, "a point in time or a boundary is not stored"));
        }
    })
}
