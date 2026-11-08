mod builder;
pub(crate) mod payload;
use builder::{
    BuilderHash, BuilderKeyMap, BuilderKeySet, BuilderOrderKeys, BuilderVec, stable_sort_paid,
};

use payload::{PaidValue, PaidVec};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use bsl_prim::{Collation, Strength};
use ibschema::storage::FieldId;
use query_ir::{ExecError, QValue};
use query_syntax::ast::CmpOp;

use crate::coll::{ExecCollation, Keys};
use crate::eval::try_compare;
use crate::key::{Key, try_key_of_totals};
use crate::model::{LedgerWork, SizingStack, VALUE_SIZE, vec_block};
use crate::ops::MemBudget;
use crate::source::{DataSource, ExecEnv, Row};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordType {
    Overall,
    GroupTotal,
    HierarchyTotal,
    Detail,
}

pub use query_ir::totals::{
    AggArg, AggFunc, AggSpec, ArithOp, HierTableSpec, HierarchyMode, KeySource, OrderKey,
    PeriodsSpec, PointSpec, TotalsExpr, TotalsSpec,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TotalsError {
    DivisionByZero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRef {
    Detail(u32),
    Total(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub kind: RecordType,
    pub grouping: Option<u16>,
    pub level: u32,
    pub row: RowRef,
    pub children: Range<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsTree {
    pub spec: TotalsSpec,
    pub records: Vec<Record>,
    pub links: Vec<u32>,
    pub root_links: Range<u32>,
    pub totals: Vec<Row>,
    pub max_number_limbs: u64,
    pub error: Option<TotalsError>,
}

impl TotalsTree {
    pub fn roots(&self) -> impl DoubleEndedIterator<Item = u32> + '_ {
        self.slice(self.root_links.clone()).iter().copied()
    }

    pub fn children(&self, id: u32) -> impl DoubleEndedIterator<Item = u32> + '_ {
        let r = self
            .records
            .get(id as usize)
            .map_or(0..0, |r| r.children.clone());
        self.slice(r).iter().copied()
    }

    fn slice(&self, r: Range<u32>) -> &[u32] {
        self.links
            .get(r.start as usize..r.end as usize)
            .unwrap_or(&[])
    }

    pub fn model(&self) -> u64 {
        self.model_charged(&MemBudget::new(u64::MAX), None)
            .unwrap_or(u64::MAX)
    }

    pub(crate) fn model_charged(
        &self,
        mem: &MemBudget<'_>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<u64, ExecError> {
        let backing = crate::model::size_add(
            crate::model::size_add(
                vec_block(self.records.capacity(), std::mem::size_of::<Record>()),
                vec_block(self.links.capacity(), std::mem::size_of::<u32>()),
            )?,
            vec_block(self.totals.capacity(), std::mem::size_of::<Row>()),
        )?;
        let rows = self.totals.iter().try_fold(backing, |bytes, row| {
            crate::model::size_add(bytes, crate::model::totals_row_model(row, mem, env)?)
        })?;
        crate::model::size_add(rows, crate::model::totals_spec_model(&self.spec, mem, env)?)
    }
}

pub fn compare_totals(a: &QValue, b: &QValue, coll: &dyn Collation) -> Ordering {
    crate::coll::infallible(compare_totals_with(a, b, coll))
}

pub fn try_compare_totals(
    a: &QValue,
    b: &QValue,
    coll: ExecCollation<'_>,
) -> Result<Ordering, ExecError> {
    compare_totals_with(a, b, coll)
}

fn compare_totals_with<C: Keys>(a: &QValue, b: &QValue, coll: C) -> Result<Ordering, C::Error> {
    match (a, b) {
        (QValue::String(x), QValue::String(y)) => Ok(coll
            .key(Strength::Secondary, &x.0)?
            .cmp(&coll.key(Strength::Secondary, &y.0)?)),
        _ => crate::eval::compare_cmp_with(a, b, coll),
    }
}

pub(crate) fn compare_keys(a: &[Key], b: &[Key], order: &[OrderKey]) -> Ordering {
    for (k, (x, y)) in a.iter().zip(b).enumerate() {
        let o = x.cmp(y);
        let o = if order.get(k).is_some_and(|k| k.desc) {
            o.reverse()
        } else {
            o
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

pub(crate) fn record_keys(
    order: &[OrderKey],
    i: usize,
    row: &Row,
    first: Option<&Row>,
) -> Vec<QValue> {
    order
        .iter()
        .map(|k| match k.source {
            KeySource::Row(c) => row.get(c).cloned().unwrap_or(QValue::Null),
            KeySource::FirstRow { column, point } | KeySource::Point { column, point }
                if point <= i =>
            {
                first
                    .and_then(|r| r.get(column))
                    .cloned()
                    .unwrap_or(QValue::Null)
            }
            KeySource::FirstRow { .. } | KeySource::Point { .. } | KeySource::Null => QValue::Null,
        })
        .collect()
}

fn record_key_value<'v>(
    key: &OrderKey,
    i: usize,
    row: &'v Row,
    first: Option<&'v Row>,
) -> &'v QValue {
    match key.source {
        KeySource::Row(column) => row.get(column).unwrap_or(&QValue::Null),
        KeySource::FirstRow { column, point } | KeySource::Point { column, point }
            if point <= i =>
        {
            first
                .and_then(|row| row.get(column))
                .unwrap_or(&QValue::Null)
        }
        _ => &QValue::Null,
    }
}

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q4" }
}

fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("totals: {what}"))
}

struct ScratchCharge<'m, 'a> {
    mem: &'m MemBudget<'a>,
    bytes: u64,
}

impl Drop for ScratchCharge<'_, '_> {
    fn drop(&mut self) {
        self.mem.shrink(self.bytes);
    }
}

fn prepaid_vec<T>(
    capacity: usize,
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
) -> Result<Vec<T>, ExecError> {
    std::alloc::Layout::array::<T>(capacity).map_err(|_| crate::model::invalid_size())?;
    let bytes = crate::model::size_vec(capacity, std::mem::size_of::<T>())?;
    LedgerWork::new(env).check()?;
    mem.grow(bytes)?;
    let mut charge = ScratchCharge { mem, bytes };
    #[cfg(test)]
    accounting_invariants::growth_checkpoint();
    LedgerWork::new(env).check()?;
    let values = Vec::with_capacity(capacity);
    charge.bytes = 0;
    Ok(values)
}

fn reserve_backing<T>(
    values: &mut Vec<T>,
    additional: usize,
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
) -> Result<(), ExecError> {
    LedgerWork::new(env).check()?;
    let required = values
        .len()
        .checked_add(additional)
        .ok_or_else(crate::model::invalid_size)?;
    if required <= values.capacity() {
        return Ok(());
    }
    let capacity = values
        .capacity()
        .checked_mul(2)
        .ok_or_else(crate::model::invalid_size)?
        .max(4)
        .max(required);
    let bytes = crate::model::size_vec(capacity, std::mem::size_of::<T>())?;
    let old_bytes = crate::model::size_vec(values.capacity(), std::mem::size_of::<T>())?;
    let mut charge = ScratchCharge { mem, bytes: 0 };
    let mut replacement = prepaid_vec(capacity, mem, env)?;
    charge.bytes = bytes;
    #[cfg(test)]
    builder_tests::checkpoint()?;
    let mut work = LedgerWork::new(env);
    for value in values.drain(..) {
        work.step()?;
        replacement.push(value);
        #[cfg(test)]
        builder_tests::checkpoint()?;
        #[cfg(test)]
        accounting_invariants::move_checkpoint();
    }
    work.check()?;
    #[cfg(test)]
    builder_tests::checkpoint()?;
    drop(std::mem::replace(values, replacement));
    mem.shrink(old_bytes);
    charge.bytes = 0;
    Ok(())
}

struct PaidRow<'m, 'a> {
    row: Row,
    mem: &'m MemBudget<'a>,
    backing: u64,
    payload: u64,
}

impl PaidRow<'_, '_> {
    fn set(
        &mut self,
        column: usize,
        value: PaidValue<'_, '_>,
        charges: &mut [u64],
    ) -> Result<(), ExecError> {
        if !value.is_owned_by(self.mem) {
            return Err(crate::model::invalid_size());
        }
        let Some(slot) = self.row.get_mut(column) else {
            return Ok(());
        };
        let old_charge = charges
            .get_mut(column)
            .ok_or_else(crate::model::invalid_size)?;
        let payload = self
            .payload
            .checked_sub(*old_charge)
            .and_then(|bytes| bytes.checked_add(value.bytes()))
            .ok_or_else(crate::model::invalid_size)?;
        self.mem
            .check_value(crate::model::size_add(self.backing, payload)?)?;
        let (value, bytes) = value.into_parts();
        self.payload = payload;
        drop(std::mem::replace(slot, value));
        self.mem.shrink(*old_charge);
        *old_charge = bytes;
        Ok(())
    }

    fn into_row(mut self) -> Row {
        self.backing = 0;
        self.payload = 0;
        std::mem::take(&mut self.row)
    }
}

impl std::ops::Deref for PaidRow<'_, '_> {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.row
    }
}

impl std::ops::DerefMut for PaidRow<'_, '_> {
    fn deref_mut(&mut self) -> &mut Row {
        &mut self.row
    }
}

impl Drop for PaidRow<'_, '_> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.row));
        self.mem.shrink(self.backing + self.payload);
    }
}

pub(crate) fn clone_spec(
    spec: &TotalsSpec,
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
) -> Result<TotalsSpec, ExecError> {
    let mut work = LedgerWork::new(env);
    clone_spec_with(spec, mem, env, &mut || work.step())
}

fn clone_spec_with(
    spec: &TotalsSpec,
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
    check: &mut impl FnMut() -> Result<(), ExecError>,
) -> Result<TotalsSpec, ExecError> {
    let bytes = crate::model::totals_spec_clone_scratch_model(spec, mem, Some(env))?;
    mem.grow(bytes)?;
    let scratch = ScratchCharge { mem, bytes };
    LedgerWork::new(env).check()?;
    let copy = spec.try_clone_with(check)?;
    drop(scratch);
    LedgerWork::new(env).check()?;
    Ok(copy)
}

fn all_row_ids<'m, 'a>(
    n: u32,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let mut all = BuilderVec::new(n as usize, mem, env)?;
    let mut work = LedgerWork::new(env);
    for r in 0..n {
        work.step()?;
        all.push(r, env)?;
    }
    work.check()?;
    Ok(all)
}

fn detail_row_ids<'m, 'a>(
    capacity: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    BuilderVec::new(capacity, mem, env)
}

fn overall_row_ids<'m, 'a>(
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
    emit: impl FnOnce() -> Result<u32, ExecError>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let mut out = BuilderVec::new(1, mem, env)?;
    out.push(emit()?, env)?;
    Ok(out)
}

fn group_stage<'m, 'a, T>(
    capacity: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, T>, ExecError> {
    BuilderVec::new(capacity, mem, env)
}

fn group_indices<'m, 'a>(
    len: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, usize>, ExecError> {
    let mut indices = BuilderVec::new(len, mem, env)?;
    let mut work = LedgerWork::new(env);
    for i in 0..len {
        work.step()?;
        indices.push(i, env)?;
    }
    work.check()?;
    Ok(indices)
}

fn reorder_groups<'m, 'a, T>(
    made: BuilderVec<'m, 'a, T>,
    idx: BuilderVec<'m, 'a, usize>,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, T>, ExecError> {
    let mut slots = BuilderVec::new(made.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    for value in made {
        work.step()?;
        slots.push(Some(value), env)?;
    }
    let mut out = BuilderVec::new(idx.len(), mem, env)?;
    for k in idx {
        work.step()?;
        if let Some(value) = slots.get_mut(k).and_then(Option::take) {
            out.push(value, env)?;
        }
    }
    work.check()?;
    Ok(out)
}

fn group_output_ids<'m, 'a>(
    capacity: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    BuilderVec::new(capacity, mem, env)
}

pub fn build(
    spec: TotalsSpec,
    rows: &[Row],
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    coll: ExecCollation<'_>,
    mem: &MemBudget<'_>,
) -> Result<TotalsTree, ExecError> {
    let n = u32::try_from(rows.len()).map_err(|_| fatal("too many rows"))?;
    let all = all_row_ids(n, mem, env)?;
    run(&spec, rows, Some(src), env, coll, mem, &all, &[], 0)
}

pub(crate) fn build_sub(
    spec: &TotalsSpec,
    rows: &[Row],
    subset: &[u32],
    ctx: &[(usize, QValue)],
    level: u32,
    coll: ExecCollation<'_>,
    limbs: u64,
) -> Result<TotalsTree, ExecError> {
    let cancel = AtomicBool::new(false);
    let env = ExecEnv {
        collation: &crate::coll::NotConsulted,
        native: None,
        cancel: &cancel,
        stop: None,
        mem_limit: u64::MAX,
        max_number_limbs: limbs,
        memory: None,
        lease: None,
        evictor: None,
    };
    let mem = MemBudget::new(u64::MAX);
    run(spec, rows, None, &env, coll, &mem, subset, ctx, level)
}

#[allow(clippy::too_many_arguments)]
fn run(
    spec: &TotalsSpec,
    rows: &[Row],
    src: Option<&dyn DataSource>,
    env: &ExecEnv<'_>,
    coll: ExecCollation<'_>,
    mem: &MemBudget<'_>,
    subset: &[u32],
    ctx: &[(usize, QValue)],
    level: u32,
) -> Result<TotalsTree, ExecError> {
    let known = spec.points.iter().all(|point| point.column.is_none());
    let (records, links, totals) = if known {
        let totals = if subset.is_empty() {
            0
        } else {
            spec.points.len()
        };
        let records = subset
            .len()
            .checked_add(totals)
            .ok_or_else(crate::model::invalid_size)?;
        u32::try_from(records).map_err(|_| fatal("too many records"))?;
        u32::try_from(totals).map_err(|_| fatal("too many records"))?;
        (
            prepaid_vec(records, mem, env)?,
            prepaid_vec(records, mem, env)?,
            prepaid_vec(totals, mem, env)?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let mut b = Builder {
        spec,
        rows,
        src,
        env,
        mem,
        coll,
        records,
        links,
        totals,
        ticks: 0,
        infos: BuilderHash::new(mem),
        div_zero: Cell::new(false),
    };
    let r = b
        .level(0, subset, ctx, level)
        .and_then(|roots| b.push_links(&roots));
    let error = b.div_zero.get().then_some(TotalsError::DivisionByZero);
    let (records, links, totals) = (b.records, b.links, b.totals);
    drop(b.infos);
    let roots = r?;
    let copy_charge = crate::model::totals_spec_copy_model(spec, mem, Some(env))?;
    mem.grow(copy_charge)?;
    let saved_spec = clone_spec(spec, mem, env)?;
    let retained = crate::model::totals_spec_model(&saved_spec, mem, Some(env))?;
    let _residual = copy_charge
        .checked_sub(retained)
        .ok_or_else(crate::model::invalid_size)?;
    Ok(TotalsTree {
        spec: saved_spec,
        records,
        links,
        root_links: roots,
        totals,
        max_number_limbs: env.max_number_limbs,
        error,
    })
}

struct NodeInfo<'m, 'a> {
    parent: Option<PaidValue<'m, 'a>>,
    folder: bool,
    keys: PaidVec<'m, 'a, Option<PaidValue<'m, 'a>>>,
}

struct Group<'m, 'a> {
    value: PaidValue<'m, 'a>,
    rows: BuilderVec<'m, 'a, u32>,
}

struct Node<'m, 'a> {
    value: PaidValue<'m, 'a>,
    parent: Option<usize>,
    children: BuilderVec<'m, 'a, usize>,
    group: Option<usize>,
    first: usize,
    depth: u32,
}

#[derive(Clone, Copy)]
enum Kid {
    Node(usize),
    Own(usize),
}

struct Builder<'b, 'e> {
    spec: &'b TotalsSpec,
    rows: &'b [Row],
    src: Option<&'b dyn DataSource>,
    env: &'b ExecEnv<'e>,
    mem: &'b MemBudget<'e>,
    coll: ExecCollation<'b>,
    records: Vec<Record>,
    links: Vec<u32>,
    totals: Vec<Row>,
    ticks: u64,
    infos: BuilderHash<'b, 'e, (u32, [u8; 16]), Option<NodeInfo<'b, 'e>>>,
    div_zero: Cell<bool>,
}

fn cell_of(rows: &[Row], r: u32, c: usize) -> Option<&QValue> {
    rows.get(r as usize).and_then(|x| x.get(c))
}

fn is_empty_ref(v: &QValue) -> bool {
    matches!(v, QValue::Ref { id, .. } if id.iter().all(|b| *b == 0))
}

impl<'b, 'e> Builder<'b, 'e> {
    fn tick(&mut self) -> Result<(), ExecError> {
        let t = self.ticks;
        self.ticks = t.wrapping_add(1);
        if t.is_multiple_of(1024)
            && (self.env.cancel.load(AtomicOrdering::Relaxed)
                || self
                    .env
                    .stop
                    .is_some_and(|f| f.load(AtomicOrdering::Relaxed)))
        {
            return Err(ExecError::Fatal("выполнение прервано".to_string()));
        }
        Ok(())
    }

    fn record_order_keys(
        &mut self,
        i: usize,
        row: &Row,
        first: Option<&Row>,
    ) -> Result<BuilderOrderKeys<'b, 'e>, ExecError> {
        let mut keys = BuilderOrderKeys::new(self.spec.order.len(), self.mem, self.env)?;
        for order in &self.spec.order {
            keys.push(record_key_value(order, i, row, first), self.coll, self.env)?;
        }
        Ok(keys)
    }

    fn push_links(&mut self, ids: &[u32]) -> Result<Range<u32>, ExecError> {
        let start = u32::try_from(self.links.len()).map_err(|_| fatal("too many records"))?;
        let required = self
            .links
            .len()
            .checked_add(ids.len())
            .ok_or_else(crate::model::invalid_size)?;
        let end = u32::try_from(required).map_err(|_| fatal("too many records"))?;
        reserve_backing(&mut self.links, ids.len(), self.mem, self.env)?;
        let mut work = LedgerWork::new(self.env);
        for &id in ids {
            work.step()?;
            self.links.push(id);
        }
        work.check()?;
        Ok(start..end)
    }

    fn push_record(
        &mut self,
        kind: RecordType,
        grouping: Option<usize>,
        level: u32,
        row: RowRef,
        children: &[u32],
    ) -> Result<u32, ExecError> {
        let id = u32::try_from(self.records.len()).map_err(|_| fatal("too many records"))?;
        let grouping = match grouping {
            Some(g) => Some(u16::try_from(g).map_err(|_| fatal("too many control points"))?),
            None => None,
        };
        let children = self.push_links(children)?;
        reserve_backing(&mut self.records, 1, self.mem, self.env)?;
        self.records.push(Record {
            kind,
            grouping,
            level,
            row,
            children,
        });
        Ok(id)
    }

    fn push_total(&mut self, row: PaidRow<'b, 'e>) -> Result<RowRef, ExecError> {
        let i = u32::try_from(self.totals.len()).map_err(|_| fatal("too many records"))?;
        reserve_backing(&mut self.totals, 1, self.mem, self.env)?;
        self.totals.push(row.into_row());
        Ok(RowRef::Total(i))
    }

    fn cell(&self, row: u32, col: usize) -> Result<PaidValue<'b, 'e>, ExecError> {
        PaidValue::copy(
            cell_of(self.rows, row, col).unwrap_or(&QValue::Null),
            self.mem,
            Some(self.env),
        )
    }

    fn first_row(&self, rows: &[u32]) -> Option<&'b Row> {
        rows.first().and_then(|r| self.rows.get(*r as usize))
    }

    fn level(
        &mut self,
        i: usize,
        rows: &[u32],
        ctx: &[(usize, QValue)],
        level: u32,
    ) -> Result<BuilderVec<'b, 'e, u32>, ExecError> {
        if rows.is_empty() {
            return BuilderVec::new(0, self.mem, self.env);
        }
        let spec = self.spec;
        let Some(point) = spec.points.get(i) else {
            let mut out = detail_row_ids(rows.len(), self.mem, self.env)?;
            for &r in rows {
                self.tick()?;
                out.push(
                    self.push_record(RecordType::Detail, None, level, RowRef::Detail(r), &[])?,
                    self.env,
                )?;
            }
            return Ok(out);
        };
        let Some(col) = point.column else {
            let row = self.total_row(rows, ctx)?;
            let children = self.level(i + 1, rows, ctx, level + 1)?;
            let row = self.push_total(row)?;
            return overall_row_ids(self.mem, self.env, || {
                self.push_record(RecordType::Overall, Some(i), level, row, &children)
            });
        };
        match point.hierarchy {
            HierarchyMode::No => {
                let groups = self.group(rows, |b, r| b.cell(r, col))?;
                let mut made = group_stage(groups.len(), self.mem, self.env)?;
                for g in groups {
                    let inner = payload::context(ctx, col, &g.value, self.mem, Some(self.env))?;
                    let row = self.total_row(&g.rows, &inner)?;
                    made.push((g, row), self.env)?;
                }
                if !spec.order.is_empty() {
                    let mut ordered = BuilderVec::new(made.len(), self.mem, self.env)?;
                    for (group, row) in &made {
                        ordered.push(
                            self.record_order_keys(i, row, self.first_row(&group.rows))?,
                            self.env,
                        )?;
                    }
                    let mut idx = group_indices(made.len(), self.mem, self.env)?;
                    stable_sort_paid(&mut idx, self.mem, self.env, |a, b| {
                        match (ordered.get(*a), ordered.get(*b)) {
                            (Some(x), Some(y)) => compare_keys(x, y, &spec.order),
                            _ => Ordering::Equal,
                        }
                    })?;
                    made = reorder_groups(made, idx, self.mem, self.env)?;
                }
                let mut out = group_output_ids(made.len(), self.mem, self.env)?;
                for (g, row) in made {
                    out.push(self.group_record(i, col, &g, row, ctx, level)?, self.env)?;
                }
                Ok(out)
            }
            HierarchyMode::All | HierarchyMode::Only => self.hierarchy(i, col, rows, ctx, level),
        }
    }

    fn group_record(
        &mut self,
        i: usize,
        col: usize,
        g: &Group<'b, 'e>,
        row: PaidRow<'b, 'e>,
        ctx: &[(usize, QValue)],
        level: u32,
    ) -> Result<u32, ExecError> {
        let inner = payload::context(ctx, col, &g.value, self.mem, Some(self.env))?;
        let children = self.level(i + 1, &g.rows, &inner, level + 1)?;
        let row = self.push_total(row)?;
        self.push_record(RecordType::GroupTotal, Some(i), level, row, &children)
    }

    fn group(
        &mut self,
        rows: &[u32],
        mut value: impl FnMut(&mut Self, u32) -> Result<PaidValue<'b, 'e>, ExecError>,
    ) -> Result<BuilderVec<'b, 'e, Group<'b, 'e>>, ExecError> {
        let mut index = BuilderKeyMap::<usize>::new(self.mem);
        let mut groups: BuilderVec<'b, 'e, Group<'b, 'e>> = BuilderVec::new(0, self.mem, self.env)?;
        for &r in rows {
            self.tick()?;
            let v = value(self, r)?;
            let mut key =
                crate::ops::hier::TotalsKey::new(&v, self.mem, self.coll, Some(self.env))?;
            if let Some(&g) = index.get(key.as_key()) {
                let group = groups.get_mut(g).ok_or_else(crate::model::invalid_size)?;
                group.rows.push(r, self.env)?;
            } else {
                key.retain()?;
                groups.reserve(1, self.env)?;
                let mut ids = BuilderVec::new(1, self.mem, self.env)?;
                ids.push(r, self.env)?;
                index.insert(key, groups.len())?;
                groups.push(
                    Group {
                        value: v,
                        rows: ids,
                    },
                    self.env,
                )?;
            }
        }
        Ok(groups)
    }

    fn table_of(&self, i: usize, v: &QValue) -> Option<&'b HierTableSpec> {
        match v {
            QValue::Ref { table, id } if id.iter().any(|b| *b != 0) => {
                let spec = self.spec;
                spec.points.get(i)?.hier.iter().find(|t| t.number == *table)
            }
            _ => None,
        }
    }

    fn empty_of_table(&self, i: usize, v: &QValue) -> bool {
        match v {
            QValue::Ref { table, .. } if is_empty_ref(v) => self
                .spec
                .points
                .get(i)
                .is_some_and(|p| p.hier.iter().any(|t| t.number == *table)),
            _ => false,
        }
    }

    fn info(&mut self, i: usize, v: &QValue) -> Result<Option<&NodeInfo<'b, 'e>>, ExecError> {
        let Some(table) = self.table_of(i, v) else {
            return Ok(None);
        };
        let QValue::Ref { table: number, id } = v else {
            return Ok(None);
        };
        let key = (*number, *id);
        if !self.infos.contains_key(&key) {
            let Some(source) = self.src else {
                return Err(later("переход к группировке с иерархией"));
            };
            self.tick()?;
            let fields = hierarchy_fields(table, self.mem, self.env)?;
            let row = source.get_by_id_fields(table.table, v, &fields)?;
            let info = if let Some(row) = row {
                let at = |field: FieldId| row.get(usize::from(field.0)).unwrap_or(&QValue::Null);
                let parent = match at(table.parent) {
                    value @ QValue::Ref { .. } if !is_empty_ref(value) => {
                        Some(PaidValue::copy(value, self.mem, Some(self.env))?)
                    }
                    _ => None,
                };
                let folder = table
                    .folder
                    .is_some_and(|field| *at(field) == QValue::Bool(false));
                let mut keys = PaidVec::new(self.spec.order.len(), self.mem, Some(self.env))?;
                for index in 0..self.spec.order.len() {
                    let value = match table.keys.get(index).copied().flatten() {
                        Some((field, invert)) => {
                            let value = at(field);
                            let inverted;
                            let value = if let QValue::Bool(value) = value
                                && invert
                            {
                                inverted = QValue::Bool(!*value);
                                &inverted
                            } else {
                                value
                            };
                            Some(PaidValue::copy(value, self.mem, Some(self.env))?)
                        }
                        None => None,
                    };
                    keys.push(value)?;
                }
                Some(NodeInfo {
                    parent,
                    folder,
                    keys,
                })
            } else {
                None
            };
            self.cache_info(key, info)?;
        }
        Ok(self.infos.get(&key).and_then(Option::as_ref))
    }

    fn cache_info(
        &mut self,
        key: (u32, [u8; 16]),
        info: Option<NodeInfo<'b, 'e>>,
    ) -> Result<(), ExecError> {
        self.infos.insert(key, info)
    }

    fn parents_among(
        &mut self,
        i: usize,
        col: usize,
        rows: &[u32],
    ) -> Result<BuilderKeySet<'b, 'e>, ExecError> {
        let mut seen = BuilderKeySet::new(self.mem);
        let mut out = BuilderKeySet::new(self.mem);
        for &r in rows {
            self.tick()?;
            let v = self.cell(r, col)?;
            if !seen.insert(&v, self.coll, Some(self.env))? {
                continue;
            }
            let coll = self.coll;
            let env = self.env;
            if let Some(p) = self.info(i, &v)?.and_then(|n| n.parent.as_ref()) {
                out.insert(p, coll, Some(env))?;
            }
        }
        Ok(out)
    }

    fn only_key(
        &mut self,
        i: usize,
        v: PaidValue<'b, 'e>,
        parents: &BuilderKeySet<'b, 'e>,
    ) -> Result<PaidValue<'b, 'e>, ExecError> {
        if self.empty_of_table(i, &v) {
            return Ok(v);
        }
        let Some(table) = self.table_of(i, &v) else {
            return Ok(PaidValue::null(self.mem));
        };
        let mem = self.mem;
        let env = self.env;
        let coll = self.coll;
        let Some(info) = self.info(i, &v)? else {
            return Ok(PaidValue::null(mem));
        };
        let parent_itself = match table.folder {
            Some(_) => info.folder,
            None => parents.contains(&v, coll, Some(env))?,
        };
        if parent_itself {
            return Ok(v);
        }
        match &info.parent {
            Some(parent) => PaidValue::copy(parent, mem, Some(env)),
            None => PaidValue::copy(
                &QValue::Ref {
                    table: table.number,
                    id: [0; 16],
                },
                mem,
                Some(env),
            ),
        }
    }

    fn node_keys(
        &mut self,
        i: usize,
        value: &QValue,
        row: &Row,
        rows: &[u32],
    ) -> Result<BuilderOrderKeys<'b, 'e>, ExecError> {
        if self.spec.order.is_empty() {
            return BuilderOrderKeys::new(0, self.mem, self.env);
        }
        let _ = self.info(i, value)?;
        let info = match value {
            QValue::Ref { table, id } => self.infos.get(&(*table, *id)).and_then(Option::as_ref),
            _ => None,
        };
        let spec = self.spec;
        let first = rows.first().and_then(|r| self.rows.get(*r as usize));
        let mut keys = BuilderOrderKeys::new(spec.order.len(), self.mem, self.env)?;
        let mut work = LedgerWork::new(self.env);
        for (index, order) in spec.order.iter().enumerate() {
            work.step()?;
            let mut input = record_key_value(order, i, row, first);
            if let KeySource::Point { point, .. } = order.source
                && point == i
            {
                input = value;
            }
            if let KeySource::FirstRow { point, .. } = order.source
                && point == i
                && let Some(field) = info
                    .and_then(|info| info.keys.get(index))
                    .and_then(Option::as_ref)
            {
                input = field;
            }
            keys.push(input, self.coll, self.env)?;
        }
        work.check()?;
        Ok(keys)
    }

    fn hierarchy(
        &mut self,
        i: usize,
        col: usize,
        rows: &[u32],
        ctx: &[(usize, QValue)],
        level: u32,
    ) -> Result<BuilderVec<'b, 'e, u32>, ExecError> {
        let only = self
            .spec
            .points
            .get(i)
            .is_some_and(|p| p.hierarchy == HierarchyMode::Only);
        let parents = if only {
            self.parents_among(i, col, rows)?
        } else {
            BuilderKeySet::new(self.mem)
        };
        let groups = self.group(rows, |b, r| {
            let v = b.cell(r, col)?;
            if only {
                b.only_key(i, v, &parents)
            } else {
                Ok(v)
            }
        })?;
        let mut nodes = self.nodes(i, &groups, only)?;
        let order = post_order(&nodes, self.mem, self.env)?;
        hierarchy_first_and_depth(&mut nodes, &order, self.env)?;
        let under = hierarchy_under(&nodes, &groups, &order, self.mem, self.env)?;
        let mut node_rows: BuilderVec<'b, 'e, Option<PaidRow<'b, 'e>>> =
            hierarchy_slots(nodes.len(), self.mem, self.env)?;
        let mut group_rows: BuilderVec<'b, 'e, Option<PaidRow<'b, 'e>>> =
            hierarchy_slots(groups.len(), self.mem, self.env)?;
        for k in 0..nodes.len() {
            let Some(node) = nodes.get(k) else { continue };
            let inner = payload::context(ctx, col, &node.value, self.mem, Some(self.env))?;
            if !node.children.is_empty() {
                let rows = hierarchy_total_rows(&under, k, self.mem, self.env)?;
                let row = self.total_row(&rows, &inner)?;
                if let Some(slot) = node_rows.get_mut(k) {
                    *slot = Some(row);
                }
            }
            if let Some(g) = node.group
                && let Some(group) = groups.get(g)
            {
                let row = self.total_row(&group.rows, &inner)?;
                if let Some(slot) = group_rows.get_mut(g) {
                    *slot = Some(row);
                }
            }
        }
        let sorted = !self.spec.order.is_empty();
        let mut node_keys: BuilderVec<'b, 'e, BuilderOrderKeys<'b, 'e>> =
            hierarchy_list(0, self.mem, self.env)?;
        let mut group_keys: BuilderVec<'b, 'e, BuilderOrderKeys<'b, 'e>> =
            hierarchy_list(0, self.mem, self.env)?;
        if sorted {
            node_keys = hierarchy_list(nodes.len(), self.mem, self.env)?;
            group_keys = hierarchy_list(groups.len(), self.mem, self.env)?;
            for k in 0..nodes.len() {
                let (value, group, is_hier) = match nodes.get(k) {
                    Some(n) => (&*n.value, n.group, !n.children.is_empty()),
                    None => continue,
                };
                let row_of_node = if is_hier {
                    node_rows.get(k).and_then(Option::as_ref)
                } else {
                    group.and_then(|g| group_rows.get(g).and_then(Option::as_ref))
                };
                let empty = Vec::new();
                let row_of_node = row_of_node.map_or(&empty, |row| &row.row);
                let rows = hierarchy_order_rows(&under, k, self.mem, self.env)?;
                let ks = self.node_keys(i, value, row_of_node, &rows)?;
                node_keys.push(ks, self.env)?;
            }
            for (g, group) in groups.iter().enumerate() {
                let empty = Vec::new();
                let row = group_rows
                    .get(g)
                    .and_then(Option::as_ref)
                    .map_or(&empty, |row| &row.row);
                let ks = self.node_keys(i, &group.value, row, &group.rows)?;
                group_keys.push(ks, self.env)?;
            }
        }
        let arranging = HierarchyOrder {
            nodes: &nodes,
            only,
            node_keys: &node_keys,
            group_keys: &group_keys,
            spec_order: &self.spec.order,
        };
        let mut roots = hierarchy_root_kids(&nodes, self.mem, self.env)?;
        hierarchy_arrange(&mut roots, &arranging, self.mem, self.env)?;
        let mut kids_of: BuilderVec<'b, 'e, BuilderVec<'b, 'e, Kid>> =
            hierarchy_list(nodes.len(), self.mem, self.env)?;
        for node in nodes.iter() {
            let mut kids = hierarchy_child_kids(node, self.mem, self.env)?;
            if !node.children.is_empty() {
                hierarchy_arrange(&mut kids, &arranging, self.mem, self.env)?;
            }
            kids_of.push(kids, self.env)?;
        }
        let mut ids: BuilderVec<'b, 'e, Option<u32>> =
            hierarchy_slots(nodes.len(), self.mem, self.env)?;
        for &k in &order {
            let Some(node) = nodes.get(k) else { continue };
            let lvl = level + node.depth;
            let group = node.group.and_then(|g| groups.get(g));
            let id = if node.children.is_empty() {
                let (Some(g), Some(gi)) = (group, node.group) else {
                    continue;
                };
                let row = group_rows
                    .get_mut(gi)
                    .and_then(Option::take)
                    .ok_or_else(|| fatal("missing total row"))?;
                self.group_record(i, col, g, row, ctx, lvl)?
            } else {
                let children = hierarchy_child_ids(
                    kids_of.get(k).map_or(&[], |kids| kids.as_slice()),
                    self.mem,
                    self.env,
                    |kid| match kid {
                        Kid::Own(gi) => {
                            if let Some(g) = groups.get(gi) {
                                let row = group_rows
                                    .get_mut(gi)
                                    .and_then(Option::take)
                                    .ok_or_else(|| fatal("missing total row"))?;
                                return self.group_record(i, col, g, row, ctx, lvl + 1).map(Some);
                            }
                            Ok(None)
                        }
                        Kid::Node(c) => Ok(ids.get(c).copied().flatten()),
                    },
                )?;
                let row = node_rows
                    .get_mut(k)
                    .and_then(Option::take)
                    .ok_or_else(|| fatal("missing total row"))?;
                let row = self.push_total(row)?;
                self.push_record(RecordType::HierarchyTotal, Some(i), lvl, row, &children)?
            };
            if let Some(slot) = ids.get_mut(k) {
                *slot = Some(id);
            }
        }
        nodes.clear();
        hierarchy_root_ids(roots, &ids, self.mem, self.env)
    }

    fn nodes(
        &mut self,
        i: usize,
        groups: &[Group<'b, 'e>],
        only: bool,
    ) -> Result<BuilderVec<'b, 'e, Node<'b, 'e>>, ExecError> {
        let mut nodes: BuilderVec<'b, 'e, Node<'b, 'e>> = BuilderVec::new(0, self.mem, self.env)?;
        let mut index = BuilderKeyMap::<usize>::new(self.mem);
        for (gi, g) in groups.iter().enumerate() {
            let at = {
                let mut key = crate::ops::hier::TotalsKey::new(
                    &g.value,
                    self.mem,
                    self.coll,
                    Some(self.env),
                )?;
                if let Some(&n) = index.get(key.as_key()) {
                    n
                } else {
                    key.retain()?;
                    nodes.reserve(1, self.env)?;
                    let value = PaidValue::copy(&g.value, self.mem, Some(self.env))?;
                    let children = BuilderVec::new(0, self.mem, self.env)?;
                    let at = nodes.len();
                    index.insert(key, at)?;
                    nodes.push(
                        Node {
                            value,
                            parent: None,
                            children,
                            group: None,
                            first: gi,
                            depth: 0,
                        },
                        self.env,
                    )?;
                    at
                }
            };
            if let Some(node) = nodes.get_mut(at) {
                node.group = Some(gi);
            }
            let mut cur = at;
            loop {
                self.tick()?;
                let v = match nodes.get(cur) {
                    Some(n) if n.parent.is_none() => &*n.value,
                    _ => break,
                };
                let mem = self.mem;
                let env = self.env;
                let coll = self.coll;
                let Some(info) = self.info(i, v)? else { break };
                let Some(parent) = info.parent.as_ref() else {
                    break;
                };
                let parent = PaidValue::copy(parent, mem, Some(env))?;
                let mut key = crate::ops::hier::TotalsKey::new(&parent, mem, coll, Some(env))?;
                if let Some(&p) = index.get(key.as_key()) {
                    if !is_ancestor_or_self(&nodes, cur, p)
                        && let Some(node) = nodes.get_mut(cur)
                    {
                        node.parent = Some(p);
                    }
                    break;
                }
                key.retain()?;
                nodes.reserve(1, env)?;
                let children = BuilderVec::new(0, mem, env)?;
                let p = nodes.len();
                index.insert(key, p)?;
                nodes.push(
                    Node {
                        value: parent,
                        parent: None,
                        children,
                        group: None,
                        first: gi,
                        depth: 0,
                    },
                    env,
                )?;
                if let Some(node) = nodes.get_mut(cur) {
                    node.parent = Some(p);
                }
                cur = p;
            }
        }
        if only {
            let tables = self
                .spec
                .points
                .get(i)
                .map_or(&[][..], |p| p.hier.as_slice());
            let folders = hierarchy_folders(tables, self.mem, self.env)?;
            let empties = hierarchy_empties(&nodes, &folders, self.mem, self.env, |value| {
                self.empty_of_table(i, value)
            })?;
            hierarchy_link_empties(&mut nodes, empties, self.env)?;
        }
        hierarchy_children(&mut nodes, self.env)?;
        Ok(nodes)
    }

    fn total_row(
        &mut self,
        rows: &[u32],
        ctx: &[(usize, QValue)],
    ) -> Result<PaidRow<'b, 'e>, ExecError> {
        let backing = crate::model::size_vec(self.spec.width, VALUE_SIZE)?;
        self.mem.check_value(backing)?;
        let values = prepaid_vec(self.spec.width, self.mem, self.env)?;
        let mut row = PaidRow {
            row: values,
            mem: self.mem,
            backing,
            payload: 0,
        };
        row.resize(self.spec.width, QValue::Null);
        let mut charges = PaidVec::new(self.spec.width, self.mem, Some(self.env))?;
        for _ in 0..self.spec.width {
            charges.push(0u64)?;
        }
        let data = self.rows;
        let aggs = aggregate_paid(
            &self.spec.aggs,
            rows,
            |r, c| cell_of(data, r, c),
            self.coll,
            self.env.max_number_limbs,
            &self.div_zero,
            self.mem,
            Some(self.env),
        )?;
        for (column, value) in ctx {
            row.set(
                *column,
                PaidValue::copy(value, self.mem, Some(self.env))?,
                &mut charges,
            )?;
        }
        for (expression, column) in &self.spec.items {
            let value = eval_paid(
                expression,
                |index| aggs.get(index).map(|value| &**value),
                ctx,
                self.coll,
                self.env.max_number_limbs,
                &self.div_zero,
                self.mem,
                Some(self.env),
            )?;
            row.set(*column, value, &mut charges)?;
        }
        self.mem.check_value(crate::model::totals_row_model(
            &row,
            self.mem,
            Some(self.env),
        )?)?;
        Ok(row)
    }
}

pub(crate) fn with(ctx: &[(usize, QValue)], col: usize, v: &QValue) -> Vec<(usize, QValue)> {
    let mut inner = ctx.to_vec();
    inner.push((col, v.clone()));
    inner
}

pub(crate) fn total_values(
    spec: &TotalsSpec,
    aggs: &[QValue],
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
) -> Result<Row, ExecError> {
    let mem = MemBudget::new(u64::MAX);
    total_values_in(spec, aggs, ctx, coll, limbs, div_zero, &mem, None)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Preserve the six total_values inputs; work-memory and cancellation are explicit execution-only additions."
)]
fn total_values_in<'a>(
    spec: &TotalsSpec,
    aggs: &[QValue],
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<Row, ExecError> {
    let mut row = vec![QValue::Null; spec.width];
    fill_total_values_in(&mut row, spec, aggs, ctx, coll, limbs, div_zero, mem, env)?;
    Ok(row)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Fill either a caller-prepaid execution row or the existing walk row without changing expression inputs."
)]
fn fill_total_values_in<'a>(
    row: &mut Row,
    spec: &TotalsSpec,
    aggs: &[QValue],
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<(), ExecError> {
    for (c, v) in ctx {
        if let Some(slot) = row.get_mut(*c) {
            *slot = v.clone();
        }
    }
    for (e, c) in &spec.items {
        let v = eval(e, aggs, ctx, coll, limbs, div_zero, mem, env)?;
        if let Some(slot) = row.get_mut(*c) {
            *slot = v;
        }
    }
    Ok(())
}

pub(crate) fn aggregate<'v>(
    aggs: &'v [AggSpec],
    rows: &[u32],
    cell: impl Fn(u32, usize) -> Option<&'v QValue>,
    coll: ExecCollation<'_>,
    limbs: u64,
) -> Result<Vec<QValue>, ExecError> {
    let mem = MemBudget::new(u64::MAX);
    let div_zero = Cell::new(false);
    let mut paid = aggregate_paid(aggs, rows, cell, coll, limbs, &div_zero, &mem, None)?;
    let mut result = Vec::with_capacity(paid.len());
    for value in paid.drain(..) {
        result.push(value.into_unbudgeted());
    }
    Ok(result)
}

struct TotalsDistinct<'m, 'a> {
    keys: HashSet<Key>,
    model: crate::ops::hier::HierarchyHash<(Key, ())>,
    mem: &'m MemBudget<'a>,
    balance: u64,
}

impl<'m, 'a> TotalsDistinct<'m, 'a> {
    fn new(mem: &'m MemBudget<'a>) -> Self {
        #[cfg(test)]
        distinct_tests::snapshot(distinct_tests::Point::SetNew, mem, 0, 0);
        Self {
            keys: HashSet::new(),
            model: crate::ops::hier::HierarchyHash::new(),
            mem,
            balance: 0,
        }
    }

    fn insert(
        &mut self,
        value: &QValue,
        coll: ExecCollation<'_>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<bool, ExecError> {
        #[cfg(test)]
        distinct_tests::snapshot(
            distinct_tests::Point::InsertStart,
            self.mem,
            self.keys.len() as u64,
            self.keys.capacity() as u64,
        );
        let mut key = { crate::ops::hier::TotalsKey::new(value, self.mem, coll, env)? };
        if self.keys.contains(key.as_key()) {
            #[cfg(test)]
            distinct_tests::snapshot(
                distinct_tests::Point::Duplicate,
                self.mem,
                self.keys.len() as u64,
                self.keys.capacity() as u64,
            );
            return Ok(false);
        }
        key.retain()?;
        #[cfg(test)]
        {
            distinct_tests::snapshot(
                distinct_tests::Point::PrepareBefore,
                self.mem,
                self.keys.len() as u64,
                self.keys.capacity() as u64,
            );
            distinct_tests::fault(distinct_tests::Point::PrepareBefore)?;
        }
        let prepared = self.model.prepare(self.mem, &mut self.balance)?;
        #[cfg(test)]
        {
            distinct_tests::snapshot(
                distinct_tests::Point::Prepared,
                self.mem,
                prepared.0 as u64,
                prepared.1,
            );
            distinct_tests::fault(distinct_tests::Point::Prepared)?;
        }
        let next = crate::model::size_add(self.balance, key.bytes())?;
        let key = key.transfer(&mut self.balance, next);
        self.keys.insert(key);
        #[cfg(test)]
        {
            distinct_tests::snapshot(
                distinct_tests::Point::NativeInserted,
                self.mem,
                self.keys.len() as u64,
                self.keys.capacity() as u64,
            );
            distinct_tests::fault(distinct_tests::Point::NativeInserted)?;
        }
        self.model
            .inserted(prepared, self.keys.capacity(), self.mem, &mut self.balance)?;
        #[cfg(test)]
        {
            distinct_tests::snapshot(
                distinct_tests::Point::InsertedAfter,
                self.mem,
                self.keys.len() as u64,
                self.keys.capacity() as u64,
            );
            distinct_tests::fault(distinct_tests::Point::InsertedAfter)?;
        }
        Ok(true)
    }
}

impl Drop for TotalsDistinct<'_, '_> {
    fn drop(&mut self) {
        #[cfg(test)]
        distinct_tests::snapshot(
            distinct_tests::Point::SetDropBegin,
            self.mem,
            self.balance,
            0,
        );
        drop(std::mem::take(&mut self.keys));
        #[cfg(test)]
        distinct_tests::snapshot(
            distinct_tests::Point::SetDestroyed,
            self.mem,
            self.balance,
            0,
        );
        self.mem.shrink(std::mem::take(&mut self.balance));
        #[cfg(test)]
        distinct_tests::snapshot(distinct_tests::Point::SetDropEnd, self.mem, 0, 0);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Aggregate inputs and payload ownership share the explicit execution context."
)]
fn aggregate_paid<'v, 'm, 'a>(
    aggs: &'v [AggSpec],
    rows: &[u32],
    cell: impl Fn(u32, usize) -> Option<&'v QValue>,
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidVec<'m, 'a, PaidValue<'m, 'a>>, ExecError> {
    let mut out = PaidVec::new(aggs.len(), mem, env)?;
    for agg in aggs {
        payload::checkpoint(env)?;
        let get = |r| match &agg.arg {
            AggArg::Column(column) => cell(r, *column).filter(|value| **value != QValue::Null),
            AggArg::Const(value) => Some(value).filter(|value| **value != QValue::Null),
            AggArg::Star => None,
        };
        let value = match (agg.f, &agg.arg) {
            (AggFunc::Count, AggArg::Star) => payload::count(rows.len() as u64, mem, env)?,
            (_, AggArg::Star) => PaidValue::null(mem),
            (AggFunc::Count, _) => {
                let mut count = 0u64;
                let mut seen = agg.distinct.then(|| TotalsDistinct::new(mem));
                for &r in rows {
                    payload::checkpoint(env)?;
                    if let Some(value) = get(r) {
                        let unique = match &mut seen {
                            Some(seen) => seen.insert(value, coll, env)?,
                            None => true,
                        };
                        if !unique {
                            continue;
                        }
                        count = count
                            .checked_add(1)
                            .ok_or_else(crate::model::invalid_size)?;
                    }
                }
                #[cfg(test)]
                {
                    distinct_tests::snapshot(distinct_tests::Point::CountBefore, mem, 0, 0);
                    distinct_tests::fault(distinct_tests::Point::CountBefore)?;
                }
                let result = payload::count(count, mem, env)?;
                #[cfg(test)]
                {
                    distinct_tests::inline_result(mem, result.bytes());
                    distinct_tests::fault(distinct_tests::Point::CountResult)?;
                }
                result
            }
            (AggFunc::Sum | AggFunc::Avg, _) => {
                let mut sum: Option<PaidValue<'m, 'a>> = None;
                let mut seen = HashSet::new();
                let mut count = 0u64;
                for &r in rows {
                    payload::checkpoint(env)?;
                    let Some(value) = get(r) else { continue };
                    let QValue::Number(_) = value else {
                        return Err(later("ИТОГИ: СУММА/СРЕДНЕЕ не числа"));
                    };
                    if agg.distinct && !seen.insert(try_key_of_totals(value, coll)?) {
                        continue;
                    }
                    count = count
                        .checked_add(1)
                        .ok_or_else(crate::model::invalid_size)?;
                    let next = match &sum {
                        Some(sum) => {
                            payload::binary(ArithOp::Add, sum, value, limbs, div_zero, mem, env)?
                        }
                        None => PaidValue::copy(value, mem, env)?,
                    };
                    if let QValue::Number(number) = &*next
                        && number.size_hint_limbs() > limbs
                    {
                        return Err(ExecError::Fatal("превышен лимит размера числа".to_string()));
                    }
                    sum = Some(next);
                }
                match sum {
                    None => PaidValue::null(mem),
                    Some(sum) if agg.f == AggFunc::Avg => {
                        let count = payload::count(count, mem, env)?;
                        payload::binary(ArithOp::Div, &sum, &count, limbs, div_zero, mem, env)?
                    }
                    Some(sum) => sum,
                }
            }
            (AggFunc::Min | AggFunc::Max, _) => {
                let mut best: Option<&QValue> = None;
                for &r in rows {
                    payload::checkpoint(env)?;
                    let Some(value) = get(r) else { continue };
                    best = match best {
                        None => Some(value),
                        Some(current) => {
                            let ordering = try_compare_totals(value, current, coll)?;
                            let better = if agg.f == AggFunc::Min {
                                ordering == Ordering::Less
                            } else {
                                ordering == Ordering::Greater
                            };
                            Some(if better { value } else { current })
                        }
                    };
                }
                PaidValue::copy(best.unwrap_or(&QValue::Null), mem, env)?
            }
        };
        out.push(value)?;
    }
    payload::checkpoint(env)?;
    Ok(out)
}

enum EvalFrame<'e> {
    Visit(&'e TotalsExpr),
    Neg,
    Not,
    Bin(ArithOp),
    Cmp(CmpOp),
    IsNull(&'e TotalsExpr),
    CaseNext {
        whens: &'e [(TotalsExpr, TotalsExpr)],
        otherwise: Option<&'e TotalsExpr>,
        index: usize,
    },
    CaseAfter {
        value: &'e TotalsExpr,
        whens: &'e [(TotalsExpr, TotalsExpr)],
        otherwise: Option<&'e TotalsExpr>,
        next: usize,
    },
    BooleanNext {
        rest: &'e [TotalsExpr],
        unknown: bool,
        short: bool,
    },
    BooleanAfter {
        rest: &'e [TotalsExpr],
        unknown: bool,
        short: bool,
    },
}

fn eval_pop<'m, 'a>(
    values: &mut SizingStack<'m, 'a, PaidValue<'m, 'a>>,
) -> Result<PaidValue<'m, 'a>, ExecError> {
    values
        .pop()
        .ok_or_else(|| fatal("unbalanced total expression values"))
}

#[allow(
    clippy::too_many_arguments,
    reason = "Match total_values_in borrowed evaluation inputs, with explicit work-memory and optional cancellation contexts."
)]
pub(crate) fn eval<'a>(
    e: &TotalsExpr,
    aggs: &[QValue],
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<QValue, ExecError> {
    eval_paid(
        e,
        |index| aggs.get(index),
        ctx,
        coll,
        limbs,
        div_zero,
        mem,
        env,
    )
    .map(PaidValue::into_unbudgeted)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Preserve evaluation inputs with an explicit payload owner and budget."
)]
fn eval_paid<'v, 'm, 'a>(
    e: &TotalsExpr,
    aggs: impl Fn(usize) -> Option<&'v QValue>,
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
    mem: &'m MemBudget<'a>,
    env: Option<&ExecEnv<'a>>,
) -> Result<PaidValue<'m, 'a>, ExecError> {
    let mut frames = SizingStack::new(mem);
    let mut values = SizingStack::new(mem);
    let mut cancel = env.map(LedgerWork::new);
    frames.push(EvalFrame::Visit(e))?;
    while let Some(frame) = frames.pop() {
        if let Some(cancel) = &mut cancel {
            cancel.step()?;
        }
        match frame {
            EvalFrame::Visit(expression) => match expression {
                TotalsExpr::Agg(i) => values.push(PaidValue::copy(
                    aggs(*i).unwrap_or(&QValue::Null),
                    mem,
                    env,
                )?)?,
                TotalsExpr::Param(_) => return Err(fatal("an unbound parameter in a total")),
                TotalsExpr::Column(column) => values.push(PaidValue::copy(
                    ctx.iter()
                        .rev()
                        .find(|(key, _)| key == column)
                        .map_or(&QValue::Null, |(_, value)| value),
                    mem,
                    env,
                )?)?,
                TotalsExpr::Const(value) => values.push(PaidValue::copy(value, mem, env)?)?,
                TotalsExpr::Neg(child) | TotalsExpr::Not(child) => {
                    frames.push(if matches!(expression, TotalsExpr::Neg(_)) {
                        EvalFrame::Neg
                    } else {
                        EvalFrame::Not
                    })?;
                    frames.push(EvalFrame::Visit(child))?;
                }
                TotalsExpr::Bin(op, left, right) => {
                    frames.push(EvalFrame::Bin(*op))?;
                    frames.push(EvalFrame::Visit(right))?;
                    frames.push(EvalFrame::Visit(left))?;
                }
                TotalsExpr::Cmp(op, left, right) => {
                    frames.push(EvalFrame::Cmp(*op))?;
                    frames.push(EvalFrame::Visit(right))?;
                    frames.push(EvalFrame::Visit(left))?;
                }
                TotalsExpr::IsNull(left, right) => {
                    frames.push(EvalFrame::IsNull(right))?;
                    frames.push(EvalFrame::Visit(left))?;
                }
                TotalsExpr::Case { whens, otherwise } => frames.push(EvalFrame::CaseNext {
                    whens,
                    otherwise: otherwise.as_deref(),
                    index: 0,
                })?,
                TotalsExpr::And(rest) | TotalsExpr::Or(rest) => {
                    frames.push(EvalFrame::BooleanNext {
                        rest,
                        unknown: false,
                        short: matches!(expression, TotalsExpr::Or(_)),
                    })?
                }
            },
            EvalFrame::Neg => {
                let input = eval_pop(&mut values)?;
                values.push(payload::neg(&input, mem, env)?)?;
            }
            EvalFrame::Not => {
                let input = eval_pop(&mut values)?;
                let value = match &*input {
                    QValue::Bool(value) => QValue::Bool(!*value),
                    _ => QValue::Null,
                };
                values.push(PaidValue::copy(&value, mem, env)?)?;
            }
            EvalFrame::Bin(op) => {
                let right = eval_pop(&mut values)?;
                let left = eval_pop(&mut values)?;
                values.push(payload::binary(
                    op, &left, &right, limbs, div_zero, mem, env,
                )?)?;
            }
            EvalFrame::Cmp(op) => {
                let right = eval_pop(&mut values)?;
                let left = eval_pop(&mut values)?;
                values.push(PaidValue::copy(
                    &try_compare(op, &left, &right, coll)?.map_or(QValue::Null, QValue::Bool),
                    mem,
                    env,
                )?)?;
            }
            EvalFrame::IsNull(right) => {
                let value = eval_pop(&mut values)?;
                if matches!(&*value, QValue::Null) {
                    frames.push(EvalFrame::Visit(right))?;
                } else {
                    values.push(value)?;
                }
            }
            EvalFrame::CaseNext {
                whens,
                otherwise,
                index,
            } => {
                if let Some((condition, value)) = whens.get(index) {
                    frames.push(EvalFrame::CaseAfter {
                        value,
                        whens,
                        otherwise,
                        next: index + 1,
                    })?;
                    frames.push(EvalFrame::Visit(condition))?;
                } else if let Some(otherwise) = otherwise {
                    frames.push(EvalFrame::Visit(otherwise))?;
                } else {
                    values.push(PaidValue::null(mem))?;
                }
            }
            EvalFrame::CaseAfter {
                value,
                whens,
                otherwise,
                next,
            } => {
                if *eval_pop(&mut values)? == QValue::Bool(true) {
                    frames.push(EvalFrame::Visit(value))?;
                } else {
                    frames.push(EvalFrame::CaseNext {
                        whens,
                        otherwise,
                        index: next,
                    })?;
                }
            }
            EvalFrame::BooleanNext {
                rest,
                unknown,
                short,
            } => {
                if let Some((first, rest)) = rest.split_first() {
                    frames.push(EvalFrame::BooleanAfter {
                        rest,
                        unknown,
                        short,
                    })?;
                    frames.push(EvalFrame::Visit(first))?;
                } else {
                    values.push(PaidValue::copy(
                        &if unknown {
                            QValue::Null
                        } else {
                            QValue::Bool(!short)
                        },
                        mem,
                        env,
                    )?)?;
                }
            }
            EvalFrame::BooleanAfter {
                rest,
                unknown,
                short,
            } => {
                let value = eval_pop(&mut values)?;
                if *value == QValue::Bool(short) {
                    values.push(value)?;
                } else {
                    frames.push(EvalFrame::BooleanNext {
                        rest,
                        unknown: unknown || *value != QValue::Bool(!short),
                        short,
                    })?;
                }
            }
        }
    }
    if let Some(cancel) = cancel {
        cancel.check()?;
    }
    let result = eval_pop(&mut values)?;
    if values.pop().is_some() {
        return Err(fatal("unbalanced total expression values"));
    }
    Ok(result)
}

fn is_ancestor_or_self(nodes: &[Node<'_, '_>], a: usize, b: usize) -> bool {
    let mut cur = Some(b);
    let mut steps = 0usize;
    while let Some(c) = cur {
        if c == a {
            return true;
        }
        steps += 1;
        if steps > nodes.len() {
            return true;
        }
        cur = nodes.get(c).and_then(|n| n.parent);
    }
    false
}

fn hierarchy_under_slots<'m, 'a>(
    nodes: &[Node<'_, '_>],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, BuilderVec<'m, 'a, u32>>, ExecError> {
    let mut under = BuilderVec::new(nodes.len(), mem, env)?;
    for _ in nodes {
        under.push(BuilderVec::new(0, mem, env)?, env)?;
    }
    Ok(under)
}

fn hierarchy_slots<'m, 'a, T>(
    len: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, Option<T>>, ExecError> {
    let mut out = BuilderVec::new(len, mem, env)?;
    let mut work = LedgerWork::new(env);
    for _ in 0..len {
        work.step()?;
        out.push(None, env)?;
    }
    work.check()?;
    Ok(out)
}
fn hierarchy_list<'m, 'a, T>(
    capacity: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, T>, ExecError> {
    BuilderVec::new(capacity, mem, env)
}
fn hierarchy_root_kids<'m, 'a>(
    nodes: &[Node<'_, '_>],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, Kid>, ExecError> {
    let mut roots = BuilderVec::new(nodes.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    for (index, node) in nodes.iter().enumerate() {
        work.step()?;
        if node.parent.is_none() {
            roots.push(Kid::Node(index), env)?;
        }
    }
    work.check()?;
    Ok(roots)
}
fn hierarchy_child_kids<'m, 'a>(
    node: &Node<'_, '_>,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, Kid>, ExecError> {
    let capacity = if node.children.is_empty() {
        0
    } else {
        node.children
            .len()
            .checked_add(usize::from(node.group.is_some()))
            .ok_or_else(crate::model::invalid_size)?
    };
    let mut kids = BuilderVec::new(capacity, mem, env)?;
    let mut work = LedgerWork::new(env);
    if !node.children.is_empty() {
        if let Some(group) = node.group {
            kids.push(Kid::Own(group), env)?;
        }
        for child in node.children.iter() {
            work.step()?;
            kids.push(Kid::Node(*child), env)?;
        }
    }
    work.check()?;
    Ok(kids)
}
fn hierarchy_child_ids<'m, 'a>(
    kids: &[Kid],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
    mut emit: impl FnMut(Kid) -> Result<Option<u32>, ExecError>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let mut children = BuilderVec::new(kids.len(), mem, env)?;
    let copied = BuilderVec::copy_from(kids, mem, env)?;
    let mut work = LedgerWork::new(env);
    for kid in copied {
        work.step()?;
        if let Some(id) = emit(kid)? {
            children.push(id, env)?;
        }
    }
    work.check()?;
    Ok(children)
}
struct HierarchyOrder<'s, 'm, 'a> {
    nodes: &'s [Node<'m, 'a>],
    only: bool,
    node_keys: &'s [BuilderOrderKeys<'m, 'a>],
    group_keys: &'s [BuilderOrderKeys<'m, 'a>],
    spec_order: &'s [OrderKey],
}
fn hierarchy_arrange(
    kids: &mut [Kid],
    order: &HierarchyOrder<'_, '_, '_>,
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
) -> Result<(), ExecError> {
    let HierarchyOrder {
        nodes,
        only,
        node_keys,
        group_keys,
        spec_order,
    } = order;
    let kid_first = |kid: &Kid| match kid {
        Kid::Node(n) => nodes.get(*n).map_or(usize::MAX, |node| node.first),
        Kid::Own(group) => *group,
    };
    let kid_keys = |kid: &Kid| match kid {
        Kid::Node(n) => node_keys.get(*n).map(|keys| &keys[..]),
        Kid::Own(group) => group_keys.get(*group).map(|keys| &keys[..]),
    };
    stable_sort_paid(kids, mem, env, |a, b| {
        (*only && matches!(a, Kid::Own(_)), kid_first(a))
            .cmp(&(*only && matches!(b, Kid::Own(_)), kid_first(b)))
    })?;
    if !spec_order.is_empty() {
        stable_sort_paid(kids, mem, env, |a, b| match (kid_keys(a), kid_keys(b)) {
            (Some(x), Some(y)) => compare_keys(x, y, spec_order),
            _ => Ordering::Equal,
        })?;
    }
    Ok(())
}

fn hierarchy_fields<'m, 'a>(
    table: &HierTableSpec,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, FieldId>, ExecError> {
    let mut capacity = 2usize + usize::from(table.folder.is_some());
    let mut count_work = LedgerWork::new(env);
    for key in &table.keys {
        count_work.step()?;
        if key.is_some() {
            capacity = capacity
                .checked_add(1)
                .ok_or_else(crate::model::invalid_size)?;
        }
    }
    count_work.check()?;
    let mut fields = BuilderVec::new(capacity, mem, env)?;
    fields.push(table.id, env)?;
    fields.push(table.parent, env)?;
    if let Some(folder) = table.folder {
        fields.push(folder, env)?;
    }
    let mut work = LedgerWork::new(env);
    for (field, _) in table.keys.iter().flatten() {
        work.step()?;
        fields.push(*field, env)?;
    }
    work.check()?;
    Ok(fields)
}
fn hierarchy_folders<'m, 'a>(
    tables: &[HierTableSpec],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let mut out = BuilderVec::new(tables.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    for table in tables {
        work.step()?;
        if table.folder.is_some() {
            out.push(table.number, env)?;
        }
    }
    work.check()?;
    Ok(out)
}
fn hierarchy_empties<'m, 'a>(
    nodes: &[Node<'_, '_>],
    folders: &[u32],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
    mut empty: impl FnMut(&QValue) -> bool,
) -> Result<BuilderVec<'m, 'a, (u32, usize)>, ExecError> {
    let mut out = BuilderVec::new(nodes.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    for (index, node) in nodes.iter().enumerate() {
        work.step()?;
        if empty(&node.value)
            && let QValue::Ref { table, .. } = &*node.value
            && folders.contains(table)
        {
            out.push((*table, index), env)?;
        }
    }
    work.check()?;
    Ok(out)
}
fn hierarchy_children(nodes: &mut [Node<'_, '_>], env: &ExecEnv<'_>) -> Result<(), ExecError> {
    let mut work = LedgerWork::new(env);
    for k in 0..nodes.len() {
        work.step()?;
        if let Some(p) = nodes.get(k).and_then(|n| n.parent)
            && let Some(parent) = nodes.get_mut(p)
        {
            parent.children.push(k, env)?;
        }
    }
    work.check()
}
fn hierarchy_first_and_depth(
    nodes: &mut [Node<'_, '_>],
    order: &[usize],
    env: &ExecEnv<'_>,
) -> Result<(), ExecError> {
    let mut work = LedgerWork::new(env);
    for &k in order {
        work.step()?;
        let Some(n) = nodes.get(k) else { continue };
        let mut first = n.group.unwrap_or(usize::MAX).min(n.first);
        for &c in &n.children {
            work.step()?;
            if let Some(c) = nodes.get(c) {
                first = first.min(c.first);
            }
        }
        if let Some(n) = nodes.get_mut(k) {
            n.first = first;
        }
    }
    for &k in order.iter().rev() {
        work.step()?;
        let d = nodes
            .get(k)
            .and_then(|n| n.parent)
            .and_then(|p| nodes.get(p))
            .map_or(0, |p| p.depth + 1);
        if let Some(n) = nodes.get_mut(k) {
            n.depth = d;
        }
    }
    work.check()
}
fn hierarchy_link_empties(
    nodes: &mut [Node<'_, '_>],
    empties: BuilderVec<'_, '_, (u32, usize)>,
    env: &ExecEnv<'_>,
) -> Result<(), ExecError> {
    let mut work = LedgerWork::new(env);
    for (table, e) in empties {
        for k in 0..nodes.len() {
            work.step()?;
            let link = k != e
                && nodes.get(k).is_some_and(|n| {
                    n.parent.is_none()
                        && !is_empty_ref(&n.value)
                        && matches!(&*n.value, QValue::Ref { table: t, .. } if *t == table)
                });
            if link && let Some(n) = nodes.get_mut(k) {
                n.parent = Some(e);
            }
        }
    }
    work.check()
}
fn hierarchy_root_ids<'m, 'a>(
    roots: BuilderVec<'m, 'a, Kid>,
    ids: &[Option<u32>],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let mut out = BuilderVec::new(roots.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    for root in roots {
        work.step()?;
        if let Kid::Node(n) = root
            && let Some(id) = ids.get(n).copied().flatten()
        {
            out.push(id, env)?;
        }
    }
    work.check()?;
    Ok(out)
}

fn hierarchy_acc<'m, 'a>(
    nodes: &[Node<'_, '_>],
    groups: &[Group<'_, '_>],
    under: &[BuilderVec<'m, 'a, u32>],
    k: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    let own = nodes
        .get(k)
        .and_then(|n| n.group)
        .and_then(|g| groups.get(g));
    let mut acc = BuilderVec::copy_from(own.map_or(&[], |g| g.rows.as_slice()), mem, env)?;
    let children = BuilderVec::copy_from(
        nodes.get(k).map_or(&[], |n| n.children.as_slice()),
        mem,
        env,
    )?;
    for &c in &children {
        if let Some(rows) = under.get(c) {
            acc.extend_copy(rows, env)?;
        }
    }
    LedgerWork::new(env).check()?;
    acc.sort_unstable();
    LedgerWork::new(env).check()?;
    Ok(acc)
}

fn hierarchy_under<'m, 'a>(
    nodes: &[Node<'_, '_>],
    groups: &[Group<'_, '_>],
    order: &[usize],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, BuilderVec<'m, 'a, u32>>, ExecError> {
    let mut under = hierarchy_under_slots(nodes, mem, env)?;
    for &k in order {
        let acc = hierarchy_acc(nodes, groups, &under, k, mem, env)?;
        if let Some(slot) = under.get_mut(k) {
            *slot = acc;
        }
    }
    Ok(under)
}

fn hierarchy_total_rows<'m, 'a>(
    under: &[BuilderVec<'m, 'a, u32>],
    k: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    BuilderVec::copy_from(under.get(k).map_or(&[], |v| v.as_slice()), mem, env)
}

fn hierarchy_order_rows<'m, 'a>(
    under: &[BuilderVec<'m, 'a, u32>],
    k: usize,
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, u32>, ExecError> {
    BuilderVec::copy_from(under.get(k).map_or(&[], |v| v.as_slice()), mem, env)
}

fn post_order<'m, 'a>(
    nodes: &[Node<'_, '_>],
    mem: &'m MemBudget<'a>,
    env: &ExecEnv<'_>,
) -> Result<BuilderVec<'m, 'a, usize>, ExecError> {
    let mut out = BuilderVec::new(nodes.len(), mem, env)?;
    let mut work = LedgerWork::new(env);
    let mut roots = 0usize;
    for node in nodes {
        work.step()?;
        if node.parent.is_none() {
            roots += 1;
        }
    }
    let mut stack = BuilderVec::new(roots, mem, env)?;
    for k in 0..nodes.len() {
        work.step()?;
        if nodes.get(k).is_some_and(|n| n.parent.is_none()) {
            stack.push((k, false), env)?;
        }
    }
    while !stack.is_empty() {
        work.step()?;
        let (k, done) = stack.pop().ok_or_else(crate::model::invalid_size)?;
        if done {
            out.push(k, env)?;
            continue;
        }
        stack.push((k, true), env)?;
        if let Some(n) = nodes.get(k) {
            for &c in &n.children {
                work.step()?;
                stack.push((c, false), env)?;
            }
        }
    }
    work.check()?;
    Ok(out)
}
