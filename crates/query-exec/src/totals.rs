pub(crate) mod payload;

use payload::{PaidValue, PaidVec};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use bsl_prim::{Collation, Strength};
use ibschema::storage::FieldId;
use query_ir::{ExecError, QValue};
use query_syntax::ast::CmpOp;

use crate::coll::{ExecCollation, Keys};
use crate::eval::try_compare;
use crate::key::{Key, try_key_of_totals};
use crate::model::{
    LedgerWork, SizingStack, VALUE_SIZE, key_model, try_charge_totals_key, vec_block,
};
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

fn order_value_key(
    value: &QValue,
    mem: &MemBudget<'_>,
    coll: ExecCollation<'_>,
    temp: &mut u64,
) -> Result<Key, ExecError> {
    let mut paid = 0;
    let charged = try_charge_totals_key(value, coll, &mut |bytes| {
        mem.grow(bytes)?;
        paid = crate::model::size_add(paid, bytes)?;
        Ok(())
    });
    *temp = crate::model::size_add(*temp, paid)?;
    charged?;
    let key = try_key_of_totals(value, coll)?;
    let kept = key_model(&key);
    if kept > paid {
        mem.grow(kept - paid)?;
        *temp = crate::model::size_add(*temp, kept - paid)?;
    } else {
        mem.shrink(paid - kept);
        *temp -= paid - kept;
    }
    Ok(key)
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
    let mut work = LedgerWork::new(env);
    for value in values.drain(..) {
        work.step()?;
        replacement.push(value);
        #[cfg(test)]
        accounting_invariants::move_checkpoint();
    }
    work.check()?;
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

pub fn build(
    spec: TotalsSpec,
    rows: &[Row],
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    coll: ExecCollation<'_>,
    mem: &MemBudget<'_>,
) -> Result<TotalsTree, ExecError> {
    let n = u32::try_from(rows.len()).map_err(|_| fatal("too many rows"))?;
    let all: Vec<u32> = (0..n).collect();
    mem.grow(vec_block(all.len(), 4))?;
    let r = run(&spec, rows, Some(src), env, coll, mem, &all, &[], 0);
    mem.shrink(vec_block(all.len(), 4));
    r
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
        temp: 0,
        infos: HashMap::new(),
        div_zero: Cell::new(false),
    };
    let r = b
        .level(0, subset, ctx, level)
        .and_then(|roots| b.push_links(&roots));
    let temp = b.temp;
    let error = b.div_zero.get().then_some(TotalsError::DivisionByZero);
    let (records, links, totals) = (b.records, b.links, b.totals);
    drop(b.infos);
    mem.shrink(temp);
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
    rows: Vec<u32>,
}

struct Node<'m, 'a> {
    value: PaidValue<'m, 'a>,
    parent: Option<usize>,
    children: Vec<usize>,
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
    temp: u64,
    infos: HashMap<(u32, [u8; 16]), Option<NodeInfo<'b, 'e>>>,
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

    fn temp(&mut self, n: u64) -> Result<(), ExecError> {
        self.mem.grow(n)?;
        self.temp += n;
        Ok(())
    }

    fn record_order_keys(
        &mut self,
        i: usize,
        row: &Row,
        first: Option<&Row>,
    ) -> Result<Vec<Key>, ExecError> {
        let spec = self.spec;
        self.temp(vec_block(spec.order.len(), std::mem::size_of::<Key>()))?;
        let mut keys = Vec::with_capacity(spec.order.len());
        for order in &spec.order {
            let value = record_key_value(order, i, row, first);
            keys.push(order_value_key(value, self.mem, self.coll, &mut self.temp)?);
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
    ) -> Result<Vec<u32>, ExecError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let spec = self.spec;
        let Some(point) = spec.points.get(i) else {
            self.temp(vec_block(rows.len(), 4))?;
            let mut out = Vec::with_capacity(rows.len());
            for &r in rows {
                self.tick()?;
                out.push(self.push_record(
                    RecordType::Detail,
                    None,
                    level,
                    RowRef::Detail(r),
                    &[],
                )?);
            }
            return Ok(out);
        };
        let Some(col) = point.column else {
            let row = self.total_row(rows, ctx)?;
            let children = self.level(i + 1, rows, ctx, level + 1)?;
            let row = self.push_total(row)?;
            return Ok(vec![self.push_record(
                RecordType::Overall,
                Some(i),
                level,
                row,
                &children,
            )?]);
        };
        match point.hierarchy {
            HierarchyMode::No => {
                let groups = self.group(rows, |b, r| b.cell(r, col))?;
                let mut made: Vec<(Group<'b, 'e>, PaidRow<'b, 'e>)> =
                    Vec::with_capacity(groups.len());
                for g in groups {
                    let inner = payload::context(ctx, col, &g.value, self.mem, Some(self.env))?;
                    let row = self.total_row(&g.rows, &inner)?;
                    made.push((g, row));
                }
                if !spec.order.is_empty() {
                    self.temp(vec_block(made.len(), std::mem::size_of::<Vec<Key>>()))?;
                    let mut ordered = Vec::with_capacity(made.len());
                    for (group, row) in &made {
                        ordered.push(self.record_order_keys(
                            i,
                            row,
                            self.first_row(&group.rows),
                        )?);
                    }
                    let mut idx: Vec<usize> = (0..made.len()).collect();
                    idx.sort_by(|a, b| match (ordered.get(*a), ordered.get(*b)) {
                        (Some(x), Some(y)) => compare_keys(x, y, &spec.order),
                        _ => Ordering::Equal,
                    });
                    let mut slots: Vec<Option<(Group<'b, 'e>, PaidRow<'b, 'e>)>> =
                        made.into_iter().map(Some).collect();
                    made = idx
                        .into_iter()
                        .filter_map(|k| slots.get_mut(k).and_then(Option::take))
                        .collect();
                }
                let mut out = Vec::with_capacity(made.len());
                for (g, row) in made {
                    out.push(self.group_record(i, col, &g, row, ctx, level)?);
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
    ) -> Result<Vec<Group<'b, 'e>>, ExecError> {
        let mut index: HashMap<Key, usize> = HashMap::new();
        let mut groups: Vec<Group<'b, 'e>> = Vec::new();
        for &r in rows {
            self.tick()?;
            let v = value(self, r)?;
            let k = try_key_of_totals(&v, self.coll)?;
            match index.get(&k) {
                Some(&g) => {
                    self.temp(4)?;
                    if let Some(g) = groups.get_mut(g) {
                        g.rows.push(r);
                    }
                }
                None => {
                    self.temp(
                        key_model(&k)
                            + 48
                            + vec_block(1, std::mem::size_of::<Group<'b, 'e>>())
                            + vec_block(1, 4),
                    )?;
                    index.insert(k, groups.len());
                    groups.push(Group {
                        value: v,
                        rows: vec![r],
                    });
                }
            }
        }
        Ok(groups)
    }

    fn table_of(&self, i: usize, v: &QValue) -> Option<HierTableSpec> {
        match v {
            QValue::Ref { table, id } if id.iter().any(|b| *b != 0) => self
                .spec
                .points
                .get(i)?
                .hier
                .iter()
                .find(|t| t.number == *table)
                .cloned(),
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
            let mut fields = vec![table.id, table.parent];
            fields.extend(table.folder);
            fields.extend(table.keys.iter().flatten().map(|(field, _)| *field));
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
            self.temp(64)?;
            self.infos.insert(key, info);
        }
        Ok(self.infos.get(&key).and_then(Option::as_ref))
    }

    fn parents_among(
        &mut self,
        i: usize,
        col: usize,
        rows: &[u32],
    ) -> Result<HashSet<Key>, ExecError> {
        let mut seen: HashSet<Key> = HashSet::new();
        let mut out: HashSet<Key> = HashSet::new();
        for &r in rows {
            self.tick()?;
            let v = self.cell(r, col)?;
            let k = try_key_of_totals(&v, self.coll)?;
            if seen.contains(&k) {
                continue;
            }
            self.temp(key_model(&k) + 48)?;
            seen.insert(k);
            let coll = self.coll;
            if let Some(p) = self.info(i, &v)?.and_then(|n| n.parent.as_ref()) {
                let pk = try_key_of_totals(p, coll)?;
                if !out.contains(&pk) {
                    self.temp(key_model(&pk) + 48)?;
                    out.insert(pk);
                }
            }
        }
        Ok(out)
    }

    fn only_key(
        &mut self,
        i: usize,
        v: PaidValue<'b, 'e>,
        parents: &HashSet<Key>,
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
            None => parents.contains(&try_key_of_totals(&v, coll)?),
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
    ) -> Result<Vec<Key>, ExecError> {
        if self.spec.order.is_empty() {
            return Ok(Vec::new());
        }
        let _ = self.info(i, value)?;
        let info = match value {
            QValue::Ref { table, id } => self.infos.get(&(*table, *id)).and_then(Option::as_ref),
            _ => None,
        };
        let spec = self.spec;
        let first = rows.first().and_then(|r| self.rows.get(*r as usize));
        let bytes = vec_block(spec.order.len(), std::mem::size_of::<Key>());
        self.mem.grow(bytes)?;
        self.temp = crate::model::size_add(self.temp, bytes)?;
        let mut keys = Vec::with_capacity(spec.order.len());
        for (index, order) in spec.order.iter().enumerate() {
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
            keys.push(order_value_key(input, self.mem, self.coll, &mut self.temp)?);
        }
        Ok(keys)
    }

    fn hierarchy(
        &mut self,
        i: usize,
        col: usize,
        rows: &[u32],
        ctx: &[(usize, QValue)],
        level: u32,
    ) -> Result<Vec<u32>, ExecError> {
        let only = self
            .spec
            .points
            .get(i)
            .is_some_and(|p| p.hierarchy == HierarchyMode::Only);
        let parents = if only {
            self.parents_among(i, col, rows)?
        } else {
            HashSet::new()
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
        let order = post_order(&nodes);
        for &k in &order {
            let first = nodes.get(k).map_or(usize::MAX, |n| {
                n.children
                    .iter()
                    .filter_map(|c| nodes.get(*c).map(|c| c.first))
                    .fold(n.group.unwrap_or(usize::MAX).min(n.first), usize::min)
            });
            if let Some(n) = nodes.get_mut(k) {
                n.first = first;
            }
        }
        for &k in order.iter().rev() {
            let d = nodes
                .get(k)
                .and_then(|n| n.parent)
                .and_then(|p| nodes.get(p))
                .map_or(0, |p| p.depth + 1);
            if let Some(n) = nodes.get_mut(k) {
                n.depth = d;
            }
        }
        let mut under: Vec<Vec<u32>> = nodes.iter().map(|_| Vec::new()).collect();
        for &k in &order {
            let mut acc: Vec<u32> = nodes
                .get(k)
                .and_then(|n| n.group)
                .and_then(|g| groups.get(g))
                .map(|g| g.rows.clone())
                .unwrap_or_default();
            for c in nodes.get(k).map(|n| n.children.clone()).unwrap_or_default() {
                if let Some(rows) = under.get(c) {
                    acc.extend_from_slice(rows);
                }
            }
            acc.sort_unstable();
            self.temp(vec_block(acc.len(), 4))?;
            if let Some(slot) = under.get_mut(k) {
                *slot = acc;
            }
        }
        let mut node_rows: Vec<Option<PaidRow<'b, 'e>>> = nodes.iter().map(|_| None).collect();
        let mut group_rows: Vec<Option<PaidRow<'b, 'e>>> = groups.iter().map(|_| None).collect();
        for k in 0..nodes.len() {
            let Some(node) = nodes.get(k) else { continue };
            let inner = payload::context(ctx, col, &node.value, self.mem, Some(self.env))?;
            if !node.children.is_empty() {
                let rows = under.get(k).cloned().unwrap_or_default();
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
        let mut node_keys: Vec<Vec<Key>> = Vec::new();
        let mut group_keys: Vec<Vec<Key>> = Vec::new();
        if sorted {
            self.temp(
                vec_block(nodes.len(), std::mem::size_of::<Vec<Key>>())
                    + vec_block(groups.len(), std::mem::size_of::<Vec<Key>>()),
            )?;
            node_keys = Vec::with_capacity(nodes.len());
            group_keys = Vec::with_capacity(groups.len());
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
                let rows = under.get(k).cloned().unwrap_or_default();
                let ks = self.node_keys(i, value, row_of_node, &rows)?;
                node_keys.push(ks);
            }
            for (g, group) in groups.iter().enumerate() {
                let empty = Vec::new();
                let row = group_rows
                    .get(g)
                    .and_then(Option::as_ref)
                    .map_or(&empty, |row| &row.row);
                let ks = self.node_keys(i, &group.value, row, &group.rows)?;
                group_keys.push(ks);
            }
        }
        let spec_order = &self.spec.order;
        let kid_first = |k: Kid, nodes: &[Node<'_, '_>]| match k {
            Kid::Node(n) => nodes.get(n).map_or(usize::MAX, |x| x.first),
            Kid::Own(g) => g,
        };
        let kid_keys = |k: Kid| -> Option<&Vec<Key>> {
            match k {
                Kid::Node(n) => node_keys.get(n),
                Kid::Own(g) => group_keys.get(g),
            }
        };
        let arrange = |kids: &mut Vec<Kid>, nodes: &[Node<'_, '_>]| {
            kids.sort_by_key(|k| (only && matches!(k, Kid::Own(_)), kid_first(*k, nodes)));
            if sorted {
                kids.sort_by(|a, b| match (kid_keys(*a), kid_keys(*b)) {
                    (Some(x), Some(y)) => compare_keys(x, y, spec_order),
                    _ => Ordering::Equal,
                });
            }
        };
        let mut roots: Vec<Kid> = (0..nodes.len())
            .filter(|k| nodes.get(*k).is_some_and(|n| n.parent.is_none()))
            .map(Kid::Node)
            .collect();
        arrange(&mut roots, &nodes);
        let mut kids_of: Vec<Vec<Kid>> = Vec::with_capacity(nodes.len());
        for k in 0..nodes.len() {
            let mut kids: Vec<Kid> = Vec::new();
            if let Some(n) = nodes.get(k)
                && !n.children.is_empty()
            {
                if let Some(g) = n.group {
                    kids.push(Kid::Own(g));
                }
                kids.extend(n.children.iter().map(|c| Kid::Node(*c)));
                arrange(&mut kids, &nodes);
            }
            kids_of.push(kids);
        }
        let mut ids: Vec<Option<u32>> = nodes.iter().map(|_| None).collect();
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
                let mut children: Vec<u32> = Vec::new();
                for kid in kids_of.get(k).cloned().unwrap_or_default() {
                    match kid {
                        Kid::Own(gi) => {
                            if let Some(g) = groups.get(gi) {
                                let row = group_rows
                                    .get_mut(gi)
                                    .and_then(Option::take)
                                    .ok_or_else(|| fatal("missing total row"))?;
                                children.push(self.group_record(i, col, g, row, ctx, lvl + 1)?);
                            }
                        }
                        Kid::Node(c) => {
                            if let Some(Some(id)) = ids.get(c) {
                                children.push(*id);
                            }
                        }
                    }
                }
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
        Ok(roots
            .into_iter()
            .filter_map(|r| match r {
                Kid::Node(n) => ids.get(n).copied().flatten(),
                Kid::Own(_) => None,
            })
            .collect())
    }

    fn nodes(
        &mut self,
        i: usize,
        groups: &[Group<'b, 'e>],
        only: bool,
    ) -> Result<Vec<Node<'b, 'e>>, ExecError> {
        let mut nodes: Vec<Node<'b, 'e>> = Vec::new();
        let mut index: HashMap<Key, usize> = HashMap::new();
        let node_bytes =
            |k: &Key| key_model(k) + 48 + vec_block(1, std::mem::size_of::<Node<'b, 'e>>());
        for (gi, g) in groups.iter().enumerate() {
            let k = try_key_of_totals(&g.value, self.coll)?;
            let at = match index.get(&k) {
                Some(&n) => n,
                None => {
                    self.temp(node_bytes(&k))?;
                    index.insert(k, nodes.len());
                    nodes.push(Node {
                        value: PaidValue::copy(&g.value, self.mem, Some(self.env))?,
                        parent: None,
                        children: Vec::new(),
                        group: None,
                        first: gi,
                        depth: 0,
                    });
                    nodes.len() - 1
                }
            };
            if let Some(n) = nodes.get_mut(at) {
                n.group = Some(gi);
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
                let pk = try_key_of_totals(&parent, coll)?;
                match index.get(&pk) {
                    Some(&p) => {
                        if !is_ancestor_or_self(&nodes, cur, p)
                            && let Some(n) = nodes.get_mut(cur)
                        {
                            n.parent = Some(p);
                        }
                        break;
                    }
                    None => {
                        self.temp(node_bytes(&pk))?;
                        index.insert(pk, nodes.len());
                        nodes.push(Node {
                            value: parent,
                            parent: None,
                            children: Vec::new(),
                            group: None,
                            first: gi,
                            depth: 0,
                        });
                        let p = nodes.len() - 1;
                        if let Some(n) = nodes.get_mut(cur) {
                            n.parent = Some(p);
                        }
                        cur = p;
                    }
                }
            }
        }
        if only {
            let folders: Vec<u32> = self
                .spec
                .points
                .get(i)
                .map(|p| {
                    p.hier
                        .iter()
                        .filter(|t| t.folder.is_some())
                        .map(|t| t.number)
                        .collect()
                })
                .unwrap_or_default();
            let empties: Vec<(u32, usize)> = nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    self.empty_of_table(i, &n.value)
                        && matches!(&*n.value, QValue::Ref { table, .. } if folders.contains(table))
                })
                .filter_map(|(k, n)| match &*n.value {
                    QValue::Ref { table, .. } => Some((*table, k)),
                    _ => None,
                })
                .collect();
            for (table, e) in empties {
                for k in 0..nodes.len() {
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
        }
        for k in 0..nodes.len() {
            if let Some(p) = nodes.get(k).and_then(|n| n.parent) {
                self.temp(4)?;
                if let Some(pn) = nodes.get_mut(p) {
                    pn.children.push(k);
                }
            }
        }
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

fn post_order(nodes: &[Node<'_, '_>]) -> Vec<usize> {
    let mut out = Vec::with_capacity(nodes.len());
    let mut stack: Vec<(usize, bool)> = (0..nodes.len())
        .filter(|k| nodes.get(*k).is_some_and(|n| n.parent.is_none()))
        .map(|k| (k, false))
        .collect();
    while let Some((k, done)) = stack.pop() {
        if done {
            out.push(k);
            continue;
        }
        stack.push((k, true));
        if let Some(n) = nodes.get(k) {
            stack.extend(n.children.iter().map(|c| (*c, false)));
        }
    }
    out
}
