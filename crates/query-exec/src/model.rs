use crate::coll::{ExecCollation, Keys};
use bsl_prim::{Collation, Strength};
use query_ir::{ColumnInfo, ExecError, NestedResult, QType, QValue};

use crate::key::Key;

pub const BLOCK_HEADER: u64 = 16;

pub const fn block(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    n.div_ceil(16)
        .saturating_mul(16)
        .saturating_add(BLOCK_HEADER)
}

pub const fn vec_block(cap: usize, elem: usize) -> u64 {
    block((cap as u64).saturating_mul(elem as u64))
}

pub const VALUE_SIZE: usize = std::mem::size_of::<QValue>();

pub fn value_model(v: &QValue) -> u64 {
    match v {
        QValue::String(s) => block(16 + 2 * s.0.len() as u64),
        QValue::Binary(b) | QValue::Storage(b) => block(16 + b.len() as u64),
        QValue::Number(n) => {
            let limbs = n.size_hint_limbs();
            if limbs >= 2 { block(4 * limbs) } else { 0 }
        }
        QValue::Type(t) => type_model(t),
        QValue::Nested(n) => nested_model(n),
        QValue::PointInTime(_) => block(std::mem::size_of::<query_ir::value::Moment>() as u64),
        QValue::Boundary(b) => {
            block(std::mem::size_of::<query_ir::value::Boundary>() as u64) + value_model(&b.point)
        }
        QValue::Null
        | QValue::Undefined
        | QValue::Bool(_)
        | QValue::Date(_)
        | QValue::Uuid(_)
        | QValue::Ref { .. }
        | QValue::SysEnum(..) => 0,
    }
}

pub fn type_model(t: &QType) -> u64 {
    vec_block(t.refs.capacity(), std::mem::size_of::<query_ir::RefAtom>())
        + vec_block(t.others.capacity(), std::mem::size_of::<query_ir::Other>())
}

pub(crate) fn invalid_size() -> ExecError {
    ExecError::Memory(memory_budget::MemoryLimit {
        category: memory_budget::Category::Execution,
        requested: u64::MAX,
        reason: memory_budget::LimitReason::InvalidSize,
    })
}

pub(crate) fn size_add(a: u64, b: u64) -> Result<u64, ExecError> {
    a.checked_add(b).ok_or_else(invalid_size)
}

pub(crate) fn size_block(bytes: u64) -> Result<u64, ExecError> {
    if bytes == 0 {
        return Ok(0);
    }
    bytes
        .checked_add(15)
        .map(|n| n / 16)
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(BLOCK_HEADER))
        .ok_or_else(invalid_size)
}

pub(crate) fn size_vec(capacity: usize, element: usize) -> Result<u64, ExecError> {
    let bytes = (capacity as u64)
        .checked_mul(element as u64)
        .ok_or_else(invalid_size)?;
    size_block(bytes)
}

fn temp_aligned(size: usize, alignment: usize) -> Result<usize, ExecError> {
    size.checked_add(alignment - 1)
        .map(|n| n / alignment * alignment)
        .ok_or_else(invalid_size)
}

pub(crate) fn temp_tree_model(high_water: usize) -> Result<u64, ExecError> {
    use crate::temp::{TempTable, TempTableId};
    use std::alloc::Layout;
    use std::mem::{align_of, size_of};
    use std::ptr::NonNull;

    if high_water == 0 {
        return Ok(0);
    }
    let alignment = align_of::<TempTableId>()
        .max(align_of::<TempTable>())
        .max(align_of::<Option<NonNull<()>>>())
        .max(align_of::<u16>());
    if alignment > 16 || 16 % alignment != 0 {
        return Err(invalid_size());
    }
    let keys = Layout::array::<TempTableId>(11).map_err(|_| invalid_size())?;
    let values = Layout::array::<TempTable>(11).map_err(|_| invalid_size())?;
    let edges = Layout::array::<NonNull<()>>(12).map_err(|_| invalid_size())?;
    let mut node = 0usize;
    for field in [
        size_of::<Option<NonNull<()>>>(),
        size_of::<u16>(),
        size_of::<u16>(),
        keys.size(),
        values.size(),
        edges.size(),
    ] {
        node = node
            .checked_add(temp_aligned(field, alignment)?)
            .ok_or_else(invalid_size)?;
    }
    Layout::from_size_align(node, alignment).map_err(|_| invalid_size())?;
    let nodes = high_water
        .checked_sub(1)
        .map(|n| n / 5)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(invalid_size)?;
    size_block(u64::try_from(node).map_err(|_| invalid_size())?)?
        .checked_mul(u64::try_from(nodes).map_err(|_| invalid_size())?)
        .ok_or_else(invalid_size)
}

pub(crate) fn temp_name_table_model(capacity: usize) -> Result<u64, ExecError> {
    use crate::temp::TempTableId;
    if capacity == 0 {
        return Ok(0);
    }
    let buckets = if capacity < 4 {
        4
    } else if capacity < 8 {
        8
    } else {
        capacity
            .checked_mul(8)
            .map(|n| n / 7)
            .and_then(usize::checked_next_power_of_two)
            .ok_or_else(invalid_size)?
    };
    let slots =
        std::alloc::Layout::array::<(String, TempTableId)>(buckets).map_err(|_| invalid_size())?;
    if slots.align() > 16 || 16 % slots.align() != 0 {
        return Err(invalid_size());
    }
    let bytes = slots
        .size()
        .checked_add(buckets)
        .and_then(|n| n.checked_add(16))
        .ok_or_else(invalid_size)?;
    std::alloc::Layout::from_size_align(bytes, slots.align()).map_err(|_| invalid_size())?;
    size_block(u64::try_from(bytes).map_err(|_| invalid_size())?)
}

fn type_capacity(t: &QType, copy: bool) -> Result<u64, ExecError> {
    size_add(
        size_vec(
            if copy {
                t.refs.len()
            } else {
                t.refs.capacity()
            },
            std::mem::size_of::<query_ir::RefAtom>(),
        )?,
        size_vec(
            if copy {
                t.others.len()
            } else {
                t.others.capacity()
            },
            std::mem::size_of::<query_ir::Other>(),
        )?,
    )
}

pub(crate) fn temp_schema_node_model() -> u64 {
    block(16 + std::mem::size_of::<query_ir::TempSchema>() as u64)
}

pub(crate) fn index_merge_schedule_model(count: usize) -> Result<u64, ExecError> {
    size_vec(count, std::mem::size_of::<bool>())
}

pub(crate) fn temp_schema_model(schema: &query_ir::TempSchema) -> Result<u64, ExecError> {
    let mut bytes = size_add(
        temp_schema_node_model(),
        size_block(schema.name.capacity() as u64)?,
    )?;
    bytes = size_add(
        bytes,
        size_vec(
            schema.columns.capacity(),
            std::mem::size_of::<query_ir::TempColumn>(),
        )?,
    )?;
    for column in &schema.columns {
        bytes = size_add(bytes, size_block(column.name.capacity() as u64)?)?;
        if let Some(ty) = &column.ty {
            bytes = size_add(bytes, type_capacity(ty, false)?)?;
        }
    }
    Ok(bytes)
}

pub(crate) fn totals_spec_clone_model(
    spec: &query_ir::totals::TotalsSpec,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    totals_spec_capacity(spec, true, false, mem, env)
}

pub(crate) fn totals_spec_copy_model(
    spec: &query_ir::totals::TotalsSpec,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    totals_spec_capacity(spec, true, true, mem, env)
}

pub(crate) fn totals_spec_model(
    spec: &query_ir::totals::TotalsSpec,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    totals_spec_capacity(spec, false, true, mem, env)
}

pub(crate) fn totals_spec_clone_scratch_model(
    spec: &query_ir::totals::TotalsSpec,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    use query_ir::totals::TotalsExpr as E;
    let mut pending = SizingStack::new(mem);
    let mut cancel = LedgerWork { env, ticks: 0 };
    for (root, _) in &spec.items {
        if matches!(root, E::Agg(_) | E::Param(_) | E::Column(_) | E::Const(_)) {
            continue;
        }
        pending.push(root)?;
        while let Some(expression) = pending.pop() {
            cancel.step()?;
            match expression {
                E::Neg(child) | E::Not(child) => pending.push(child.as_ref())?,
                E::Bin(_, a, b) | E::Cmp(_, a, b) | E::IsNull(a, b) => {
                    pending.push(b.as_ref())?;
                    pending.push(a.as_ref())?;
                }
                E::And(values) | E::Or(values) => {
                    for child in values.iter().rev() {
                        cancel.step()?;
                        pending.push(child)?;
                    }
                }
                E::Case { whens, otherwise } => {
                    if let Some(child) = otherwise {
                        pending.push(child.as_ref())?;
                    }
                    for (a, b) in whens.iter().rev() {
                        cancel.step()?;
                        pending.push(b)?;
                        pending.push(a)?;
                    }
                }
                E::Agg(_) | E::Param(_) | E::Column(_) | E::Const(_) => {}
            }
        }
    }
    cancel.check()?;
    let capacity = pending.items.capacity();
    let old_capacity = if capacity > 4 { capacity / 2 } else { 0 };
    let item_size = std::mem::size_of::<(&E, &mut E)>();
    std::alloc::Layout::array::<(&E, &mut E)>(capacity).map_err(|_| invalid_size())?;
    size_add(
        size_vec(capacity, item_size)?,
        size_vec(old_capacity, item_size)?,
    )
}

struct TotalsPayloads<'m, 'a> {
    seen: std::collections::HashSet<(u8, usize, usize)>,
    model: HashModel,
    mem: &'m crate::ops::MemBudget<'a>,
}

impl TotalsPayloads<'_, '_> {
    fn first(&mut self, value: &QValue) -> Result<bool, ExecError> {
        let Some(id) = totals_payload_id(value)? else {
            return Ok(true);
        };
        if self.seen.contains(&id) {
            return Ok(false);
        }
        self.mem.grow(self.model.growth())?;
        self.seen.insert(id);
        self.mem.shrink(self.model.inserted());
        Ok(true)
    }
}

impl Drop for TotalsPayloads<'_, '_> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.seen));
        self.mem.shrink(self.model.table());
    }
}

struct TotalsSizeWork<'a> {
    remaining: std::cell::Cell<u64>,
    cancel: Option<&'a std::sync::atomic::AtomicBool>,
    stop: Option<&'a std::sync::atomic::AtomicBool>,
}

impl<'a> TotalsSizeWork<'a> {
    fn new(env: Option<&crate::source::ExecEnv<'a>>) -> Self {
        Self {
            remaining: std::cell::Cell::new(1 << 20),
            cancel: env.map(|env| env.cancel),
            stop: env.and_then(|env| env.stop),
        }
    }

    fn step(&self) -> Result<(), ExecError> {
        use std::sync::atomic::Ordering;
        if self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
            || self.stop.is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(ExecError::Fatal("выполнение прервано".to_string()));
        }
        let remaining = self
            .remaining
            .get()
            .checked_sub(1)
            .ok_or_else(invalid_size)?;
        self.remaining.set(remaining);
        Ok(())
    }
}

fn totals_spec_capacity(
    spec: &query_ir::totals::TotalsSpec,
    copy: bool,
    shared: bool,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    use query_ir::totals::*;
    let mut cancel = LedgerWork { env, ticks: 0 };
    cancel.check()?;
    let mut payloads = TotalsPayloads {
        seen: std::collections::HashSet::new(),
        model: HashModel::new(std::mem::size_of::<(u8, usize, usize)>() as u64),
        mem,
    };
    let work = TotalsSizeWork::new(env);
    work.step()?;
    visit_totals_values(spec, &work, &mut |_| true, mem, &mut cancel)?;
    let cap = |len, capacity| if copy { len } else { capacity };
    let mut bytes = size_vec(
        cap(spec.points.len(), spec.points.capacity()),
        std::mem::size_of::<PointSpec>(),
    )?;
    for point in &spec.points {
        bytes = size_add(
            bytes,
            size_block(cap(point.name.len(), point.name.capacity()) as u64)?,
        )?;
        bytes = size_add(
            bytes,
            size_vec(
                cap(point.hier.len(), point.hier.capacity()),
                std::mem::size_of::<HierTableSpec>(),
            )?,
        )?;
        for hier in &point.hier {
            bytes = size_add(
                bytes,
                size_vec(
                    cap(hier.keys.len(), hier.keys.capacity()),
                    std::mem::size_of::<Option<(ibschema::storage::FieldId, bool)>>(),
                )?,
            )?;
        }
    }
    bytes = size_add(
        bytes,
        size_vec(
            cap(spec.aggs.len(), spec.aggs.capacity()),
            std::mem::size_of::<AggSpec>(),
        )?,
    )?;
    for agg in &spec.aggs {
        if let AggArg::Const(v) = &agg.arg {
            bytes = size_add(
                bytes,
                totals_value_model(v, copy, shared, &work, &mut payloads)?,
            )?;
        }
    }
    bytes = size_add(
        bytes,
        size_vec(
            cap(spec.items.len(), spec.items.capacity()),
            std::mem::size_of::<(TotalsExpr, usize)>(),
        )?,
    )?;
    for (expr, _) in &spec.items {
        bytes = size_add(
            bytes,
            totals_expr_model(expr, copy, shared, &work, &mut payloads, &mut cancel)?,
        )?;
    }
    cancel.check()?;
    size_add(
        bytes,
        size_vec(
            cap(spec.order.len(), spec.order.capacity()),
            std::mem::size_of::<OrderKey>(),
        )?,
    )
}

pub(crate) struct SizingStack<'m, 'a, T> {
    items: Vec<T>,
    bytes: u64,
    mem: &'m crate::ops::MemBudget<'a>,
}

impl<'m, 'a, T> SizingStack<'m, 'a, T> {
    pub(crate) fn new(mem: &'m crate::ops::MemBudget<'a>) -> Self {
        Self {
            items: Vec::new(),
            bytes: 0,
            mem,
        }
    }

    pub(crate) fn push(&mut self, value: T) -> Result<(), ExecError> {
        if self.items.len() == self.items.capacity() {
            let cap = self
                .items
                .capacity()
                .checked_mul(2)
                .ok_or_else(invalid_size)?
                .max(4);
            std::alloc::Layout::array::<T>(cap).map_err(|_| invalid_size())?;
            let bytes = size_vec(cap, std::mem::size_of::<T>())?;
            self.mem.grow(bytes)?;
            let mut replacement = Vec::with_capacity(cap);
            replacement.append(&mut self.items);
            drop(std::mem::replace(&mut self.items, replacement));
            self.mem.shrink(self.bytes);
            self.bytes = bytes;
        }
        self.items.push(value);
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.items.pop()
    }
}

impl<T> Drop for SizingStack<'_, '_, T> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.items));
        self.mem.shrink(self.bytes);
    }
}

fn totals_expr_model(
    expr: &query_ir::totals::TotalsExpr,
    copy: bool,
    shared: bool,
    work: &TotalsSizeWork<'_>,
    payloads: &mut TotalsPayloads<'_, '_>,
    cancel: &mut LedgerWork<'_, '_>,
) -> Result<u64, ExecError> {
    use query_ir::totals::TotalsExpr as E;
    let mut pending = SizingStack::new(payloads.mem);
    pending.push(expr)?;
    let mut bytes = 0;
    let boxed = size_block(std::mem::size_of::<E>() as u64)?;
    while let Some(expr) = pending.pop() {
        cancel.step()?;
        match expr {
            E::Const(value) => {
                bytes = size_add(
                    bytes,
                    totals_value_model(value, copy, shared, work, payloads)?,
                )?
            }
            E::Neg(child) | E::Not(child) => {
                bytes = size_add(bytes, boxed)?;
                pending.push(child.as_ref())?;
            }
            E::Bin(_, a, b) | E::IsNull(a, b) | E::Cmp(_, a, b) => {
                bytes = size_add(bytes, size_add(boxed, boxed)?)?;
                pending.push(b.as_ref())?;
                pending.push(a.as_ref())?;
            }
            E::Case { whens, otherwise } => {
                bytes = size_add(
                    bytes,
                    size_vec(
                        if copy { whens.len() } else { whens.capacity() },
                        std::mem::size_of::<(E, E)>(),
                    )?,
                )?;
                if let Some(child) = otherwise {
                    bytes = size_add(bytes, boxed)?;
                    pending.push(child.as_ref())?;
                }
                for (a, b) in whens.iter().rev() {
                    cancel.step()?;
                    pending.push(b)?;
                    pending.push(a)?;
                }
            }
            E::And(values) | E::Or(values) => {
                bytes = size_add(
                    bytes,
                    size_vec(
                        if copy {
                            values.len()
                        } else {
                            values.capacity()
                        },
                        std::mem::size_of::<E>(),
                    )?,
                )?;
                for child in values.iter().rev() {
                    cancel.step()?;
                    pending.push(child)?;
                }
            }
            E::Agg(_) | E::Param(_) | E::Column(_) => {}
        }
    }
    Ok(bytes)
}

fn totals_value_model(
    value: &QValue,
    copy: bool,
    shared: bool,
    work: &TotalsSizeWork<'_>,
    payloads: &mut TotalsPayloads<'_, '_>,
) -> Result<u64, ExecError> {
    match value {
        QValue::String(_) | QValue::Binary(_) | QValue::Storage(_) | QValue::Nested(_) => {
            if !shared {
                return Ok(0);
            }
            if !payloads.first(value)? {
                return Ok(0);
            }
            match value {
                QValue::String(s) => {
                    totals_shared_backing(s.0.backing_layout().map_err(|_| invalid_size())?)
                }
                QValue::Binary(b) | QValue::Storage(b) => {
                    totals_shared_backing(b.backing_layout().map_err(|_| invalid_size())?)
                }
                QValue::Nested(n) => size_add(
                    totals_shared_backing(n.backing_layout().map_err(|_| invalid_size())?)?,
                    retained_totals_nested_model(n, work, payloads)?,
                ),
                _ => Ok(0),
            }
        }
        QValue::Number(n) => size_vec(n.backing_capacity_limbs(), std::mem::size_of::<u32>()),
        QValue::Type(t) => type_capacity(t, copy),
        QValue::PointInTime(_) => size_block(std::mem::size_of::<query_ir::value::Moment>() as u64),
        QValue::Boundary(b) => size_add(
            size_block(std::mem::size_of::<query_ir::value::Boundary>() as u64)?,
            totals_value_model(&b.point, copy, shared, work, payloads)?,
        ),
        _ => Ok(0),
    }
}

fn totals_payload_id(value: &QValue) -> Result<Option<(u8, usize, usize)>, ExecError> {
    let (tag, backing) = match value {
        QValue::String(s) => (0, s.0.backing_layout()),
        QValue::Binary(b) | QValue::Storage(b) => (1, b.backing_layout()),
        QValue::Nested(n) => (2, n.backing_layout()),
        _ => return Ok(None),
    };
    let backing = backing.map_err(|_| invalid_size())?;
    Ok(Some((
        tag + if backing.managed { 3 } else { 0 },
        backing.identity,
        0,
    )))
}

fn totals_shared_backing(backing: query_ir::value::SharedBacking) -> Result<u64, ExecError> {
    let node = size_block(u64::try_from(backing.node.size()).map_err(|_| invalid_size())?)?;
    size_add(
        node,
        match backing.buffer {
            Some(buffer) => size_block(u64::try_from(buffer.size()).map_err(|_| invalid_size())?)?,
            None => 0,
        },
    )
}

fn validate_totals_payload(
    value: &QValue,
    depth: usize,
    work: &TotalsSizeWork<'_>,
) -> Result<(), ExecError> {
    work.step()?;
    totals_size_depth(depth)?;
    match value {
        QValue::Boundary(boundary) => validate_totals_payload(&boundary.point, depth + 1, work)?,
        QValue::Nested(nested) => {
            retained_totals_columns_model(&nested.columns, depth + 1, work)?;
            for row in &nested.rows {
                work.step()?;
                for value in row {
                    validate_totals_payload(value, depth + 1, work)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn totals_payload_model(
    value: &QValue,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    let work = TotalsSizeWork::new(env);
    validate_totals_payload(value, 0, &work)?;
    let mut payloads = TotalsPayloads {
        seen: std::collections::HashSet::new(),
        model: HashModel::new(std::mem::size_of::<(u8, usize, usize)>() as u64),
        mem,
    };
    totals_value_model(value, false, true, &work, &mut payloads)
}

pub(crate) fn totals_row_model(
    row: &Vec<QValue>,
    mem: &crate::ops::MemBudget<'_>,
    env: Option<&crate::source::ExecEnv<'_>>,
) -> Result<u64, ExecError> {
    row.iter()
        .try_fold(size_vec(row.capacity(), VALUE_SIZE)?, |bytes, value| {
            size_add(bytes, totals_payload_model(value, mem, env)?)
        })
}

fn retained_totals_nested_model(
    nested: &NestedResult,
    work: &TotalsSizeWork<'_>,
    payloads: &mut TotalsPayloads<'_, '_>,
) -> Result<u64, ExecError> {
    work.step()?;
    let mut bytes = retained_totals_columns_model(&nested.columns, 0, work)?;
    bytes = size_add(
        bytes,
        size_vec(nested.rows.capacity(), std::mem::size_of::<Vec<QValue>>())?,
    )?;
    for row in &nested.rows {
        work.step()?;
        bytes = size_add(bytes, size_vec(row.capacity(), VALUE_SIZE)?)?;
        for value in row {
            bytes = size_add(
                bytes,
                totals_value_model(value, false, true, work, payloads)?,
            )?;
        }
    }
    Ok(bytes)
}

fn totals_size_depth(depth: usize) -> Result<(), ExecError> {
    if depth >= 128 {
        return Err(invalid_size());
    }
    Ok(())
}

fn retained_totals_columns_model(
    columns: &Vec<ColumnInfo>,
    depth: usize,
    work: &TotalsSizeWork<'_>,
) -> Result<u64, ExecError> {
    work.step()?;
    totals_size_depth(depth)?;
    let mut bytes = size_vec(columns.capacity(), std::mem::size_of::<ColumnInfo>())?;
    for column in columns {
        work.step()?;
        bytes = size_add(bytes, size_block(column.name.capacity() as u64)?)?;
        bytes = size_add(bytes, type_capacity(&column.ty, false)?)?;
        if let Some(nested) = &column.nested {
            bytes = size_add(
                bytes,
                retained_totals_columns_model(nested, depth + 1, work)?,
            )?;
        }
    }
    Ok(bytes)
}

fn visit_totals_values(
    spec: &query_ir::totals::TotalsSpec,
    work: &TotalsSizeWork<'_>,
    visit: &mut dyn FnMut(&QValue) -> bool,
    mem: &crate::ops::MemBudget<'_>,
    cancel: &mut LedgerWork<'_, '_>,
) -> Result<bool, ExecError> {
    use query_ir::totals::{AggArg, TotalsExpr as E};
    fn value(
        v: &QValue,
        depth: usize,
        work: &TotalsSizeWork<'_>,
        visit: &mut dyn FnMut(&QValue) -> bool,
    ) -> Result<bool, ExecError> {
        if depth > 0 || matches!(v, QValue::Nested(_) | QValue::Boundary(_)) {
            work.step()?;
        }
        totals_size_depth(depth)?;
        if !visit(v) {
            return Ok(false);
        }
        match v {
            QValue::Boundary(b) => value(&b.point, depth + 1, work, visit),
            QValue::Nested(n) => {
                retained_totals_columns_model(&n.columns, depth + 1, work)?;
                for row in &n.rows {
                    work.step()?;
                    for v in row {
                        if !value(v, depth + 1, work, visit)? {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            }
            _ => Ok(true),
        }
    }
    fn expr(
        root: &E,
        work: &TotalsSizeWork<'_>,
        visit: &mut dyn FnMut(&QValue) -> bool,
        mem: &crate::ops::MemBudget<'_>,
        cancel: &mut LedgerWork<'_, '_>,
    ) -> Result<bool, ExecError> {
        let mut pending = SizingStack::new(mem);
        pending.push(root)?;
        while let Some(expr) = pending.pop() {
            cancel.step()?;
            match expr {
                E::Const(v) => {
                    if !value(v, 0, work, visit)? {
                        return Ok(false);
                    }
                }
                E::Neg(e) | E::Not(e) => pending.push(e.as_ref())?,
                E::Bin(_, a, b) | E::IsNull(a, b) | E::Cmp(_, a, b) => {
                    pending.push(b.as_ref())?;
                    pending.push(a.as_ref())?;
                }
                E::Case { whens, otherwise } => {
                    if let Some(child) = otherwise {
                        pending.push(child.as_ref())?;
                    }
                    for (a, b) in whens.iter().rev() {
                        cancel.step()?;
                        pending.push(b)?;
                        pending.push(a)?;
                    }
                }
                E::And(values) | E::Or(values) => {
                    for child in values.iter().rev() {
                        cancel.step()?;
                        pending.push(child)?;
                    }
                }
                _ => {}
            }
        }
        Ok(true)
    }
    for agg in &spec.aggs {
        if let AggArg::Const(v) = &agg.arg
            && !value(v, 0, work, visit)?
        {
            return Ok(false);
        }
    }
    for (e, _) in &spec.items {
        if !expr(e, work, visit, mem, cancel)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn column_model(c: &ColumnInfo) -> u64 {
    block(c.name.capacity() as u64)
        + type_model(&c.ty)
        + c.nested.as_ref().map_or(0, |n| columns_model(n))
}

pub fn columns_model(cs: &[ColumnInfo]) -> u64 {
    vec_block(cs.len(), std::mem::size_of::<ColumnInfo>())
        + cs.iter().map(column_model).sum::<u64>()
}

pub fn nested_model(n: &NestedResult) -> u64 {
    block(16 + std::mem::size_of::<NestedResult>() as u64)
        + columns_model(&n.columns)
        + rows_model(&n.rows)
}

pub fn row_model(r: &[QValue]) -> u64 {
    vec_block(r.len(), VALUE_SIZE) + r.iter().map(value_model).sum::<u64>()
}

pub fn row_cap_model(r: &Vec<QValue>) -> u64 {
    vec_block(r.capacity(), VALUE_SIZE) + r.iter().map(value_model).sum::<u64>()
}

pub fn rows_model(rows: &Vec<Vec<QValue>>) -> u64 {
    let mut seen = Payloads::default();
    vec_block(rows.capacity(), std::mem::size_of::<Vec<QValue>>())
        + rows
            .iter()
            .map(|r| {
                vec_block(r.capacity(), VALUE_SIZE)
                    + r.iter().map(|v| seen.value_model(v)).sum::<u64>()
            })
            .sum::<u64>()
}

fn identity_table_bytes<T>(count: usize) -> Result<u64, ExecError> {
    let buckets = if count == 0 {
        0
    } else if count < 4 {
        4
    } else if count < 8 {
        8
    } else {
        count
            .checked_mul(8)
            .and_then(|n| (n / 7).checked_next_power_of_two())
            .ok_or_else(invalid_size)?
    };
    let slots = std::alloc::Layout::array::<T>(buckets).map_err(|_| invalid_size())?;
    if slots.align() > 16 || 16 % slots.align() != 0 {
        return Err(invalid_size());
    }
    let bytes = buckets
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| n.checked_add(buckets))
        .and_then(|n| n.checked_add(16))
        .ok_or_else(invalid_size)?;
    std::alloc::Layout::array::<u8>(bytes).map_err(|_| invalid_size())?;
    if buckets == 0 {
        Ok(0)
    } else {
        size_block(bytes as u64)
    }
}

pub(crate) struct LedgerWork<'e, 'a> {
    env: Option<&'e crate::source::ExecEnv<'a>>,
    ticks: u64,
}

impl<'e, 'a> LedgerWork<'e, 'a> {
    pub(crate) fn new(env: &'e crate::source::ExecEnv<'a>) -> Self {
        Self {
            env: Some(env),
            ticks: 0,
        }
    }

    pub(crate) fn check(&self) -> Result<(), ExecError> {
        use std::sync::atomic::Ordering;
        if self.env.is_some_and(|env| {
            env.cancel.load(Ordering::Relaxed)
                || env.stop.is_some_and(|stop| stop.load(Ordering::Relaxed))
        }) {
            return Err(ExecError::Fatal("выполнение прервано".to_string()));
        }
        Ok(())
    }

    pub(crate) fn step(&mut self) -> Result<(), ExecError> {
        crate::probe::hit();
        let tick = self.ticks;
        self.ticks = tick.wrapping_add(1);
        if tick.is_multiple_of(1024) {
            self.check()?;
        }
        Ok(())
    }
}

fn row_id_count(rows: &[Vec<QValue>], work: &mut LedgerWork<'_, '_>) -> Result<usize, ExecError> {
    rows.iter().flatten().try_fold(0usize, |count, value| {
        work.step()?;
        count
            .checked_add(usize::from(payload_id(value).is_some()))
            .ok_or_else(invalid_size)
    })
}

fn admitted_rows_model(
    rows: &Vec<Vec<QValue>>,
    admission: &mut dyn FnMut(u64, bool) -> Result<(), ExecError>,
    work: &mut LedgerWork<'_, '_>,
) -> Result<u64, ExecError> {
    let count = row_id_count(rows, work)?;
    let table = identity_table_bytes::<(usize, usize)>(count)?;
    admission(table, true)?;
    let result = {
        let mut seen = std::collections::HashSet::with_capacity(count);
        let mut size = || -> Result<u64, ExecError> {
            let mut bytes = size_vec(rows.capacity(), std::mem::size_of::<Vec<QValue>>())?;
            for row in rows {
                work.step()?;
                bytes = size_add(bytes, size_vec(row.capacity(), VALUE_SIZE)?)?;
                for value in row {
                    work.step()?;
                    if payload_id(value).is_none_or(|id| seen.insert(id)) {
                        bytes = size_add(bytes, admitted_value_model(value, admission, work)?)?;
                    }
                }
            }
            Ok(bytes)
        };
        size()
    };
    admission(table, false)?;
    result
}

fn admitted_row_model(
    row: &[QValue],
    admission: &mut dyn FnMut(u64, bool) -> Result<(), ExecError>,
    work: &mut LedgerWork<'_, '_>,
) -> Result<u64, ExecError> {
    row.iter()
        .try_fold(size_vec(row.len(), VALUE_SIZE)?, |bytes, value| {
            work.step()?;
            size_add(bytes, admitted_value_model(value, admission, work)?)
        })
}

fn admitted_value_model(
    value: &QValue,
    admission: &mut dyn FnMut(u64, bool) -> Result<(), ExecError>,
    work: &mut LedgerWork<'_, '_>,
) -> Result<u64, ExecError> {
    match value {
        QValue::Nested(n) => size_add(
            size_add(
                block(16 + std::mem::size_of::<NestedResult>() as u64),
                columns_model(&n.columns),
            )?,
            admitted_rows_model(&n.rows, admission, work)?,
        ),
        QValue::Boundary(b) => size_add(
            block(std::mem::size_of::<query_ir::value::Boundary>() as u64),
            admitted_value_model(&b.point, admission, work)?,
        ),
        _ => Ok(value_model(value)),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RowCharge {
    direct: u64,
    pub(crate) new_row: usize,
}

#[derive(Clone, Copy, Debug)]
struct PayloadCharge {
    references: usize,
    bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RowLedger {
    pub(crate) rows: Vec<RowCharge>,
    payloads: std::collections::HashMap<(usize, usize), PayloadCharge>,
    pub(crate) bytes: u64,
    row_heap: u64,
}

impl RowLedger {
    pub(crate) fn build_checked(
        rows: &[Vec<QValue>],
        leased: bool,
        admission: &mut dyn FnMut(u64, bool) -> Result<(), ExecError>,
        work: &mut LedgerWork<'_, '_>,
    ) -> Result<Self, ExecError> {
        let count = if leased { row_id_count(rows, work)? } else { 0 };
        let bytes = size_add(
            size_vec(rows.len(), std::mem::size_of::<RowCharge>())?,
            identity_table_bytes::<((usize, usize), PayloadCharge)>(count)?,
        )?;
        std::alloc::Layout::array::<RowCharge>(rows.len()).map_err(|_| invalid_size())?;
        admission(bytes, true)?;
        let mut ledger = Self {
            rows: Vec::with_capacity(rows.len()),
            payloads: std::collections::HashMap::with_capacity(count),
            bytes,
            row_heap: 0,
        };
        for row in rows {
            work.step()?;
            let mut direct = if leased {
                size_vec(row.capacity(), VALUE_SIZE)?
            } else {
                admitted_row_model(row, admission, work)?
            };
            if leased {
                for value in row {
                    work.step()?;
                    if let Some(id) = payload_id(value) {
                        if let Some(held) = ledger.payloads.get_mut(&id) {
                            held.references =
                                held.references.checked_add(1).ok_or_else(invalid_size)?;
                        } else {
                            let bytes = admitted_value_model(value, admission, work)?;
                            ledger.row_heap = size_add(ledger.row_heap, bytes)?;
                            ledger.payloads.insert(
                                id,
                                PayloadCharge {
                                    references: 1,
                                    bytes,
                                },
                            );
                        }
                    } else {
                        direct = size_add(direct, admitted_value_model(value, admission, work)?)?;
                    }
                }
            }
            ledger.row_heap = size_add(ledger.row_heap, direct)?;
            ledger.rows.push(RowCharge { direct, new_row: 0 });
        }
        Ok(ledger)
    }

    pub(crate) fn prepare_append(
        &self,
        rows: &[Vec<QValue>],
        leased: bool,
        admission: &mut dyn FnMut(u64, bool) -> Result<(), ExecError>,
        work: &mut LedgerWork<'_, '_>,
    ) -> Result<LedgerAppend, ExecError> {
        let mut delta = Self::build_checked(rows, leased, admission, work)?;
        let old_row_bytes = size_vec(self.rows.capacity(), std::mem::size_of::<RowCharge>())?;
        let old_map_bytes = self.bytes - old_row_bytes;
        let required = self
            .rows
            .len()
            .checked_add(delta.rows.len())
            .ok_or_else(invalid_size)?;
        let (replacement_rows, row_bytes) = if required > self.rows.capacity() {
            let cap = self
                .rows
                .capacity()
                .checked_mul(2)
                .ok_or_else(invalid_size)?
                .max(required)
                .max(4);
            std::alloc::Layout::array::<RowCharge>(cap).map_err(|_| invalid_size())?;
            let bytes = size_vec(cap, std::mem::size_of::<RowCharge>())?;
            admission(bytes, true)?;
            let mut replacement = Vec::with_capacity(cap);
            for charge in &self.rows {
                work.step()?;
                replacement.push(*charge);
            }
            (Some(replacement), bytes)
        } else {
            (None, old_row_bytes)
        };
        let mut missing = 0usize;
        let mut added_heap = delta.row_heap;
        for (id, charge) in &mut delta.payloads {
            work.step()?;
            if let Some(old) = self.payloads.get(id) {
                if old.references > 0 {
                    added_heap -= charge.bytes;
                }
                charge.references = old
                    .references
                    .checked_add(charge.references)
                    .ok_or_else(invalid_size)?;
            } else {
                missing = missing.checked_add(1).ok_or_else(invalid_size)?;
            }
        }
        let required = self
            .payloads
            .len()
            .checked_add(missing)
            .ok_or_else(invalid_size)?;
        let (replacement_payloads, map_bytes) = if required > self.payloads.capacity() {
            let cap = self
                .payloads
                .capacity()
                .checked_mul(2)
                .ok_or_else(invalid_size)?
                .max(required)
                .max(4);
            let bytes = identity_table_bytes::<((usize, usize), PayloadCharge)>(cap)?;
            admission(bytes, true)?;
            let mut replacement = std::collections::HashMap::with_capacity(cap);
            for (id, charge) in &self.payloads {
                work.step()?;
                replacement.insert(*id, *charge);
            }
            for (id, charge) in &delta.payloads {
                work.step()?;
                replacement.insert(*id, *charge);
            }
            (Some(replacement), bytes)
        } else {
            (None, old_map_bytes)
        };
        Ok(LedgerAppend {
            input_rows_bytes: if leased {
                delta.rows_model(rows.len())?
            } else {
                delta.row_heap
            },
            next_row_heap: size_add(self.row_heap, added_heap)?,
            retained_rows_bytes: added_heap,
            next_bytes: size_add(row_bytes, map_bytes)?,
            delta,
            replacement_rows,
            replacement_payloads,
        })
    }

    pub(crate) fn legacy_rows_model(&self) -> u64 {
        self.row_heap
    }

    pub(crate) fn rows_model(&self, capacity: usize) -> Result<u64, ExecError> {
        size_add(
            size_vec(capacity, std::mem::size_of::<Vec<QValue>>())?,
            self.row_heap,
        )
    }

    pub(crate) fn prepare_rollback(
        &mut self,
        rows: &[Vec<QValue>],
        remove: &mut dyn FnMut(usize) -> bool,
    ) -> u64 {
        let mut freed = 0;
        let mut kept = 0;
        for (index, (row, charge)) in rows.iter().zip(&mut self.rows).enumerate() {
            crate::probe::hit();
            if remove(index) {
                charge.new_row = usize::MAX;
                freed += charge.direct;
                for value in row {
                    crate::probe::hit();
                    if let Some(id) = payload_id(value) {
                        let dead = if let Some(held) = self.payloads.get_mut(&id) {
                            held.references -= 1;
                            if held.references == 0 {
                                freed += held.bytes;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if dead {
                            self.payloads.remove(&id);
                        }
                    }
                }
            } else {
                charge.new_row = kept;
                kept += 1;
            }
        }
        self.row_heap -= freed;
        freed
    }
}

pub(crate) struct LedgerAppend {
    delta: RowLedger,
    replacement_rows: Option<Vec<RowCharge>>,
    replacement_payloads: Option<std::collections::HashMap<(usize, usize), PayloadCharge>>,
    next_row_heap: u64,
    pub(crate) next_bytes: u64,
    pub(crate) input_rows_bytes: u64,
    pub(crate) retained_rows_bytes: u64,
}

impl LedgerAppend {
    pub(crate) fn input_model(
        &self,
        outer_capacity: usize,
        leased: bool,
    ) -> Result<u64, ExecError> {
        if leased {
            self.delta.rows_model(outer_capacity)
        } else {
            Ok(self.input_rows_bytes)
        }
    }

    pub(crate) fn commit(mut self, ledger: &mut RowLedger) {
        if let Some(mut replacement) = self.replacement_rows.take() {
            replacement.append(&mut self.delta.rows);
            drop(std::mem::replace(&mut ledger.rows, replacement));
        } else {
            ledger.rows.append(&mut self.delta.rows);
        }
        if let Some(replacement) = self.replacement_payloads.take() {
            drop(std::mem::replace(&mut ledger.payloads, replacement));
        } else {
            for (id, charge) in self.delta.payloads {
                ledger.payloads.insert(id, charge);
            }
        }
        ledger.bytes = self.next_bytes;
        ledger.row_heap = self.next_row_heap;
    }
}

pub fn rows_clone_model(rows: &[Vec<QValue>]) -> u64 {
    rows_clone_model_of(rows.len(), rows.iter())
}

pub fn rows_clone_model_of<'a>(n: usize, rows: impl Iterator<Item = &'a Vec<QValue>>) -> u64 {
    let mut seen = Payloads::default();
    vec_block(n, std::mem::size_of::<Vec<QValue>>())
        + rows
            .map(|r| {
                vec_block(r.len(), VALUE_SIZE) + r.iter().map(|v| seen.value_model(v)).sum::<u64>()
            })
            .sum::<u64>()
}

fn payload_id(v: &QValue) -> Option<(usize, usize)> {
    match v {
        QValue::String(s) if !s.0.is_empty() => Some((s.0.as_ptr() as usize, s.0.len())),
        QValue::Binary(b) | QValue::Storage(b) if !b.is_empty() => {
            Some((b.as_ptr() as usize, b.len()))
        }
        QValue::Nested(n) => {
            let r: &NestedResult = n;
            Some((std::ptr::from_ref(r) as usize, 0))
        }
        _ => None,
    }
}

#[derive(Default)]
pub struct Payloads {
    seen: std::collections::HashSet<(usize, usize)>,
    model: Option<HashModel>,
}

impl Payloads {
    pub fn value_model(&mut self, v: &QValue) -> u64 {
        match payload_id(v) {
            Some(id) if !self.seen.insert(id) => 0,
            _ => value_model(v),
        }
    }

    pub fn value_model_charged(
        &mut self,
        v: &QValue,
        charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
    ) -> Result<(u64, u64), ExecError> {
        let Some(id) = payload_id(v) else {
            return Ok((value_model(v), 0));
        };
        if self.seen.contains(&id) {
            return Ok((0, 0));
        }
        let model = self
            .model
            .get_or_insert_with(|| HashModel::new(std::mem::size_of::<(usize, usize)>() as u64));
        charge(model.growth())?;
        self.seen.insert(id);
        Ok((value_model(v), model.inserted()))
    }

    pub fn table(&self) -> u64 {
        self.model.as_ref().map_or(0, HashModel::table)
    }
}

pub fn row_cap_model_shared(
    r: &Vec<QValue>,
    seen: &mut Payloads,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<(u64, u64), ExecError> {
    let mut bytes = vec_block(r.capacity(), VALUE_SIZE);
    let mut release = 0;
    for v in r {
        let (b, old) = seen.value_model_charged(v, charge)?;
        bytes += b;
        release += old;
    }
    Ok((bytes, release))
}

pub fn key_model(k: &Key) -> u64 {
    block(k.0.capacity() as u64)
}

pub fn keys_model(ks: &[Key]) -> u64 {
    vec_block(ks.len(), std::mem::size_of::<Key>()) + ks.iter().map(key_model).sum::<u64>()
}

pub const PREFLIGHT_UNITS: usize = 512;

pub fn key_bound(v: &QValue) -> u64 {
    1 + match v {
        QValue::String(s) => 40 * crate::key::key_text(&s.0).len() as u64 + 64,
        QValue::Number(n) => 24 * n.size_hint_limbs() + 24,
        QValue::Date(_) => 8,
        QValue::Bool(_) => 1,
        QValue::Uuid(_) => 16,
        QValue::Ref { .. } => 20,
        QValue::Binary(b) | QValue::Storage(b) => b.len() as u64,
        QValue::Type(t) => 64 + 40 * (t.refs.len() + t.others.len()) as u64,
        QValue::SysEnum(..) => 6,
        QValue::PointInTime(_) => 30,
        QValue::Boundary(b) => 1 + key_bound(&b.point),
        QValue::Null | QValue::Undefined | QValue::Nested(_) => 0,
    }
}

pub fn key_cost(v: &QValue, coll: &dyn Collation) -> u64 {
    key_sizes(v, coll).1
}

pub fn charge_key(
    v: &QValue,
    coll: &dyn Collation,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    try_charge_key(v, ExecCollation::Legacy(coll), charge)
}

pub fn try_charge_key(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    let walk = walk_bytes(v);
    let (_, peak) = key_sizes_charged(v, coll, charge)?;
    charge(peak - walk)?;
    Ok(peak)
}

pub fn key_sizes_charged(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<(u64, u64), ExecError> {
    let walk = walk_bytes(v);
    if walk > 0 {
        charge(walk)?;
    }
    key_sizes_with(v, coll)
}

pub fn walk_bytes(v: &QValue) -> u64 {
    match v {
        QValue::String(s) => text_walk_bytes(crate::key::key_text(&s.0)),
        _ => 0,
    }
}

fn text_walk_bytes(text: &[u16]) -> u64 {
    if text.len() > PREFLIGHT_UNITS {
        block(2 * text.len() as u64 + 2)
    } else {
        0
    }
}

pub fn try_charge_totals_key(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    let QValue::String(s) = v else {
        return try_charge_key(v, coll, charge);
    };
    let walk = text_walk_bytes(&s.0);
    if walk > 0 {
        charge(walk)?;
    }
    let (_, peak) = text_key_sizes(&s.0, coll)?;
    charge(peak.saturating_sub(walk))?;
    Ok(peak.max(walk))
}

pub fn key_sizes(v: &QValue, coll: &dyn Collation) -> (u64, u64) {
    crate::coll::infallible(key_sizes_with(v, coll))
}

pub fn try_key_sizes(v: &QValue, coll: ExecCollation<'_>) -> Result<(u64, u64), ExecError> {
    key_sizes_with(v, coll)
}

fn key_sizes_with<C: Keys>(v: &QValue, coll: C) -> Result<(u64, u64), C::Error> {
    let QValue::String(s) = v else {
        let n = block(2 * key_bound(v));
        return Ok((n, n));
    };
    text_key_sizes(crate::key::key_text(&s.0), coll)
}

fn text_key_sizes<C: Keys>(text: &[u16], coll: C) -> Result<(u64, u64), C::Error> {
    let units = text.len() as u64;
    let bound = || {
        let peak = block(2 * (1 + 40 * units + 64));
        (block(37 * units + 48).min(peak), peak)
    };
    if text.len() <= PREFLIGHT_UNITS {
        return Ok(bound());
    }
    Ok(match coll.key_len(Strength::Secondary, text)? {
        Some(need) => (block(need as u64 + 1), icu_build_cost(units, need as u64)),
        None => bound(),
    })
}

fn icu_build_cost(units: u64, need: u64) -> u64 {
    let src = block(2 * units + 2);
    let first = block(4 * units + 32 + 4);
    let second = block(need + 4);
    src + block(need) + first.max(second)
}

pub fn vec_push_growth(len: usize, cap: usize, elem: usize) -> (u64, u64) {
    if len < cap {
        return (0, 0);
    }
    (
        vec_block(grown_cap(len, cap, elem), elem),
        vec_block(cap, elem),
    )
}

fn grown_cap(len: usize, cap: usize, elem: usize) -> usize {
    let min = if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    };
    (cap * 2).max(min).max(len + 1)
}

pub fn vec_pushes_peak(n: usize, elem: usize) -> u64 {
    let (mut cap, mut peak) = (0usize, 0u64);
    while cap < n {
        let (new, old) = vec_push_growth(cap, cap, elem);
        peak = peak.max(new + old);
        cap = grown_cap(cap, cap, elem);
    }
    peak
}

pub fn hash_table_model(buckets: usize, kv: u64) -> u64 {
    if buckets == 0 {
        return 0;
    }
    block((buckets as u64).saturating_mul(kv) + buckets as u64 + 16)
}

pub fn hash_buckets_for(cap: usize) -> usize {
    if cap == 0 {
        0
    } else if cap < 4 {
        4
    } else if cap < 8 {
        8
    } else {
        (cap * 8 / 7).next_power_of_two()
    }
}

pub fn hash_capacity(buckets: usize) -> usize {
    if buckets < 8 {
        buckets.saturating_sub(1)
    } else {
        buckets / 8 * 7
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HashModel {
    len: usize,
    buckets: usize,
    kv: u64,
}

impl HashModel {
    pub fn new(kv: u64) -> HashModel {
        HashModel {
            len: 0,
            buckets: 0,
            kv,
        }
    }

    pub fn growth(&self) -> u64 {
        if self.len < hash_capacity(self.buckets) {
            return 0;
        }
        hash_table_model(self.next_buckets(), self.kv)
    }

    fn next_buckets(&self) -> usize {
        if self.buckets == 0 {
            hash_buckets_for(1)
        } else {
            (self.buckets * 2).max(hash_buckets_for(self.len + 1))
        }
    }

    pub fn inserted(&mut self) -> u64 {
        let freed = if self.len < hash_capacity(self.buckets) {
            0
        } else {
            let old = hash_table_model(self.buckets, self.kv);
            self.buckets = self.next_buckets();
            old
        };
        self.len += 1;
        freed
    }

    pub fn table(&self) -> u64 {
        hash_table_model(self.buckets, self.kv)
    }
}

pub fn hash_inserts_peak(n: usize, kv: u64) -> u64 {
    let mut m = HashModel::new(kv);
    let mut peak = 0;
    while m.len < n {
        let grow = m.growth();
        if grow > 0 {
            peak = peak.max(grow + m.table());
            m.inserted();
        } else {
            m.len = hash_capacity(m.buckets).min(n);
        }
    }
    peak
}
