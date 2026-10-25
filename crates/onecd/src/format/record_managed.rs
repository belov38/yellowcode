use super::*;
use crate::format::value::cp1251_char;
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, Grant, LimitReason, MemoryBudget, MemoryLimit,
    RequestMemory, extend_within,
};

fn invalid_record() -> Error {
    Error::CorruptStatic {
        location: Location::Root,
        what: "invalid stored record",
    }
}

fn value_limit(requested: u64, category: Category) -> Error {
    BudgetError::Limit(MemoryLimit {
        category,
        requested,
        reason: LimitReason::Category,
    })
    .into()
}

const VALUE_OWNER: usize = std::mem::size_of::<BudgetedVec<u8>>();

enum Plan<'a> {
    Scalar(Value),
    Bytes {
        source: &'a [u8],
        invalid: bool,
    },
    Text {
        source: &'a [u8],
        utf16: bool,
        var: bool,
        capacity: usize,
    },
    Number {
        source: &'a [u8],
        length: u32,
        precision: u32,
        capacity: usize,
    },
}

impl Plan<'_> {
    fn charge(&self) -> Result<usize> {
        let bytes = match self {
            Self::Scalar(_) => 0,
            Self::Bytes { source, .. } => source.len(),
            Self::Text { capacity, .. } | Self::Number { capacity, .. } => *capacity,
        };
        Ok(bytes
            .checked_add(VALUE_OWNER)
            .ok_or(BudgetError::Overflow)?)
    }
    fn build(self, grant: &mut Grant) -> Result<Value> {
        Ok(match self {
            Self::Scalar(value) => {
                grant.try_vec_unowned::<u8>(0)?;
                value
            }
            Self::Bytes { source, invalid } => {
                let mut out = grant.try_vec_unowned::<u8>(source.len())?;
                extend_within(&mut out, source)?;
                if invalid {
                    Value::Invalid(out)
                } else {
                    Value::Binary(out)
                }
            }
            Self::Text {
                source,
                utf16,
                var,
                capacity,
            } => {
                let mut out = grant.try_vec_unowned::<u8>(capacity)?;
                let mut append = |ch: char| -> Result<()> {
                    let mut encoded = [0; 4];
                    extend_within(&mut out, ch.encode_utf8(&mut encoded).as_bytes())?;
                    Ok(())
                };
                if utf16 {
                    for ch in char::decode_utf16(
                        source
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .copied()
                            .map(u16::from_le_bytes),
                    ) {
                        append(ch.map_err(|_| invalid_record())?)?;
                    }
                } else {
                    for byte in source {
                        append(cp1251_char(*byte))?;
                    }
                }
                let text = String::from_utf8(out).map_err(|_| invalid_record())?;
                if var {
                    Value::VarChar(text)
                } else {
                    Value::Char(text)
                }
            }
            Self::Number {
                source,
                length,
                precision,
                capacity,
            } => Value::Numeric(Numeric::from_bcd_into(
                source,
                length,
                precision,
                grant.try_vec_unowned::<u8>(capacity)?,
            )?),
        })
    }
}

fn text_capacity(source: &[u8], utf16: bool) -> Result<Option<usize>> {
    if !utf16 {
        return Ok(Some(
            source.len().checked_mul(3).ok_or(BudgetError::Overflow)?,
        ));
    }
    if !source.len().is_multiple_of(2) {
        return Ok(None);
    }
    if char::decode_utf16(
        source
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes),
    )
    .any(|ch| ch.is_err())
    {
        return Ok(None);
    }
    Ok(Some(
        (source.len() / 2)
            .checked_mul(3)
            .ok_or(BudgetError::Overflow)?,
    ))
}

fn text_plan(source: &[u8], utf16: bool, var: bool) -> Result<Option<Plan<'_>>> {
    Ok(text_capacity(source, utf16)?.map(|capacity| Plan::Text {
        source,
        utf16,
        var,
        capacity,
    }))
}

fn value_plan<'a>(field: &FieldDescr, source: &'a [u8]) -> Result<Plan<'a>> {
    let invalid = Plan::Bytes {
        source,
        invalid: true,
    };
    Ok(match field.ty {
        FieldType::N => match Numeric::bcd_capacity(source, field.length, field.precision)? {
            Some(capacity) => Plan::Number {
                source,
                length: field.length,
                precision: field.precision,
                capacity,
            },
            None => invalid,
        },
        FieldType::C | FieldType::NC => {
            text_plan(source, field.ty == FieldType::NC, false)?.unwrap_or(invalid)
        }
        FieldType::B => Plan::Bytes {
            source,
            invalid: false,
        },
        FieldType::VC | FieldType::NVC => {
            let utf16 = field.ty == FieldType::NVC;
            match counted(source, field.length, if utf16 { 2 } else { 1 }) {
                Some(counted) => text_plan(counted, utf16, true)?.unwrap_or(invalid),
                None => invalid,
            }
        }
        FieldType::VB => match counted(source, field.length, 1) {
            Some(counted) => Plan::Bytes {
                source: counted,
                invalid: false,
            },
            None => invalid,
        },
        FieldType::T | FieldType::NT | FieldType::I => match (u32le(source, 0), u32le(source, 4)) {
            (Some(start), Some(len)) => {
                let reference = BlobVal::Ref(BlobRef { start, len });
                Plan::Scalar(match field.ty {
                    FieldType::T => Value::Text(reference),
                    FieldType::NT => Value::NText(reference),
                    _ => Value::Image(reference),
                })
            }
            _ => invalid,
        },
        FieldType::L => match source.first() {
            Some(byte) => Plan::Scalar(Value::Bool(*byte != 0)),
            None => invalid,
        },
        FieldType::D => match array(source, 0) {
            Some(raw) => Plan::Scalar(Value::Date(RawDate(raw))),
            None => invalid,
        },
        FieldType::DT => match array(source, 0).and_then(|raw| DateTime::from_bcd(&raw)) {
            Some(date) => Plan::Scalar(Value::DateTime(date)),
            None => invalid,
        },
        FieldType::RV => match (
            u32le(source, 0),
            u32le(source, 4),
            u32le(source, 8),
            u32le(source, 12),
        ) {
            (Some(a), Some(b), Some(c), Some(d)) => Plan::Scalar(Value::RowVersion([a, b, c, d])),
            _ => invalid,
        },
    })
}

fn field_plan<'a>(field: &FieldDescr, slot: &FieldSlot, record: &'a [u8]) -> Result<Plan<'a>> {
    let nullable = field.nullable && field.ty != FieldType::RV;
    let stored = stored_len(slot, nullable)
        .and_then(|length| sub(record, slot.offset as usize, length))
        .ok_or_else(invalid_record)?;
    if nullable {
        match stored.split_first() {
            Some((0, _)) | None => Ok(Plan::Scalar(Value::Null)),
            Some((_, source)) => value_plan(field, source),
        }
    } else {
        value_plan(field, stored)
    }
}

pub(crate) fn decode_row_budgeted(
    d: &TableDescr,
    l: &RecordLayout,
    record: &[u8],
    context: &RequestMemory,
) -> Result<Charged<Row>> {
    decode_row_managed(
        d,
        l,
        record,
        &context.budget,
        Category::Row,
        context.row_limit,
    )
}

fn row_estimate(d: &TableDescr, l: &RecordLayout, record: &[u8]) -> Result<u64> {
    let mut estimated = record
        .len()
        .checked_add(std::mem::size_of::<BudgetedVec<u8>>())
        .and_then(|n| n.checked_add(std::mem::size_of::<BudgetedVec<Value>>()))
        .and_then(|n| {
            d.fields
                .len()
                .checked_mul(std::mem::size_of::<Value>())
                .and_then(|fields| n.checked_add(fields))
        })
        .ok_or(BudgetError::Overflow)?;
    for (field, slot) in d.fields.iter().zip(&l.slots) {
        let raw = slot.value_size as usize;
        let extra = match field.ty {
            FieldType::C | FieldType::VC => raw.checked_mul(3),
            FieldType::NC | FieldType::NVC => (raw / 2).checked_mul(3),
            FieldType::N => usize::try_from(field.length)
                .ok()
                .and_then(|n| n.checked_add(1)),
            _ => Some(raw),
        }
        .ok_or(BudgetError::Overflow)?
        .max(raw);
        estimated = estimated
            .checked_add(std::mem::size_of::<BudgetedVec<u8>>())
            .and_then(|n| n.checked_add(extra))
            .ok_or(BudgetError::Overflow)?;
    }
    Ok(u64::try_from(estimated).map_err(|_| BudgetError::Overflow)?)
}

fn row_charge(
    d: &TableDescr,
    l: &RecordLayout,
    record: &[u8],
    category: Category,
    limit: u64,
) -> Result<u64> {
    if d.fields.len() != l.slots.len() {
        return Err(invalid_record());
    }
    let estimated = row_estimate(d, l, record)?;
    if estimated > limit {
        return Err(value_limit(estimated, category));
    }
    let mut charge = d
        .fields
        .len()
        .checked_mul(std::mem::size_of::<Value>())
        .and_then(|n| n.checked_add(std::mem::size_of::<BudgetedVec<Value>>()))
        .ok_or(BudgetError::Overflow)?;
    for (field, slot) in d.fields.iter().zip(&l.slots) {
        charge = charge
            .checked_add(field_plan(field, slot, record)?.charge()?)
            .ok_or(BudgetError::Overflow)?;
    }
    Ok(u64::try_from(charge).map_err(|_| BudgetError::Overflow)?)
}

fn build_row(d: &TableDescr, l: &RecordLayout, record: &[u8], grant: &mut Grant) -> Result<Row> {
    let mut row = grant.try_vec_unowned::<Value>(d.fields.len())?;
    for (field, slot) in d.fields.iter().zip(&l.slots) {
        let value = field_plan(field, slot, record)?.build(grant)?;
        if row.len() == row.capacity() {
            return Err(invalid_record());
        }
        row.push(value);
    }
    Ok(row)
}

pub(crate) fn decode_row_managed(
    d: &TableDescr,
    l: &RecordLayout,
    record: &[u8],
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<Row>> {
    let charge = row_charge(d, l, record, category, limit)?;
    budget.try_prepaid(category, charge, |grant| build_row(d, l, record, grant))
}

pub(crate) fn decode_row_into<'s>(
    d: &TableDescr,
    l: &RecordLayout,
    record: &[u8],
    scratch: &'s mut memory_budget::Scratch<Row>,
    category: Category,
    limit: u64,
) -> Result<&'s Row> {
    let charge = row_charge(d, l, record, category, limit)?;
    scratch.refill(charge, |grant| build_row(d, l, record, grant))
}
