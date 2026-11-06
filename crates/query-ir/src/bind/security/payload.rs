use crate::{ExecError, QType, QValue};

pub(super) fn overflow() -> ExecError {
    ExecError::Memory(memory_budget::MemoryLimit {
        category: memory_budget::Category::Execution,
        requested: u64::MAX,
        reason: memory_budget::LimitReason::InvalidSize,
    })
}
fn add(a: u64, b: u64) -> Result<u64, ExecError> {
    a.checked_add(b).ok_or_else(overflow)
}
fn block(bytes: u64) -> Result<u64, ExecError> {
    if bytes == 0 {
        return Ok(0);
    }
    bytes
        .checked_add(15)
        .and_then(|b| (b / 16).checked_mul(16))
        .and_then(|b| b.checked_add(16))
        .ok_or_else(overflow)
}
fn vector<T>(capacity: usize) -> Result<u64, ExecError> {
    block(
        (capacity as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .ok_or_else(overflow)?,
    )
}
fn ty(t: &QType) -> Result<u64, ExecError> {
    add(
        vector::<crate::RefAtom>(t.refs.capacity())?,
        vector::<crate::Other>(t.others.capacity())?,
    )
}
pub(super) fn bytes(v: &QValue) -> Result<u64, ExecError> {
    value(v, 0)
}
pub(super) fn clone_bytes(v: &QValue) -> Result<u64, ExecError> {
    copied_value(v, 0)
}
fn copied_value(v: &QValue, depth: u32) -> Result<u64, ExecError> {
    if depth > 128 {
        return Err(overflow());
    }
    match v {
        QValue::Type(t) => add(
            vector::<crate::RefAtom>(t.refs.len())?,
            vector::<crate::Other>(t.others.len())?,
        ),
        QValue::Number(n) => {
            if n.size_hint_limbs() >= 2 {
                block(n.size_hint_limbs().checked_mul(4).ok_or_else(overflow)?)
            } else {
                Ok(0)
            }
        }
        QValue::PointInTime(_) => block(std::mem::size_of::<crate::value::Moment>() as u64),
        QValue::Boundary(b) => add(
            block(std::mem::size_of::<crate::value::Boundary>() as u64)?,
            copied_value(&b.point, depth + 1)?,
        ),
        _ => Ok(0),
    }
}
fn value(v: &QValue, depth: u32) -> Result<u64, ExecError> {
    if depth > 128 {
        return Err(overflow());
    }
    match v {
        QValue::String(s) => block(add(
            16,
            (s.0.len() as u64).checked_mul(2).ok_or_else(overflow)?,
        )?),
        QValue::Binary(b) | QValue::Storage(b) => block(add(16, b.len() as u64)?),
        QValue::Number(n) => {
            let limbs = n.size_hint_limbs().max(n.backing_capacity_limbs() as u64);
            if limbs >= 2 {
                block(limbs.checked_mul(4).ok_or_else(overflow)?)
            } else {
                Ok(0)
            }
        }
        QValue::Type(t) => ty(t),
        QValue::PointInTime(_) => block(std::mem::size_of::<crate::value::Moment>() as u64),
        QValue::Boundary(b) => add(
            block(std::mem::size_of::<crate::value::Boundary>() as u64)?,
            value(&b.point, depth + 1)?,
        ),
        QValue::Nested(n) => {
            let mut b = add(
                block(16 + std::mem::size_of::<crate::NestedResult>() as u64)?,
                vector::<Vec<QValue>>(n.rows.capacity())?,
            )?;
            b = add(b, columns(&n.columns, depth + 1)?)?;
            for row in &n.rows {
                b = add(b, vector::<QValue>(row.capacity())?)?;
                for v in row {
                    b = add(b, value(v, depth + 1)?)?;
                }
            }
            Ok(b)
        }
        _ => Ok(0),
    }
}
fn columns(cs: &Vec<crate::ColumnInfo>, depth: u32) -> Result<u64, ExecError> {
    if depth > 128 {
        return Err(overflow());
    }
    let mut b = vector::<crate::ColumnInfo>(cs.capacity())?;
    for c in cs {
        b = add(b, add(block(c.name.capacity() as u64)?, ty(&c.ty)?)?)?;
        if let Some(cs) = &c.nested {
            b = add(b, columns(cs, depth + 1)?)?;
        }
    }
    Ok(b)
}

pub(super) fn name_bytes(name: &str) -> Result<u64, ExecError> {
    block(add(
        (name.len() as u64).checked_mul(3).ok_or_else(overflow)?,
        (8 * (std::mem::size_of::<String>() + std::mem::size_of::<QValue>())) as u64,
    )?)
}
