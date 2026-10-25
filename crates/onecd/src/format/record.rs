#[path = "record_managed.rs"]
mod managed;
pub(crate) use managed::{decode_row_budgeted, decode_row_into, decode_row_managed};

use crate::error::{Constraint, Error, Location, Result, Unsupported};
use crate::format::bytes::{array, put_u16le, put_u32le, sub, u16le, u32le};
use crate::format::descr::{FieldDescr, FieldType, TableDescr};
use crate::format::layout::{FieldSlot, RecordLayout};
use crate::format::value::{
    BlobRef, BlobVal, DateTime, Numeric, RawDate, Row, Value, cp1251_decode, utf16le_decode,
    write_utf16le_padded,
};

pub(crate) const FLAG_USED: u8 = 0;
pub(crate) const FLAG_FREE: u8 = 1;

pub(crate) fn is_used(rec: &[u8]) -> bool {
    rec.first() == Some(&FLAG_USED)
}

pub fn decode_row(d: &TableDescr, l: &RecordLayout, rec: &[u8]) -> Result<Row> {
    check_layout(d, l)?;
    let mut row = Vec::with_capacity(d.fields.len());
    for i in 0..d.fields.len() {
        row.push(decode_field(d, l, i, rec)?);
    }
    Ok(row)
}

pub fn decode_field(d: &TableDescr, l: &RecordLayout, i: usize, rec: &[u8]) -> Result<Value> {
    let (f, slot) = field_and_slot(d, l, i)?;
    let nullable = f.nullable && f.ty != FieldType::RV;
    let stored = stored_len(slot, nullable)
        .and_then(|len| sub(rec, slot.offset as usize, len))
        .ok_or_else(|| {
            Error::corrupt(
                table_loc(d),
                format!("record is too short for field {}", f.name),
            )
        })?;
    let value = if nullable {
        match stored.split_first() {
            Some((0, _)) | None => return Ok(Value::Null),
            Some((_, value)) => value,
        }
    } else {
        stored
    };
    Ok(decode_value(f, value))
}

pub(crate) fn decode_value(f: &FieldDescr, b: &[u8]) -> Value {
    let invalid = || Value::Invalid(b.to_vec());
    match f.ty {
        FieldType::N => {
            Numeric::from_bcd(b, f.length, f.precision).map_or_else(invalid, Value::Numeric)
        }
        FieldType::C => Value::Char(cp1251_decode(b)),
        FieldType::NC => utf16le_decode(b).map_or_else(invalid, Value::Char),
        FieldType::B => Value::Binary(b.to_vec()),
        FieldType::VC => {
            counted(b, f.length, 1).map_or_else(invalid, |t| Value::VarChar(cp1251_decode(t)))
        }
        FieldType::NVC => counted(b, f.length, 2)
            .and_then(utf16le_decode)
            .map_or_else(invalid, Value::VarChar),
        FieldType::VB => {
            counted(b, f.length, 1).map_or_else(invalid, |v| Value::Binary(v.to_vec()))
        }
        FieldType::T | FieldType::NT | FieldType::I => {
            let (Some(start), Some(len)) = (u32le(b, 0), u32le(b, 4)) else {
                return invalid();
            };
            let r = BlobVal::Ref(BlobRef { start, len });
            match f.ty {
                FieldType::T => Value::Text(r),
                FieldType::NT => Value::NText(r),
                _ => Value::Image(r),
            }
        }
        FieldType::L => b.first().map_or_else(invalid, |&x| Value::Bool(x != 0)),
        FieldType::D => array(b, 0).map_or_else(invalid, |raw| Value::Date(RawDate(raw))),
        FieldType::DT => array(b, 0)
            .and_then(|raw| DateTime::from_bcd(&raw))
            .map_or_else(invalid, Value::DateTime),
        FieldType::RV => match (u32le(b, 0), u32le(b, 4), u32le(b, 8), u32le(b, 12)) {
            (Some(a), Some(b), Some(c), Some(d)) => Value::RowVersion([a, b, c, d]),
            _ => invalid(),
        },
    }
}

fn counted(b: &[u8], max_items: u32, unit: usize) -> Option<&[u8]> {
    let n = u32::from(u16le(b, 0)?);
    if n > max_items {
        return None;
    }
    sub(b, 2, (n as usize).checked_mul(unit)?)
}

pub fn encode_row(d: &TableDescr, l: &RecordLayout, row: &Row, rec: &mut [u8]) -> Result<()> {
    check_layout(d, l)?;
    if row.len() != d.fields.len() {
        return Err(Error::Constraint(Constraint::WrongArity {
            table: d.name.clone(),
            expected: d.fields.len(),
            got: row.len(),
        }));
    }
    let rec = rec.get_mut(..l.record_len as usize).ok_or_else(|| {
        Error::corrupt(
            table_loc(d),
            "record buffer is shorter than the record length",
        )
    })?;
    rec.fill(0);
    if let Some(flag) = rec.first_mut() {
        *flag = FLAG_USED;
    }
    for ((f, slot), v) in d.fields.iter().zip(&l.slots).zip(row) {
        encode_field(d, f, slot, v, rec)?;
    }
    Ok(())
}

fn encode_field(
    d: &TableDescr,
    f: &FieldDescr,
    slot: &FieldSlot,
    v: &Value,
    rec: &mut [u8],
) -> Result<()> {
    if matches!(f.ty, FieldType::C | FieldType::VC | FieldType::D) {
        return Err(Error::Unsupported(Unsupported::FieldType {
            table: d.name.clone(),
            field: f.name.clone(),
            ty: f.ty.code(),
        }));
    }
    let nullable = f.nullable && f.ty != FieldType::RV;
    let start = slot.offset as usize;
    let outside = || slot_err(d, f);
    let stored = stored_len(slot, nullable)
        .and_then(|len| start.checked_add(len))
        .and_then(|end| rec.get_mut(start..end))
        .ok_or_else(outside)?;
    let value = if nullable {
        let (flag, value) = stored.split_first_mut().ok_or_else(outside)?;
        if matches!(v, Value::Null) {
            return Ok(());
        }
        *flag = 1;
        value
    } else {
        if matches!(v, Value::Null) {
            return Err(Error::Constraint(Constraint::NullNotAllowed {
                table: d.name.clone(),
                field: f.name.clone(),
            }));
        }
        stored
    };
    encode_value(d, f, v, value)
}

fn encode_value(d: &TableDescr, f: &FieldDescr, v: &Value, out: &mut [u8]) -> Result<()> {
    let misfit = |why: &str| misfit_err(d, f, why);
    let outside = || slot_err(d, f);
    match (f.ty, v) {
        (FieldType::N, Value::Numeric(n)) => {
            if f.precision > f.length {
                return Err(Error::Constraint(Constraint::BadDescr(format!(
                    "table {}: field {} has precision {} beyond length {}",
                    d.name, f.name, f.precision, f.length
                ))));
            }
            n.write_bcd(f.length, f.precision, out).map_err(misfit)
        }
        (FieldType::NC, Value::Char(s)) => {
            put_utf16(d, f, s, f.length as usize, out)?;
            Ok(())
        }
        (FieldType::NVC, Value::VarChar(s)) => {
            let body = out.get_mut(2..).ok_or_else(outside)?;
            let units = put_utf16(d, f, s, f.length as usize, body)?;
            let units =
                u16::try_from(units).map_err(|_| misfit("string is longer than 65535 units"))?;
            put_u16le(out, 0, units).ok_or_else(outside)
        }
        (FieldType::B, Value::Binary(b)) => {
            if b.len() != f.length as usize {
                return Err(misfit(&format!(
                    "expected exactly {} bytes, got {}",
                    f.length,
                    b.len()
                )));
            }
            out.get_mut(..b.len())
                .ok_or_else(outside)?
                .copy_from_slice(b);
            Ok(())
        }
        (FieldType::VB, Value::Binary(b)) => {
            if b.len() > f.length as usize {
                return Err(misfit(&format!(
                    "at most {} bytes allowed, got {}",
                    f.length,
                    b.len()
                )));
            }
            let n = u16::try_from(b.len()).map_err(|_| misfit("more than 65535 bytes"))?;
            put_u16le(out, 0, n).ok_or_else(outside)?;
            out.get_mut(2..2 + b.len())
                .ok_or_else(outside)?
                .copy_from_slice(b);
            Ok(())
        }
        (FieldType::L, Value::Bool(x)) => {
            *out.first_mut().ok_or_else(outside)? = u8::from(*x);
            Ok(())
        }
        (FieldType::DT, Value::DateTime(dt)) => {
            if !dt.is_valid() {
                return Err(misfit("not a valid date and time"));
            }
            out.get_mut(..7)
                .ok_or_else(outside)?
                .copy_from_slice(&dt.to_bcd());
            Ok(())
        }
        (FieldType::T, Value::Text(b))
        | (FieldType::NT, Value::NText(b))
        | (FieldType::I, Value::Image(b)) => match b {
            BlobVal::Ref(r) => {
                put_u32le(out, 0, r.start).ok_or_else(outside)?;
                put_u32le(out, 4, r.len).ok_or_else(outside)
            }
            BlobVal::Data(_) => Err(misfit(
                "blob bytes have to be stored in the blob file first",
            )),
        },
        (FieldType::RV, Value::RowVersion(words)) => {
            for (k, w) in words.iter().enumerate() {
                put_u32le(out, 4 * k, *w).ok_or_else(outside)?;
            }
            Ok(())
        }
        _ => Err(mismatch_err(d, f)),
    }
}

fn misfit_err(d: &TableDescr, f: &FieldDescr, why: impl Into<String>) -> Error {
    Error::Constraint(Constraint::ValueDoesNotFit {
        table: d.name.clone(),
        field: f.name.clone(),
        why: why.into(),
    })
}

fn mismatch_err(d: &TableDescr, f: &FieldDescr) -> Error {
    Error::Constraint(Constraint::TypeMismatch {
        table: d.name.clone(),
        field: f.name.clone(),
    })
}

fn slot_err(d: &TableDescr, f: &FieldDescr) -> Error {
    Error::corrupt(
        table_loc(d),
        format!("field {} does not fit its slot in the record", f.name),
    )
}

fn put_utf16(d: &TableDescr, f: &FieldDescr, s: &str, cap: usize, out: &mut [u8]) -> Result<usize> {
    if s.encode_utf16().nth(cap).is_some() {
        return Err(misfit_err(
            d,
            f,
            format!("string is longer than {cap} UTF-16 units"),
        ));
    }
    write_utf16le_padded(s, cap, out).ok_or_else(|| slot_err(d, f))
}

fn stored_len(slot: &FieldSlot, nullable: bool) -> Option<usize> {
    (slot.value_size as usize).checked_add(usize::from(nullable))
}

fn table_loc(d: &TableDescr) -> Location {
    Location::Table(d.name.clone())
}

fn check_layout(d: &TableDescr, l: &RecordLayout) -> Result<()> {
    if l.slots.len() != d.fields.len() {
        return Err(Error::corrupt(
            table_loc(d),
            format!(
                "layout has {} slots, the description {} fields",
                l.slots.len(),
                d.fields.len()
            ),
        ));
    }
    Ok(())
}

fn field_and_slot<'a>(
    d: &'a TableDescr,
    l: &'a RecordLayout,
    i: usize,
) -> Result<(&'a FieldDescr, &'a FieldSlot)> {
    match (d.fields.get(i), l.slots.get(i)) {
        (Some(f), Some(slot)) => Ok((f, slot)),
        _ => Err(Error::corrupt(table_loc(d), format!("no field {i}"))),
    }
}

pub fn free_next(rec: &[u8]) -> Option<u32> {
    u32le(rec, 1)
}

pub fn set_free_link(rec: &mut [u8], next: u32) {
    if put_u32le(rec, 1, next).is_none() {
        return;
    }
    if let Some(flag) = rec.first_mut() {
        *flag = FLAG_FREE;
    }
}

pub fn hidden_version(l: &RecordLayout, rec: &[u8]) -> Option<[u32; 2]> {
    if !l.has_hidden_version {
        return None;
    }
    Some([u32le(rec, 1)?, u32le(rec, 5)?])
}
