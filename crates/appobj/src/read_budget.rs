use bsl_vm::Value;
use ibschema::fields::FieldValue;
use query_ir::QValue;

pub(crate) use bsl_stdlib::collections::{
    retained_rc_size as rc_size, retained_shallow_bytes as value_heap,
};
pub(crate) fn slots<T>(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

pub(crate) fn field_heap(value: &FieldValue) -> u64 {
    match value {
        FieldValue::String(s) => s.capacity() as u64,
        FieldValue::Number(n) => n.digits().len() as u64,
        FieldValue::Binary(b) | FieldValue::Storage(b) => b.capacity() as u64,
        _ => 0,
    }
}

pub(crate) fn conversion_peak(value: &FieldValue) -> u64 {
    let payload = match value {
        FieldValue::String(s) => (s.len() as u64).saturating_mul(4),
        FieldValue::Number(n) => (n.digits().len() as u64).saturating_mul(16),
        _ => 0,
    };
    payload.saturating_add(256)
}

pub(crate) fn stored_row(row: &[FieldValue]) -> u64 {
    row.iter().fold(slots::<FieldValue>(row.len()), |n, v| {
        n.saturating_add(field_heap(v))
    })
}

pub(crate) fn rows(rows: &[Vec<Value>], capacity: usize) -> u64 {
    rows.iter().fold(slots::<Vec<Value>>(capacity), |n, row| {
        row.iter()
            .fold(n.saturating_add(slots::<Value>(row.capacity())), |n, v| {
                n.saturating_add(value_heap(v))
            })
    })
}

pub(crate) fn query_row(row: &[QValue]) -> u64 {
    query_exec::ops::row_bytes(row)
}

pub(crate) fn admit(
    ex: &mut bsl_vm::Execution,
    memory: &memory_budget::MemoryBudget,
    bytes: u64,
) -> Result<(), bsl_vm::Exception> {
    let scratch = memory
        .scope_snapshot()
        .map_or_else(|| memory.current_bytes(), |scope| scope.current);
    reserve(ex, bytes.saturating_add(scratch))
}
pub(crate) fn reserve(ex: &mut bsl_vm::Execution, bytes: u64) -> Result<(), bsl_vm::Exception> {
    ex.reserve(bytes.saturating_add(ex.retained_workspace_bytes()))
}

pub(crate) fn remaining(
    ex: &bsl_vm::Execution,
    memory: &memory_budget::MemoryBudget,
    retained: u64,
) {
    let limit = ex
        .options()
        .memory_limit
        .unwrap_or(u64::MAX)
        .saturating_sub(ex.retained_workspace_bytes())
        .saturating_sub(retained);
    if memory.scope_snapshot().is_some() {
        let _ = memory.set_scope_limit(limit);
    } else {
        memory.set_limits(memory_budget::Limits::new(limit));
    }
}

pub(crate) fn hash_growth<T>(capacity: usize, len: usize) -> u64 {
    if len < capacity {
        return 0;
    }
    bsl_vm::retained_hash_bytes::<T>(if capacity == 0 {
        3
    } else {
        capacity.saturating_mul(2).saturating_add(1)
    })
    .unwrap_or(u64::MAX)
}

pub(crate) fn visit_rows(
    rows: &std::rc::Rc<Vec<Vec<Value>>>,
    visit: &mut dyn bsl_vm::RetainedMemory,
) -> Result<(), bsl_vm::Exception> {
    visit.work(1)?;
    if !visit.allocation(
        std::rc::Rc::as_ptr(rows) as usize,
        rc_size::<Vec<Vec<Value>>>(),
    )? {
        return Ok(());
    }
    if rows.capacity() != 0 {
        visit.allocation(rows.as_ptr() as usize, slots::<Vec<Value>>(rows.capacity()))?;
    }
    for row in rows.iter() {
        visit.work(1)?;
        if row.capacity() != 0
            && visit.allocation(row.as_ptr() as usize, slots::<Value>(row.capacity()))?
        {
            for value in row {
                visit.value(value)?;
            }
        }
    }
    Ok(())
}
