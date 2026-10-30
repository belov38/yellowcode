use std::fmt;
use std::ops::Range;

use crate::collate::{Collate, Strength};
use crate::error::{Constraint, Error, Location, Result, Unsupported, brief};
use crate::format::bytes::{sub, u16le};
use crate::format::descr::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};
use crate::format::layout::{FieldSlot, RecordLayout, value_size};
use crate::format::value::{Value, cp1251_decode};

pub const MAX_KEY_LEN: u32 = 0x780;

#[derive(Clone, Copy)]
pub struct KeyCtx<'a> {
    pub collate: Option<&'a dyn Collate>,
    pub locale: &'a str,
}

impl fmt::Debug for KeyCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyCtx")
            .field("collate", &self.collate.map(|_| "<collator>"))
            .field("locale", &self.locale)
            .finish()
    }
}

fn bad_descr(why: impl Into<String>) -> Error {
    Error::Constraint(Constraint::BadDescr(why.into()))
}

fn table_loc(d: &TableDescr) -> Location {
    Location::Table(brief(&d.name).to_owned())
}

fn misfit(d: &TableDescr, f: &FieldDescr, why: impl Into<String>) -> Error {
    Error::Constraint(Constraint::ValueDoesNotFit {
        table: brief(&d.name).to_owned(),
        field: brief(&f.name).to_owned(),
        why: why.into(),
    })
}

pub fn key_slot_size(f: &FieldDescr, prefix_len: u32) -> Result<u32> {
    let n = u64::from(if prefix_len == 0 {
        f.length
    } else {
        prefix_len
    });
    let size = match f.ty {
        FieldType::N | FieldType::L | FieldType::D | FieldType::DT => u64::from(value_size(f)),
        FieldType::C | FieldType::NC | FieldType::VC | FieldType::NVC => {
            if f.case_sensitive {
                4 * n + 3
            } else {
                3 * n + 2
            }
        }
        FieldType::B | FieldType::VB => n,
        FieldType::T | FieldType::NT | FieldType::I | FieldType::RV => {
            return Err(bad_descr(format!(
                "field {} of type {} cannot be indexed",
                f.name,
                f.ty.code()
            )));
        }
    };
    u32::try_from(size).map_err(|_| {
        bad_descr(format!(
            "field {} has a key slot of {size} bytes",
            brief(&f.name)
        ))
    })
}

struct KeyField<'a> {
    index: usize,
    f: &'a FieldDescr,
    prefix_len: u32,
    slot: usize,
}

impl KeyField<'_> {
    fn is_string(&self) -> bool {
        self.f.ty.is_string()
    }
}

fn key_field<'a>(d: &'a TableDescr, idx: &IndexDescr, fld: &IndexField) -> Result<KeyField<'a>> {
    let (index, f) = d.field(&fld.name).ok_or_else(|| {
        bad_descr(format!(
            "index {} refers to the unknown field {}",
            brief(&idx.name),
            brief(&fld.name)
        ))
    })?;
    let slot = key_slot_size(f, fld.prefix_len)?;
    Ok(KeyField {
        index,
        f,
        prefix_len: fld.prefix_len,
        slot: slot as usize,
    })
}

fn key_fields<'a>(d: &'a TableDescr, idx: &IndexDescr) -> Result<Vec<KeyField<'a>>> {
    let mut total = 0u64;
    let mut out = Vec::with_capacity(idx.fields.len());
    for fld in &idx.fields {
        let kf = key_field(d, idx, fld)?;
        total += kf.slot as u64 + u64::from(kf.f.nullable);
        if total > u64::from(MAX_KEY_LEN) {
            return Err(bad_descr(format!(
                "index {}: key is longer than {MAX_KEY_LEN} bytes",
                idx.name
            )));
        }
        out.push(kf);
    }
    Ok(out)
}

pub fn index_key_len(d: &TableDescr, idx: &IndexDescr) -> Result<u32> {
    let mut total = 0u64;
    for field in &idx.fields {
        let (_, descr) = d
            .field(&field.name)
            .ok_or_else(|| bad_descr("index refers to unknown field"))?;
        total += u64::from(key_slot_size(descr, field.prefix_len)?) + u64::from(descr.nullable);
        if total > u64::from(MAX_KEY_LEN) {
            return Err(bad_descr("index key exceeds maximum length"));
        }
    }
    u32::try_from(total).map_err(|_| bad_descr("key length overflows"))
}

enum Part {
    Number(Vec<u8>),
    Fixed(Vec<u8>),
    Binary(Vec<u8>),
    Text(Vec<u16>),
}

fn key_text(kf: &KeyField, mut units: Vec<u16>) -> Vec<u16> {
    let cut = if kf.prefix_len == 0 {
        kf.f.length
    } else {
        kf.prefix_len
    };
    units.truncate(cut as usize);
    while units.last() == Some(&0x20) {
        units.pop();
    }
    units
}

pub fn has_text(d: &TableDescr, l: &RecordLayout, idx: &IndexDescr, rec: &[u8]) -> Result<bool> {
    for kf in key_fields(d, idx)?.iter().filter(|k| k.is_string()) {
        let slot = l.slots.get(kf.index).ok_or_else(|| {
            Error::corrupt(
                table_loc(d),
                format!("the layout has no slot for field {}", brief(&kf.f.name)),
            )
        })?;
        if let Some(Part::Text(units)) = part_of_record(d, kf, slot, rec)?
            && !key_text(kf, units).is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn push_part(kf: &KeyField, part: Option<Part>, cx: &KeyCtx, out: &mut Vec<u8>) -> Result<()> {
    let Some(part) = part else {
        out.extend_from_slice(&[0, 1]);
        if kf.is_string() {
            out.push(0);
        } else {
            out.resize(out.len() + kf.slot.saturating_sub(1), 0);
        }
        return Ok(());
    };
    if kf.f.nullable {
        out.push(1);
    }
    match part {
        Part::Number(bcd) => out.extend_from_slice(&numeric_key(&bcd, kf.f.length)),
        Part::Fixed(bytes) => out.extend_from_slice(&bytes),
        Part::Binary(bytes) => {
            let take = bytes.len().min(kf.slot);
            out.extend_from_slice(bytes.get(..take).unwrap_or_default());
            out.resize(out.len() + kf.slot - take, 0);
        }
        Part::Text(units) => {
            let units = key_text(kf, units);
            let collate = cx
                .collate
                .ok_or(Error::Unsupported(Unsupported::NoCollator))?;
            let strength = if kf.f.case_sensitive {
                Strength::Tertiary
            } else {
                Strength::Secondary
            };
            #[cfg(feature = "icu")]
            if let Some(key) = collate.paid_sort_key(cx.locale, strength, &units) {
                push_slot(&key?, kf.slot, out);
                return Ok(());
            }
            push_slot(
                &collate.sort_key(cx.locale, strength, &units)?,
                kf.slot,
                out,
            );
        }
    }
    Ok(())
}

fn push_slot(key: &[u8], slot: usize, out: &mut Vec<u8>) {
    let (bytes, terminated) = slot_form(key, slot);
    out.extend_from_slice(bytes);
    if terminated {
        out.push(0);
    }
}

fn slot_form(key: &[u8], slot: usize) -> (&[u8], bool) {
    let cut = key.get(..slot).unwrap_or(key);
    let end = cut.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    (cut.get(..end).unwrap_or_default(), end < slot)
}

fn part_of_record(
    d: &TableDescr,
    kf: &KeyField,
    slot: &FieldSlot,
    rec: &[u8],
) -> Result<Option<Part>> {
    let f = kf.f;
    let bad =
        |what: &str| Error::corrupt(table_loc(d), format!("field {}: {what}", brief(&f.name)));
    let stored = (slot.value_size as usize)
        .checked_add(usize::from(f.nullable))
        .and_then(|len| sub(rec, slot.offset as usize, len))
        .ok_or_else(|| bad("record is too short"))?;
    let value = if f.nullable {
        match stored.split_first() {
            Some((0, _)) => return Ok(None),
            Some((_, value)) => value,
            None => return Err(bad("record is too short")),
        }
    } else {
        stored
    };
    let counted = |unit: usize, limit: usize| -> Result<&[u8]> {
        let n = usize::from(u16le(value, 0).ok_or_else(|| bad("value is too short"))?);
        if n > limit {
            return Err(bad(&format!("length {n} exceeds the field length")));
        }
        sub(value, 2, n * unit).ok_or_else(|| bad("value is too short"))
    };
    let utf16 = |b: &[u8]| -> Vec<u16> {
        b.as_chunks::<2>()
            .0
            .iter()
            .filter_map(|c| u16le(c, 0))
            .collect()
    };
    Ok(Some(match f.ty {
        FieldType::N => Part::Number(value.to_vec()),
        FieldType::L | FieldType::D | FieldType::DT => Part::Fixed(value.to_vec()),
        FieldType::B => Part::Binary(value.to_vec()),
        FieldType::VB => Part::Binary(counted(1, f.length as usize)?.to_vec()),
        FieldType::NC => Part::Text(utf16(value)),
        FieldType::NVC => Part::Text(utf16(counted(2, f.length as usize)?)),
        FieldType::C => Part::Text(cp1251_decode(value).encode_utf16().collect()),
        FieldType::VC => Part::Text(
            cp1251_decode(counted(1, f.length as usize)?)
                .encode_utf16()
                .collect(),
        ),
        FieldType::T | FieldType::NT | FieldType::I | FieldType::RV => {
            return Err(bad("this type cannot be indexed"));
        }
    }))
}

fn part_of_value(d: &TableDescr, kf: &KeyField, v: &Value) -> Result<Option<Part>> {
    let f = kf.f;
    let mismatch = || {
        Error::Constraint(Constraint::TypeMismatch {
            table: brief(&d.name).to_owned(),
            field: brief(&f.name).to_owned(),
        })
    };
    let text = |s: &str| -> Result<Part> {
        let units: Vec<u16> = s.encode_utf16().collect();
        if units.len() > f.length as usize {
            return Err(misfit(
                d,
                f,
                format!("string is longer than {} UTF-16 units", f.length),
            ));
        }
        Ok(Part::Text(units))
    };
    Ok(Some(match (f.ty, v) {
        (_, Value::Null) if f.nullable => return Ok(None),
        (_, Value::Null) => {
            return Err(Error::Constraint(Constraint::NullNotAllowed {
                table: brief(&d.name).to_owned(),
                field: brief(&f.name).to_owned(),
            }));
        }
        (FieldType::N, Value::Numeric(n)) => {
            let mut bcd = vec![0u8; kf.slot];
            n.write_bcd(f.length, f.precision, &mut bcd)
                .map_err(|why| misfit(d, f, why))?;
            Part::Number(bcd)
        }
        (FieldType::C | FieldType::NC, Value::Char(s))
        | (FieldType::VC | FieldType::NVC, Value::VarChar(s)) => text(s)?,
        (FieldType::B, Value::Binary(b)) if b.len() == f.length as usize => Part::Binary(b.clone()),
        (FieldType::B, Value::Binary(b)) => {
            return Err(misfit(
                d,
                f,
                format!("expected exactly {} bytes, got {}", f.length, b.len()),
            ));
        }
        (FieldType::VB, Value::Binary(b)) if b.len() <= f.length as usize => {
            Part::Binary(b.clone())
        }
        (FieldType::VB, Value::Binary(b)) => {
            return Err(misfit(
                d,
                f,
                format!("at most {} bytes allowed, got {}", f.length, b.len()),
            ));
        }
        (FieldType::L, Value::Bool(x)) => Part::Fixed(vec![u8::from(*x)]),
        (FieldType::DT, Value::DateTime(dt)) if dt.is_valid() => Part::Fixed(dt.to_bcd().to_vec()),
        (FieldType::DT, Value::DateTime(_)) => {
            return Err(misfit(d, f, "not a valid date and time"));
        }
        (FieldType::D, Value::Date(raw)) => Part::Fixed(raw.0.to_vec()),
        _ => return Err(mismatch()),
    }))
}

pub fn build_key(
    d: &TableDescr,
    l: &RecordLayout,
    idx: &IndexDescr,
    rec: &[u8],
    key_len: u32,
    cx: &KeyCtx,
) -> Result<Vec<u8>> {
    let key_len = key_len as usize;
    let mut out = Vec::new();
    for kf in &key_fields(d, idx)? {
        let slot = l.slots.get(kf.index).ok_or_else(|| {
            Error::corrupt(
                table_loc(d),
                format!("the layout has no slot for field {}", brief(&kf.f.name)),
            )
        })?;
        let part = part_of_record(d, kf, slot, rec)?;
        push_part(kf, part, cx, &mut out)?;
    }
    if out.len() > key_len {
        return Err(Error::corrupt(
            table_loc(d),
            format!(
                "index {}: key of {} bytes is longer than the key length {key_len}",
                idx.name,
                out.len()
            ),
        ));
    }
    out.resize(key_len, 0);
    Ok(out)
}

pub fn encode_key_values(
    d: &TableDescr,
    idx: &IndexDescr,
    vals: &[Value],
    cx: &KeyCtx,
) -> Result<Vec<u8>> {
    let fields = key_fields(d, idx)?;
    if vals.len() > fields.len() {
        return Err(Error::Constraint(Constraint::WrongArity {
            table: brief(&d.name).to_owned(),
            expected: fields.len(),
            got: vals.len(),
        }));
    }
    let mut out = Vec::new();
    for (kf, v) in fields.iter().zip(vals) {
        let part = part_of_value(d, kf, v)?;
        push_part(kf, part, cx, &mut out)?;
    }
    Ok(out)
}

#[cfg(feature = "icu")]
pub fn encode_key_values_budgeted(
    d: &TableDescr,
    idx: &IndexDescr,
    vals: &[Value],
    locale: &str,
    request: &crate::collate::KeyRequest,
) -> Result<memory_budget::BudgetedVec<u8>> {
    use crate::collate::prepared;
    use memory_budget::{BudgetedVec, Category};
    if vals.len() > idx.fields.len() {
        return Err(Error::Constraint(Constraint::WrongArity {
            table: brief(&d.name).to_owned(),
            expected: idx.fields.len(),
            got: vals.len(),
        }));
    }
    index_key_len(d, idx)?;
    let mut capacity = 0;
    for fld in idx.fields.iter().take(vals.len()) {
        let kf = key_field(d, idx, fld)?;
        capacity += kf.slot + usize::from(kf.f.nullable);
    }
    let mut out = prepared(BudgetedVec::try_with_capacity(
        request.budget(),
        Category::Execution,
        capacity,
    ))?;
    for (fld, v) in idx.fields.iter().zip(vals) {
        let kf = key_field(d, idx, fld)?;
        push_value_budgeted(d, &kf, v, locale, request, &mut out)?;
    }
    Ok(out)
}

#[cfg(feature = "icu")]
fn push_value_budgeted(
    d: &TableDescr,
    kf: &KeyField,
    v: &Value,
    locale: &str,
    request: &crate::collate::KeyRequest,
    out: &mut memory_budget::BudgetedVec<u8>,
) -> Result<()> {
    use crate::collate::prepared;
    use memory_budget::{BudgetedVec, Category};
    let f = kf.f;
    let mismatch = || {
        Error::Constraint(Constraint::TypeMismatch {
            table: brief(&d.name).to_owned(),
            field: brief(&f.name).to_owned(),
        })
    };
    let pad = |out: &mut BudgetedVec<u8>, n: usize| prepared(out.try_resize(out.len() + n, 0));
    if matches!(v, Value::Null) {
        if !f.nullable {
            return Err(Error::Constraint(Constraint::NullNotAllowed {
                table: brief(&d.name).to_owned(),
                field: brief(&f.name).to_owned(),
            }));
        }
        prepared(out.try_extend_from_slice(&[0, 1]))?;
        return pad(
            out,
            if kf.is_string() {
                1
            } else {
                kf.slot.saturating_sub(1)
            },
        );
    }
    let start = out.len() + usize::from(f.nullable);
    let flag = |out: &mut BudgetedVec<u8>| -> Result<()> {
        if f.nullable {
            prepared(out.try_push(1))?;
        }
        Ok(())
    };
    match (f.ty, v) {
        (FieldType::N, Value::Numeric(n)) => {
            flag(out)?;
            pad(out, kf.slot)?;
            let bcd = out.as_mut_slice().get_mut(start..).unwrap_or_default();
            n.write_bcd(f.length, f.precision, bcd)
                .map_err(|why| misfit(d, f, why))?;
            numeric_key_in_place(bcd, f.length);
        }
        (FieldType::L, Value::Bool(x)) => {
            flag(out)?;
            prepared(out.try_push(u8::from(*x)))?;
        }
        (FieldType::DT, Value::DateTime(dt)) if dt.is_valid() => {
            flag(out)?;
            prepared(out.try_extend_from_slice(&dt.to_bcd()))?;
        }
        (FieldType::DT, Value::DateTime(_)) => {
            return Err(misfit(d, f, "not a valid date and time"));
        }
        (FieldType::D, Value::Date(raw)) => {
            flag(out)?;
            prepared(out.try_extend_from_slice(&raw.0))?;
        }
        (FieldType::B, Value::Binary(b)) if b.len() != f.length as usize => {
            return Err(misfit(
                d,
                f,
                format!("expected exactly {} bytes, got {}", f.length, b.len()),
            ));
        }
        (FieldType::VB, Value::Binary(b)) if b.len() > f.length as usize => {
            return Err(misfit(
                d,
                f,
                format!("at most {} bytes allowed, got {}", f.length, b.len()),
            ));
        }
        (FieldType::B | FieldType::VB, Value::Binary(b)) => {
            flag(out)?;
            let take = b.len().min(kf.slot);
            prepared(out.try_extend_from_slice(b.get(..take).unwrap_or_default()))?;
            pad(out, kf.slot - take)?;
        }
        (FieldType::C | FieldType::NC, Value::Char(s))
        | (FieldType::VC | FieldType::NVC, Value::VarChar(s)) => {
            if s.encode_utf16().count() > f.length as usize {
                return Err(misfit(
                    d,
                    f,
                    format!("string is longer than {} UTF-16 units", f.length),
                ));
            }
            let cut = if kf.prefix_len == 0 {
                f.length
            } else {
                kf.prefix_len
            } as usize;
            let len = s.encode_utf16().take(cut).count();
            let mut units = prepared(BudgetedVec::try_with_capacity(
                request.budget(),
                Category::Execution,
                len,
            ))?;
            for unit in s.encode_utf16().take(cut) {
                prepared(units.try_push(unit))?;
            }
            while units.as_slice().last() == Some(&0x20) {
                units.pop();
            }
            let strength = if f.case_sensitive {
                Strength::Tertiary
            } else {
                Strength::Secondary
            };
            let key = request.sort_key(locale, strength, units.as_slice())?;
            drop(units);
            flag(out)?;
            let (bytes, terminated) = slot_form(key.as_slice(), kf.slot);
            prepared(out.try_extend_from_slice(bytes))?;
            if terminated {
                prepared(out.try_push(0))?;
            }
        }
        _ => return Err(mismatch()),
    }
    Ok(())
}

fn nibble(b: &[u8], i: usize) -> u8 {
    b.get(i / 2).map_or(0, |&x| {
        if i.is_multiple_of(2) {
            x >> 4
        } else {
            x & 0x0F
        }
    })
}

fn set_nibble(b: &mut [u8], i: usize, v: u8) {
    if let Some(x) = b.get_mut(i / 2) {
        *x = if i.is_multiple_of(2) {
            (*x & 0x0F) | (v << 4)
        } else {
            (*x & 0xF0) | (v & 0x0F)
        };
    }
}

pub fn numeric_key(bcd: &[u8], len: u32) -> Vec<u8> {
    let mut out = bcd.to_vec();
    numeric_key_in_place(&mut out, len);
    out
}

fn numeric_key_in_place(out: &mut [u8], len: u32) {
    let Some(&first) = out.first() else {
        return;
    };
    if first >> 4 != 0 {
        return;
    }
    let end = (len as usize).saturating_add(1).min(out.len() * 2);
    let mut borrow = false;
    for i in (1..end).rev() {
        let d = nibble(out, i);
        let k = if borrow {
            9u8.wrapping_sub(d)
        } else if d != 0 {
            borrow = true;
            10u8.wrapping_sub(d)
        } else {
            0
        };
        set_nibble(out, i, k & 0x0F);
    }
    if !borrow {
        out.fill(0);
        if let Some(sign) = out.first_mut() {
            *sign = 0x10;
        }
    }
}

pub fn split_key(d: &TableDescr, idx: &IndexDescr, key: &[u8]) -> Result<Vec<Range<usize>>> {
    let bad = |what: &str, f: &FieldDescr| {
        Error::corrupt(
            table_loc(d),
            format!(
                "index {}: key part of {}: {what}",
                brief(&idx.name),
                brief(&f.name)
            ),
        )
    };
    let mut pos = 0usize;
    let mut out = Vec::new();
    for kf in &key_fields(d, idx)? {
        let start = pos;
        let mut null = false;
        if kf.f.nullable {
            null = *key.get(pos).ok_or_else(|| bad("key is too short", kf.f))? == 0;
            pos += 1;
        }
        if null {
            pos += if kf.is_string() { 2 } else { kf.slot };
        } else if kf.is_string() {
            let rest = key
                .get(pos..)
                .ok_or_else(|| bad("key is too short", kf.f))?;
            let window = rest.get(..kf.slot).unwrap_or(rest);
            pos += match window.iter().position(|&b| b == 0) {
                Some(end) => end + 1,
                None if window.len() == kf.slot => kf.slot,
                None => return Err(bad("no terminator", kf.f)),
            };
        } else {
            pos += kf.slot;
        }
        if pos > key.len() {
            return Err(bad("key is too short", kf.f));
        }
        out.push(start..pos);
    }
    Ok(out)
}
