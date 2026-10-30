use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use bsl_prim::{Collation, Number, Strength};
use ibschema::storage::FieldId;
use query_ir::{ExecError, QValue};

use crate::coll::{ExecCollation, Keys};
use crate::eval::try_compare;
use crate::key::{Key, try_key_of_totals};
use crate::model::{
    VALUE_SIZE, key_model, row_model, try_charge_totals_key, value_model, vec_block,
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
        vec_block(self.records.capacity(), std::mem::size_of::<Record>())
            + vec_block(self.links.capacity(), std::mem::size_of::<u32>())
            + vec_block(self.totals.capacity(), std::mem::size_of::<Row>())
            + self.totals.iter().map(|r| row_model(r)).sum::<u64>()
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

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q4" }
}

fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("totals: {what}"))
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
    let mut b = Builder {
        spec,
        rows,
        src,
        env,
        mem,
        coll,
        records: Vec::new(),
        links: Vec::new(),
        totals: Vec::new(),
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
    mem.shrink(temp);
    let roots = r?;
    Ok(TotalsTree {
        spec: spec.clone(),
        records,
        links,
        root_links: roots,
        totals,
        max_number_limbs: env.max_number_limbs,
        error,
    })
}

#[derive(Clone)]
struct NodeInfo {
    parent: Option<QValue>,
    folder: bool,
    keys: Vec<Option<QValue>>,
}

struct Group {
    value: QValue,
    rows: Vec<u32>,
}

struct Node {
    value: QValue,
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
    infos: HashMap<(u32, [u8; 16]), Option<NodeInfo>>,
    div_zero: Cell<bool>,
}

fn cell_of(rows: &[Row], r: u32, c: usize) -> Option<&QValue> {
    rows.get(r as usize).and_then(|x| x.get(c))
}

fn is_empty_ref(v: &QValue) -> bool {
    matches!(v, QValue::Ref { id, .. } if id.iter().all(|b| *b == 0))
}

impl Builder<'_, '_> {
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

    fn order_keys(&mut self, tuples: &[Vec<QValue>]) -> Result<Vec<Vec<Key>>, ExecError> {
        self.temp(vec_block(tuples.len(), std::mem::size_of::<Vec<Key>>()))?;
        let mut out = Vec::with_capacity(tuples.len());
        for t in tuples {
            self.temp(vec_block(t.len(), std::mem::size_of::<Key>()))?;
            let mut keys = Vec::with_capacity(t.len());
            for v in t {
                keys.push(self.order_key(v)?);
            }
            out.push(keys);
        }
        Ok(out)
    }

    fn order_key(&mut self, v: &QValue) -> Result<Key, ExecError> {
        let mem = self.mem;
        let mut paid = 0;
        let charged = try_charge_totals_key(v, self.coll, &mut |n| {
            mem.grow(n)?;
            paid += n;
            Ok(())
        });
        self.temp += paid;
        charged?;
        let key = try_key_of_totals(v, self.coll)?;
        let kept = key_model(&key);
        if kept > paid {
            self.temp(kept - paid)?;
        } else {
            mem.shrink(paid - kept);
            self.temp -= paid - kept;
        }
        Ok(key)
    }

    fn push_links(&mut self, ids: &[u32]) -> Result<Range<u32>, ExecError> {
        self.mem.grow(vec_block(ids.len(), 4))?;
        let start = u32::try_from(self.links.len()).map_err(|_| fatal("too many records"))?;
        self.links.extend_from_slice(ids);
        let end = u32::try_from(self.links.len()).map_err(|_| fatal("too many records"))?;
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
        let children = self.push_links(children)?;
        self.mem.grow(vec_block(1, std::mem::size_of::<Record>()))?;
        let id = u32::try_from(self.records.len()).map_err(|_| fatal("too many records"))?;
        let grouping = match grouping {
            Some(g) => Some(u16::try_from(g).map_err(|_| fatal("too many control points"))?),
            None => None,
        };
        self.records.push(Record {
            kind,
            grouping,
            level,
            row,
            children,
        });
        Ok(id)
    }

    fn push_total(&mut self, row: Row) -> Result<RowRef, ExecError> {
        self.mem
            .grow(row_model(&row) + vec_block(1, std::mem::size_of::<Row>()))?;
        let i = u32::try_from(self.totals.len()).map_err(|_| fatal("too many records"))?;
        self.totals.push(row);
        Ok(RowRef::Total(i))
    }

    fn cell(&self, row: u32, col: usize) -> QValue {
        cell_of(self.rows, row, col)
            .cloned()
            .unwrap_or(QValue::Null)
    }

    fn first_row(&self, rows: &[u32]) -> Option<&Row> {
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
                let groups = self.group(rows, |b, r| Ok(b.cell(r, col)))?;
                let mut made: Vec<(Group, Row)> = Vec::with_capacity(groups.len());
                for g in groups {
                    let row = self.total_row(&g.rows, &with(ctx, col, &g.value))?;
                    made.push((g, row));
                }
                if !spec.order.is_empty() {
                    let keyed: Vec<Vec<QValue>> = made
                        .iter()
                        .map(|(g, row)| record_keys(&spec.order, i, row, self.first_row(&g.rows)))
                        .collect();
                    let ordered = self.order_keys(&keyed)?;
                    let mut idx: Vec<usize> = (0..made.len()).collect();
                    idx.sort_by(|a, b| match (ordered.get(*a), ordered.get(*b)) {
                        (Some(x), Some(y)) => compare_keys(x, y, &spec.order),
                        _ => Ordering::Equal,
                    });
                    let mut slots: Vec<Option<(Group, Row)>> = made.into_iter().map(Some).collect();
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
        g: &Group,
        row: Row,
        ctx: &[(usize, QValue)],
        level: u32,
    ) -> Result<u32, ExecError> {
        let inner = with(ctx, col, &g.value);
        let children = self.level(i + 1, &g.rows, &inner, level + 1)?;
        let row = self.push_total(row)?;
        self.push_record(RecordType::GroupTotal, Some(i), level, row, &children)
    }

    fn group(
        &mut self,
        rows: &[u32],
        mut value: impl FnMut(&mut Self, u32) -> Result<QValue, ExecError>,
    ) -> Result<Vec<Group>, ExecError> {
        let mut index: HashMap<Key, usize> = HashMap::new();
        let mut groups: Vec<Group> = Vec::new();
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
                            + value_model(&v)
                            + vec_block(1, std::mem::size_of::<Group>())
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

    fn info(&mut self, i: usize, v: &QValue) -> Result<Option<NodeInfo>, ExecError> {
        let Some(t) = self.table_of(i, v) else {
            return Ok(None);
        };
        let QValue::Ref { table, id } = v else {
            return Ok(None);
        };
        if let Some(found) = self.infos.get(&(*table, *id)) {
            return Ok(found.clone());
        }
        let Some(src) = self.src else {
            return Err(later("переход к группировке с иерархией"));
        };
        self.tick()?;
        let mut fields = vec![t.id, t.parent];
        fields.extend(t.folder);
        fields.extend(t.keys.iter().flatten().map(|(f, _)| *f));
        let row = src.get_by_id_fields(t.table, v, &fields)?;
        let order_len = self.spec.order.len();
        let info = row.map(|row| {
            let at = |f: FieldId| row.get(usize::from(f.0)).cloned().unwrap_or(QValue::Null);
            let parent = match at(t.parent) {
                p @ QValue::Ref { .. } if !is_empty_ref(&p) => Some(p),
                _ => None,
            };
            NodeInfo {
                parent,
                folder: t.folder.is_some_and(|f| at(f) == QValue::Bool(false)),
                keys: (0..order_len)
                    .map(|k| {
                        t.keys
                            .get(k)
                            .copied()
                            .flatten()
                            .map(|(f, not)| match at(f) {
                                QValue::Bool(b) if not => QValue::Bool(!b),
                                v => v,
                            })
                    })
                    .collect(),
            }
        });
        let bytes = 64
            + info.as_ref().map_or(0, |i| {
                i.parent.as_ref().map_or(0, value_model)
                    + vec_block(i.keys.len(), VALUE_SIZE)
                    + i.keys.iter().flatten().map(value_model).sum::<u64>()
            });
        self.temp(bytes)?;
        self.infos.insert((*table, *id), info.clone());
        Ok(info)
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
            let v = self.cell(r, col);
            let k = try_key_of_totals(&v, self.coll)?;
            if seen.contains(&k) {
                continue;
            }
            self.temp(key_model(&k) + 48)?;
            seen.insert(k);
            if let Some(p) = self.info(i, &v)?.and_then(|n| n.parent) {
                let pk = try_key_of_totals(&p, self.coll)?;
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
        v: QValue,
        parents: &HashSet<Key>,
    ) -> Result<QValue, ExecError> {
        if self.empty_of_table(i, &v) {
            return Ok(v);
        }
        let Some(t) = self.table_of(i, &v) else {
            return Ok(QValue::Null);
        };
        let Some(info) = self.info(i, &v)? else {
            return Ok(QValue::Null);
        };
        let parent_itself = match t.folder {
            Some(_) => info.folder,
            None => parents.contains(&try_key_of_totals(&v, self.coll)?),
        };
        if parent_itself {
            return Ok(v);
        }
        Ok(info.parent.unwrap_or(QValue::Ref {
            table: t.number,
            id: [0u8; 16],
        }))
    }

    fn node_keys(
        &mut self,
        i: usize,
        value: &QValue,
        row: &Row,
        rows: &[u32],
    ) -> Result<Vec<QValue>, ExecError> {
        let order = &self.spec.order;
        let mut keys = record_keys(order, i, row, self.first_row(rows));
        if keys.is_empty() {
            return Ok(keys);
        }
        for (k, key) in keys.iter_mut().enumerate() {
            if let Some(OrderKey {
                source: KeySource::Point { point, .. },
                ..
            }) = order.get(k)
                && *point == i
            {
                *key = value.clone();
            }
        }
        if let Some(info) = self.info(i, value)? {
            for (k, key) in keys.iter_mut().enumerate() {
                if let (
                    Some(OrderKey {
                        source: KeySource::FirstRow { point, .. },
                        ..
                    }),
                    Some(Some(v)),
                ) = (order.get(k), info.keys.get(k))
                    && *point == i
                {
                    *key = v.clone();
                }
            }
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
            let v = b.cell(r, col);
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
        let mut node_rows: Vec<Option<Row>> = nodes.iter().map(|_| None).collect();
        let mut group_rows: Vec<Option<Row>> = groups.iter().map(|_| None).collect();
        for k in 0..nodes.len() {
            let Some(node) = nodes.get(k) else { continue };
            let inner = with(ctx, col, &node.value);
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
        let mut node_keys: Vec<Vec<QValue>> = Vec::new();
        let mut group_keys: Vec<Vec<QValue>> = Vec::new();
        if sorted {
            for k in 0..nodes.len() {
                let (value, group, is_hier) = match nodes.get(k) {
                    Some(n) => (n.value.clone(), n.group, !n.children.is_empty()),
                    None => continue,
                };
                let row_of_node = if is_hier {
                    node_rows.get(k).cloned().flatten()
                } else {
                    group.and_then(|g| group_rows.get(g).cloned().flatten())
                }
                .unwrap_or_default();
                let rows = under.get(k).cloned().unwrap_or_default();
                let ks = self.node_keys(i, &value, &row_of_node, &rows)?;
                self.temp(
                    vec_block(ks.len(), VALUE_SIZE) + ks.iter().map(value_model).sum::<u64>(),
                )?;
                node_keys.push(ks);
            }
            for (g, group) in groups.iter().enumerate() {
                let row = group_rows.get(g).cloned().flatten().unwrap_or_default();
                let ks = self.node_keys(i, &group.value, &row, &group.rows)?;
                self.temp(
                    vec_block(ks.len(), VALUE_SIZE) + ks.iter().map(value_model).sum::<u64>(),
                )?;
                group_keys.push(ks);
            }
        }
        let node_keys = self.order_keys(&node_keys)?;
        let group_keys = self.order_keys(&group_keys)?;
        let spec_order = &self.spec.order;
        let kid_first = |k: Kid, nodes: &[Node]| match k {
            Kid::Node(n) => nodes.get(n).map_or(usize::MAX, |x| x.first),
            Kid::Own(g) => g,
        };
        let kid_keys = |k: Kid| -> Option<&Vec<Key>> {
            match k {
                Kid::Node(n) => node_keys.get(n),
                Kid::Own(g) => group_keys.get(g),
            }
        };
        let arrange = |kids: &mut Vec<Kid>, nodes: &[Node]| {
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
                    .unwrap_or_default();
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
                                    .unwrap_or_default();
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
                    .unwrap_or_default();
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

    fn nodes(&mut self, i: usize, groups: &[Group], only: bool) -> Result<Vec<Node>, ExecError> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut index: HashMap<Key, usize> = HashMap::new();
        let node_bytes = |k: &Key| key_model(k) + 48 + vec_block(1, std::mem::size_of::<Node>());
        for (gi, g) in groups.iter().enumerate() {
            let k = try_key_of_totals(&g.value, self.coll)?;
            let at = match index.get(&k) {
                Some(&n) => n,
                None => {
                    self.temp(node_bytes(&k))?;
                    index.insert(k, nodes.len());
                    nodes.push(Node {
                        value: g.value.clone(),
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
                    Some(n) if n.parent.is_none() => n.value.clone(),
                    _ => break,
                };
                let Some(info) = self.info(i, &v)? else { break };
                let Some(parent) = info.parent else { break };
                let pk = try_key_of_totals(&parent, self.coll)?;
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
                        && matches!(&n.value, QValue::Ref { table, .. } if folders.contains(table))
                })
                .filter_map(|(k, n)| match &n.value {
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
                                && matches!(&n.value, QValue::Ref { table: t, .. } if *t == table)
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

    fn total_row(&mut self, rows: &[u32], ctx: &[(usize, QValue)]) -> Result<Row, ExecError> {
        let data = self.rows;
        let aggs = aggregate(
            &self.spec.aggs,
            rows,
            |r, c| cell_of(data, r, c),
            self.coll,
            self.env.max_number_limbs,
        )?;
        let row = total_values(
            self.spec,
            &aggs,
            ctx,
            self.coll,
            self.env.max_number_limbs,
            &self.div_zero,
        )?;
        self.mem.check_value(row_model(&row))?;
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
    let mut row = vec![QValue::Null; spec.width];
    for (c, v) in ctx {
        if let Some(slot) = row.get_mut(*c) {
            *slot = v.clone();
        }
    }
    for (e, c) in &spec.items {
        let v = eval(e, aggs, ctx, coll, limbs, div_zero)?;
        if let Some(slot) = row.get_mut(*c) {
            *slot = v;
        }
    }
    Ok(row)
}

fn guard(n: Number, limbs: u64) -> Result<QValue, ExecError> {
    if n.size_hint_limbs() > limbs {
        return Err(ExecError::Fatal("превышен лимит размера числа".to_string()));
    }
    Ok(QValue::Number(n))
}

pub(crate) fn aggregate<'v>(
    aggs: &'v [AggSpec],
    rows: &[u32],
    cell: impl Fn(u32, usize) -> Option<&'v QValue>,
    coll: ExecCollation<'_>,
    limbs: u64,
) -> Result<Vec<QValue>, ExecError> {
    let mut out = Vec::with_capacity(aggs.len());
    for a in aggs {
        let get = |r: u32| -> Option<&'v QValue> {
            match &a.arg {
                AggArg::Column(c) => cell(r, *c).filter(|v| **v != QValue::Null),
                AggArg::Const(v) => Some(v).filter(|v| **v != QValue::Null),
                AggArg::Star => None,
            }
        };
        let v = match (a.f, &a.arg) {
            (AggFunc::Count, AggArg::Star) => QValue::Number(Number::from_u64(rows.len() as u64)),
            (_, AggArg::Star) => QValue::Null,
            (AggFunc::Count, _) if a.distinct => {
                let mut seen: HashSet<Key> = HashSet::new();
                for &r in rows {
                    if let Some(v) = get(r) {
                        seen.insert(try_key_of_totals(v, coll)?);
                    }
                }
                QValue::Number(Number::from_u64(seen.len() as u64))
            }
            (AggFunc::Count, _) => QValue::Number(Number::from_u64(
                rows.iter().filter(|r| get(**r).is_some()).count() as u64,
            )),
            (AggFunc::Sum | AggFunc::Avg, _) => {
                let mut sum: Option<Number> = None;
                let mut seen: HashSet<Key> = HashSet::new();
                let mut count: u64 = 0;
                for &r in rows {
                    let Some(v) = get(r) else { continue };
                    let QValue::Number(n) = v else {
                        return Err(later("ИТОГИ: СУММА/СРЕДНЕЕ не числа"));
                    };
                    if a.distinct && !seen.insert(try_key_of_totals(v, coll)?) {
                        continue;
                    }
                    count += 1;
                    let s = match &sum {
                        Some(s) => s.add(n),
                        None => n.clone(),
                    };
                    if s.size_hint_limbs() > limbs {
                        return Err(ExecError::Fatal("превышен лимит размера числа".to_string()));
                    }
                    sum = Some(s);
                }
                match (sum, a.f) {
                    (None, _) => QValue::Null,
                    (Some(s), AggFunc::Avg) => match s.div(&Number::from_u64(count)) {
                        Ok(q) => guard(q, limbs)?,
                        Err(_) => QValue::Null,
                    },
                    (Some(s), _) => QValue::Number(s),
                }
            }
            (AggFunc::Min | AggFunc::Max, _) => {
                let mut best: Option<&QValue> = None;
                for &r in rows {
                    let Some(v) = get(r) else { continue };
                    best = match best {
                        None => Some(v),
                        Some(b) => {
                            let o = try_compare_totals(v, b, coll)?;
                            let better = if a.f == AggFunc::Min {
                                o == Ordering::Less
                            } else {
                                o == Ordering::Greater
                            };
                            Some(if better { v } else { b })
                        }
                    };
                }
                best.cloned().unwrap_or(QValue::Null)
            }
        };
        out.push(v);
    }
    Ok(out)
}

pub(crate) fn eval(
    e: &TotalsExpr,
    aggs: &[QValue],
    ctx: &[(usize, QValue)],
    coll: ExecCollation<'_>,
    limbs: u64,
    div_zero: &Cell<bool>,
) -> Result<QValue, ExecError> {
    let ev = |x: &TotalsExpr| eval(x, aggs, ctx, coll, limbs, div_zero);
    Ok(match e {
        TotalsExpr::Agg(i) => aggs.get(*i).cloned().unwrap_or(QValue::Null),
        TotalsExpr::Param(_) => return Err(fatal("an unbound parameter in a total")),
        TotalsExpr::Column(c) => ctx
            .iter()
            .rev()
            .find(|(k, _)| k == c)
            .map_or(QValue::Null, |(_, v)| v.clone()),
        TotalsExpr::Const(v) => v.clone(),
        TotalsExpr::Neg(x) => match ev(x)? {
            QValue::Null => QValue::Null,
            QValue::Number(n) => QValue::Number(n.neg()),
            _ => return Err(later("ИТОГИ: унарный минус не числа")),
        },
        TotalsExpr::Bin(op, l, r) => {
            let (a, b) = (ev(l)?, ev(r)?);
            match (a, b) {
                (QValue::Null, _) | (_, QValue::Null) => QValue::Null,
                (QValue::Number(x), QValue::Number(y)) => match op {
                    ArithOp::Add => guard(x.add(&y), limbs)?,
                    ArithOp::Sub => guard(x.sub(&y), limbs)?,
                    ArithOp::Mul => guard(x.mul(&y), limbs)?,
                    ArithOp::Div => match x.div(&y) {
                        Ok(q) => guard(q, limbs)?,
                        Err(_) => {
                            div_zero.set(true);
                            QValue::Null
                        }
                    },
                },
                (QValue::String(x), QValue::String(y)) if *op == ArithOp::Add => {
                    let mut units: Vec<u16> = x.0.to_vec();
                    units.extend_from_slice(&y.0);
                    QValue::String(query_ir::QStr(units.into()))
                }
                _ => return Err(later("ИТОГИ: операция над нечисловыми значениями")),
            }
        }
        TotalsExpr::IsNull(a, b) => match ev(a)? {
            QValue::Null => ev(b)?,
            v => v,
        },
        TotalsExpr::Case { whens, otherwise } => {
            for (c, v) in whens {
                if ev(c)? == QValue::Bool(true) {
                    return ev(v);
                }
            }
            match otherwise {
                Some(o) => ev(o)?,
                None => QValue::Null,
            }
        }
        TotalsExpr::Cmp(op, l, r) => match try_compare(*op, &ev(l)?, &ev(r)?, coll)? {
            Some(b) => QValue::Bool(b),
            None => QValue::Null,
        },
        TotalsExpr::Not(x) => match ev(x)? {
            QValue::Bool(b) => QValue::Bool(!b),
            _ => QValue::Null,
        },
        TotalsExpr::And(xs) => {
            let mut unknown = false;
            for x in xs {
                match ev(x)? {
                    QValue::Bool(false) => return Ok(QValue::Bool(false)),
                    QValue::Bool(true) => {}
                    _ => unknown = true,
                }
            }
            if unknown {
                QValue::Null
            } else {
                QValue::Bool(true)
            }
        }
        TotalsExpr::Or(xs) => {
            let mut unknown = false;
            for x in xs {
                match ev(x)? {
                    QValue::Bool(true) => return Ok(QValue::Bool(true)),
                    QValue::Bool(false) => {}
                    _ => unknown = true,
                }
            }
            if unknown {
                QValue::Null
            } else {
                QValue::Bool(false)
            }
        }
    })
}

fn is_ancestor_or_self(nodes: &[Node], a: usize, b: usize) -> bool {
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

fn post_order(nodes: &[Node]) -> Vec<usize> {
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
