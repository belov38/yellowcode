use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bsl_prim::{BslDate, Collation, fold_name};
use query_ir::{ExecError, QValue};
use query_syntax::Kw;

use crate::key::{Key, key_of_totals};
use crate::result::QueryResult;
use crate::source::Row;
use crate::totals::{
    KeySource, OrderKey, PointSpec, RecordType, RowRef, TotalsError, TotalsSpec, TotalsTree,
    aggregate, build_sub, compare_keys, record_keys, total_values, with,
};

const MAX_FILL: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    Direct,
    ByGroups,
    ByGroupsWithHierarchy,
}

#[derive(Debug)]
pub enum WalkError {
    DimensionNotFound(String),
    DimensionNotFoundInParents(String),
    DifferentDimensionCount,
    GroupingNotFound(String),
    ColumnNotFound(String),
    NoCurrentRecord,
    DivisionByZero,
    Later(&'static str),
    Exec(ExecError),
}

impl From<ExecError> for WalkError {
    fn from(e: ExecError) -> Self {
        WalkError::Exec(e)
    }
}

fn check(tree: &TotalsTree) -> Result<(), WalkError> {
    match tree.error {
        Some(TotalsError::DivisionByZero) => Err(WalkError::DivisionByZero),
        None => Ok(()),
    }
}

type FillTarget = Option<(Option<Arc<TotalsTree>>, usize)>;

#[derive(Debug)]
struct Virtual {
    kind: RecordType,
    tree: Option<Arc<TotalsTree>>,
    grouping: u16,
    level: u32,
    row: Row,
    details: Vec<u32>,
    ctx: Vec<(usize, QValue)>,
}

#[derive(Clone, Debug)]
enum Item {
    Rec {
        tree: Option<Arc<TotalsTree>>,
        id: u32,
    },
    Row(u32),
    Virtual(Arc<Virtual>),
}

#[derive(Clone, Debug)]
pub struct Selection {
    result: Arc<QueryResult>,
    items: Arc<[Item]>,
    pos: usize,
    parents: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub row: RowRef,
    pub children: Vec<TreeRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Unloaded {
    Table(Vec<RowRef>),
    Tree(Vec<TreeRow>),
}

pub fn select(
    result: &Arc<QueryResult>,
    walk: Walk,
    groupings: &str,
    fill: &str,
    coll: &dyn Collation,
) -> Result<Selection, WalkError> {
    if let Some(t) = &result.totals {
        check(t)?;
    }
    let items = items_for(result, None, &[], walk, groupings, fill, coll)?;
    Ok(Selection {
        result: Arc::clone(result),
        items: items.into(),
        pos: 0,
        parents: Vec::new(),
    })
}

pub fn unload(result: &QueryResult, walk: Walk) -> Result<Unloaded, WalkError> {
    let Some(tree) = &result.totals else {
        let refs = (0..result.rows.len()).filter_map(|i| u32::try_from(i).ok());
        return Ok(match walk {
            Walk::Direct => Unloaded::Table(refs.map(RowRef::Detail).collect()),
            _ => Unloaded::Tree(
                refs.map(|i| TreeRow {
                    row: RowRef::Detail(i),
                    children: Vec::new(),
                })
                .collect(),
            ),
        });
    };
    check(tree)?;
    Ok(match walk {
        Walk::Direct => Unloaded::Table(
            preorder(tree, tree.roots().collect())
                .into_iter()
                .filter_map(|id| tree.records.get(id as usize).map(|r| r.row))
                .collect(),
        ),
        _ => Unloaded::Tree(unload_tree(tree, walk)),
    })
}

impl Selection {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        if self.pos <= self.items.len() {
            self.pos += 1;
        }
        self.pos <= self.items.len()
    }

    pub fn reset(&mut self) {
        self.pos = 0;
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn width(&self) -> usize {
        match &self.result.totals {
            Some(t) => t.spec.width,
            None => self.result.columns.len(),
        }
    }

    pub fn column(&self, name: &str) -> Option<usize> {
        let want = fold_name(name);
        self.result
            .columns
            .iter()
            .position(|c| fold_name(&c.name) == want)
    }

    fn current(&self) -> Option<&Item> {
        self.pos.checked_sub(1).and_then(|i| self.items.get(i))
    }

    fn current_or_err(&self) -> Result<&Item, WalkError> {
        self.current().ok_or(WalkError::NoCurrentRecord)
    }

    pub fn record_type(&self) -> Result<RecordType, WalkError> {
        Ok(kind_of(&self.result, self.current_or_err()?))
    }

    pub fn level(&self) -> Result<u32, WalkError> {
        Ok(level_of(&self.result, self.current_or_err()?))
    }

    pub fn grouping(&self) -> Result<&str, WalkError> {
        let item = self.current_or_err()?;
        Ok(point_of(&self.result, item).map_or("", |p| p.name.as_str()))
    }

    pub fn value(&self, column: usize) -> Result<&QValue, WalkError> {
        let item = self.current_or_err()?;
        if column >= self.width() {
            return Err(WalkError::ColumnNotFound(column.to_string()));
        }
        Ok(row_of(&self.result, item)
            .and_then(|r| r.get(column))
            .unwrap_or(&QValue::Null))
    }

    pub fn source_row(&self) -> Result<Option<RowRef>, WalkError> {
        Ok(match self.current_or_err()? {
            Item::Row(i) => Some(RowRef::Detail(*i)),
            Item::Rec { tree: None, id } => self
                .result
                .totals
                .as_ref()
                .and_then(|t| t.records.get(*id as usize))
                .map(|r| r.row),
            _ => None,
        })
    }

    pub fn next_by_field_value(&mut self, column: &str) -> Result<bool, WalkError> {
        let col = self
            .column(column)
            .ok_or_else(|| WalkError::ColumnNotFound(column.to_string()))?;
        let cur = match self.current() {
            Some(_) => Some(self.value(col)?.clone()),
            None => None,
        };
        while self.next() {
            let v = self.value(col)?;
            if cur.as_ref() != Some(v) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn find_next(&mut self, filter: &[(&str, QValue)]) -> Result<bool, WalkError> {
        let cols = filter
            .iter()
            .map(|(n, v)| {
                self.column(n)
                    .map(|c| (c, v))
                    .ok_or_else(|| WalkError::ColumnNotFound((*n).to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        while self.next() {
            let mut all = true;
            for (c, v) in &cols {
                if self.value(*c)? != *v {
                    all = false;
                    break;
                }
            }
            if all {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn select(
        &self,
        walk: Walk,
        groupings: &str,
        fill: &str,
        coll: &dyn Collation,
    ) -> Result<Selection, WalkError> {
        let item = self.current_or_err()?.clone();
        let mut parents = self.parents.clone();
        if let Some(p) = point_of(&self.result, &item) {
            parents.push(fold_name(&p.name));
        }
        let items = items_for(
            &self.result,
            Some(&item),
            &parents,
            walk,
            groupings,
            fill,
            coll,
        )?;
        Ok(Selection {
            result: Arc::clone(&self.result),
            items: items.into(),
            pos: 0,
            parents,
        })
    }
}

fn tree_of<'r>(
    result: &'r QueryResult,
    tree: &'r Option<Arc<TotalsTree>>,
) -> Option<&'r TotalsTree> {
    match tree {
        Some(t) => Some(t),
        None => result.totals.as_ref(),
    }
}

fn record<'r>(
    result: &'r QueryResult,
    tree: &'r Option<Arc<TotalsTree>>,
    id: u32,
) -> Option<&'r crate::totals::Record> {
    tree_of(result, tree)?.records.get(id as usize)
}

fn kind_of(result: &QueryResult, item: &Item) -> RecordType {
    match item {
        Item::Rec { tree, id } => record(result, tree, *id).map_or(RecordType::Detail, |r| r.kind),
        Item::Row(_) => RecordType::Detail,
        Item::Virtual(v) => v.kind,
    }
}

fn level_of(result: &QueryResult, item: &Item) -> u32 {
    match item {
        Item::Rec { tree, id } => record(result, tree, *id).map_or(0, |r| r.level),
        Item::Row(_) => 0,
        Item::Virtual(v) => v.level,
    }
}

fn grouping_of<'r>(result: &'r QueryResult, item: &'r Item) -> Option<(&'r TotalsTree, usize)> {
    match item {
        Item::Rec { tree, id } => {
            let t = tree_of(result, tree)?;
            Some((t, usize::from(t.records.get(*id as usize)?.grouping?)))
        }
        Item::Row(_) => None,
        Item::Virtual(v) => Some((tree_of(result, &v.tree)?, usize::from(v.grouping))),
    }
}

fn point_of<'r>(result: &'r QueryResult, item: &'r Item) -> Option<&'r PointSpec> {
    let (t, g) = grouping_of(result, item)?;
    t.spec.points.get(g)
}

fn ref_row<'r>(
    result: &'r QueryResult,
    tree: Option<&'r TotalsTree>,
    r: RowRef,
) -> Option<&'r Row> {
    match r {
        RowRef::Detail(i) => result.rows.get(i as usize),
        RowRef::Total(i) => tree?.totals.get(i as usize),
    }
}

fn row_of<'r>(result: &'r QueryResult, item: &'r Item) -> Option<&'r Row> {
    match item {
        Item::Rec { tree, id } => {
            let t = tree_of(result, tree);
            ref_row(result, t, t?.records.get(*id as usize)?.row)
        }
        Item::Row(i) => result.rows.get(*i as usize),
        Item::Virtual(v) => Some(&v.row),
    }
}

fn preorder(tree: &TotalsTree, roots: Vec<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack: Vec<u32> = roots.into_iter().rev().collect();
    while let Some(id) = stack.pop() {
        out.push(id);
        let kids: Vec<u32> = tree.children(id).collect();
        stack.extend(kids.into_iter().rev());
    }
    out
}

fn flatten(tree: &TotalsTree, list: Vec<u32>, g: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack: Vec<u32> = list.into_iter().rev().collect();
    while let Some(id) = stack.pop() {
        out.push(id);
        if let Some(r) = tree.records.get(id as usize)
            && r.kind == RecordType::HierarchyTotal
            && r.grouping.map(usize::from) == Some(g)
        {
            let kids: Vec<u32> = tree.children(id).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

fn details_of(result: &QueryResult, item: &Item) -> Vec<u32> {
    match item {
        Item::Row(i) => vec![*i],
        Item::Virtual(v) => v.details.clone(),
        Item::Rec { tree, id } => {
            let Some(t) = tree_of(result, tree) else {
                return Vec::new();
            };
            let mut out: Vec<u32> = preorder(t, vec![*id])
                .into_iter()
                .filter_map(|k| match t.records.get(k as usize).map(|r| r.row) {
                    Some(RowRef::Detail(i)) => Some(i),
                    _ => None,
                })
                .collect();
            out.sort_unstable();
            out
        }
    }
}

fn ctx_of(result: &QueryResult, item: &Item) -> Vec<(usize, QValue)> {
    match item {
        Item::Virtual(v) => v.ctx.clone(),
        Item::Row(_) => Vec::new(),
        Item::Rec { tree, id } => {
            let Some(t) = tree_of(result, tree) else {
                return Vec::new();
            };
            let Some(r) = t.records.get(*id as usize) else {
                return Vec::new();
            };
            let Some(g) = r.grouping.map(usize::from) else {
                return Vec::new();
            };
            let row = ref_row(result, Some(t), r.row);
            let first = details_of(result, item)
                .first()
                .and_then(|i| result.rows.get(*i as usize));
            t.spec
                .points
                .iter()
                .take(g + 1)
                .filter_map(|p| p.column)
                .map(|c| {
                    let v = if c < t.spec.width { row } else { first }
                        .and_then(|row| row.get(c))
                        .cloned()
                        .unwrap_or(QValue::Null);
                    (c, v)
                })
                .collect()
        }
    }
}

fn fill_of(
    spec: &TotalsSpec,
    names: usize,
    fill: &str,
    parents: &[String],
) -> Result<bool, WalkError> {
    let items: Vec<&str> = fill
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return Ok(false);
    }
    if items.len() != names.max(1) {
        return Err(WalkError::DifferentDimensionCount);
    }
    let all = |s: &str| fold_name(s) == fold_name("ВСЕ") || fold_name(s) == fold_name("ALL");
    for n in &items {
        if all(n) {
            continue;
        }
        let want = fold_name(n);
        let grouping = spec
            .points
            .iter()
            .any(|p| p.column.is_some() && fold_name(&p.name) == want);
        if !grouping {
            return Err(WalkError::DimensionNotFound((*n).to_string()));
        }
        if !parents.contains(&want) {
            return Err(WalkError::DimensionNotFoundInParents((*n).to_string()));
        }
    }
    Ok(items.first().is_some_and(|n| all(n)))
}

fn items_for(
    result: &QueryResult,
    scope: Option<&Item>,
    parents: &[String],
    walk: Walk,
    names: &str,
    fill: &str,
    coll: &dyn Collation,
) -> Result<Vec<Item>, WalkError> {
    let names: Vec<&str> = names
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let Some(main) = &result.totals else {
        if let Some(n) = names.first() {
            return Err(WalkError::GroupingNotFound((*n).to_string()));
        }
        return Ok(match scope {
            None => (0..result.rows.len())
                .filter_map(|i| u32::try_from(i).ok())
                .map(Item::Row)
                .collect(),
            Some(_) => Vec::new(),
        });
    };
    let (tree_ref, scope_g): (Option<Arc<TotalsTree>>, Option<usize>) = match scope {
        Some(Item::Rec { tree, .. }) => (
            tree.clone(),
            scope.and_then(|s| grouping_of(result, s)).map(|x| x.1),
        ),
        Some(Item::Virtual(v)) => (v.tree.clone(), Some(usize::from(v.grouping))),
        _ => (None, None),
    };
    let tree: &TotalsTree = tree_ref.as_deref().unwrap_or(main);
    let points = &tree.spec.points;
    let find = |n: &str| {
        let want = fold_name(n);
        points.iter().position(|p| fold_name(&p.name) == want)
    };
    let mut target: Option<usize> = None;
    for n in &names {
        let g = find(n).ok_or_else(|| WalkError::GroupingNotFound((*n).to_string()))?;
        target.get_or_insert(g);
    }
    let fill_all = fill_of(&main.spec, names.len(), fill, parents)?;
    let next = scope_g.map_or(0, |g| g + 1);
    let level = scope.map_or(0, |s| level_of(result, s) + 1);
    let scope_rows = || -> Vec<u32> {
        match scope {
            Some(s) => details_of(result, s),
            None => (0..result.rows.len())
                .filter_map(|i| u32::try_from(i).ok())
                .collect(),
        }
    };
    let scope_ctx = || -> Vec<(usize, QValue)> {
        match scope {
            Some(s) => ctx_of(result, s),
            None => Vec::new(),
        }
    };
    let jump = |t: usize| -> Result<(Vec<Item>, FillTarget), WalkError> {
        if names.len() > 1 {
            let items = flat_jump(
                result,
                tree,
                &tree_ref,
                t,
                &scope_rows(),
                &scope_ctx(),
                level,
                coll,
            )?;
            return Ok((items, Some((tree_ref.clone(), t))));
        }
        let sub = jump_tree(
            result,
            tree,
            t,
            scope_g,
            &scope_rows(),
            &scope_ctx(),
            level,
            coll,
        )?;
        let sub = Arc::new(sub);
        let items = sub
            .roots()
            .map(|id| Item::Rec {
                tree: Some(Arc::clone(&sub)),
                id,
            })
            .collect();
        Ok((items, Some((Some(sub), 0))))
    };
    let (items, filled): (Vec<Item>, FillTarget) = match scope {
        Some(Item::Row(_)) => (Vec::new(), None),
        Some(s @ Item::Rec { .. }) if kind_of(result, s) == RecordType::Detail => {
            (Vec::new(), None)
        }
        Some(Item::Virtual(v)) => match target {
            Some(t) if t > usize::from(v.grouping) => jump(t)?,
            _ => (Vec::new(), None),
        },
        Some(s @ Item::Rec { id, .. }) if kind_of(result, s) == RecordType::HierarchyTotal => {
            let g = scope_g.unwrap_or(0);
            match target {
                Some(t) if t == g && walk == Walk::ByGroupsWithHierarchy => (
                    tree.children(*id)
                        .map(|c| Item::Rec {
                            tree: tree_ref.clone(),
                            id: c,
                        })
                        .collect(),
                    Some((tree_ref.clone(), t)),
                ),
                Some(t) if t > g => jump(t)?,
                _ => (Vec::new(), None),
            }
        }
        _ => {
            let t = target.unwrap_or(next);
            let children: Vec<u32> = match scope {
                Some(Item::Rec { id, .. }) => tree.children(*id).collect(),
                _ => tree.roots().collect(),
            };
            if t == next {
                let ids = match walk {
                    Walk::Direct => preorder(tree, children),
                    Walk::ByGroups if next < points.len() => flatten(tree, children, t),
                    _ => children,
                };
                (
                    ids.into_iter()
                        .map(|id| Item::Rec {
                            tree: tree_ref.clone(),
                            id,
                        })
                        .collect(),
                    Some((tree_ref.clone(), t)),
                )
            } else if t > next {
                jump(t)?
            } else {
                (Vec::new(), None)
            }
        }
    };
    let mut items = items;
    if fill_all && let Some((fill_tree, t)) = filled {
        let ft: &TotalsTree = fill_tree.as_deref().unwrap_or(main);
        let added = fill_periods(result, ft, &fill_tree, t, &items, &scope_ctx(), level, coll)?;
        items.extend(added);
    }
    Ok(items)
}

#[allow(clippy::too_many_arguments)]
fn jump_tree(
    result: &QueryResult,
    tree: &TotalsTree,
    t: usize,
    scope_g: Option<usize>,
    details: &[u32],
    ctx: &[(usize, QValue)],
    level: u32,
    coll: &dyn Collation,
) -> Result<TotalsTree, WalkError> {
    let n = tree.spec.points.len();
    let after: Vec<usize> = (t + 1..n).collect();
    let rest: Vec<usize> = if after.is_empty() {
        (scope_g.map_or(0, |g| g + 1)..t).collect()
    } else {
        after
    };
    let order: Vec<usize> = std::iter::once(t).chain(rest).collect();
    let mut spec = tree.spec.clone();
    spec.points = order
        .iter()
        .filter_map(|p| tree.spec.points.get(*p).cloned())
        .collect();
    spec.order = tree
        .spec
        .order
        .iter()
        .map(|k| match k.source {
            KeySource::FirstRow { column, point } => OrderKey {
                source: match order.iter().position(|p| *p == point) {
                    Some(at) => KeySource::FirstRow { column, point: at },
                    None => KeySource::Null,
                },
                desc: k.desc,
            },
            _ => *k,
        })
        .collect();
    let sub = build_sub(
        &spec,
        &result.rows,
        details,
        ctx,
        level,
        coll,
        tree.max_number_limbs,
    )?;
    check(&sub)?;
    Ok(sub)
}

#[allow(clippy::too_many_arguments)]
fn flat_jump(
    result: &QueryResult,
    tree: &TotalsTree,
    tree_ref: &Option<Arc<TotalsTree>>,
    t: usize,
    details: &[u32],
    ctx: &[(usize, QValue)],
    level: u32,
    coll: &dyn Collation,
) -> Result<Vec<Item>, WalkError> {
    let Some(point) = tree.spec.points.get(t) else {
        return Ok(Vec::new());
    };
    let grouping = u16::try_from(t).map_err(|_| WalkError::Later("too many groupings"))?;
    if details.is_empty() {
        return Ok(Vec::new());
    }
    let div = std::cell::Cell::new(false);
    let cell = |r: u32, c: usize| result.rows.get(r as usize).and_then(|x| x.get(c));
    let make = |kind: RecordType,
                rows: Vec<u32>,
                ctx: Vec<(usize, QValue)>|
     -> Result<(Item, Vec<QValue>), WalkError> {
        let aggs = aggregate(&tree.spec.aggs, &rows, cell, coll, tree.max_number_limbs)?;
        let row = total_values(&tree.spec, &aggs, &ctx, coll, tree.max_number_limbs, &div)?;
        let first = rows.first().and_then(|r| result.rows.get(*r as usize));
        let keys = record_keys(&tree.spec.order, t, &row, first);
        Ok((
            Item::Virtual(Arc::new(Virtual {
                kind,
                tree: tree_ref.clone(),
                grouping,
                level,
                row,
                details: rows,
                ctx,
            })),
            keys,
        ))
    };
    let Some(col) = point.column else {
        let (item, _) = make(RecordType::Overall, details.to_vec(), ctx.to_vec())?;
        return Ok(vec![item]);
    };
    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut groups: Vec<(QValue, Vec<u32>)> = Vec::new();
    for &r in details {
        let v = cell(r, col).cloned().unwrap_or(QValue::Null);
        let k = key_of_totals(&v, coll);
        match index.get(&k) {
            Some(&g) => {
                if let Some(g) = groups.get_mut(g) {
                    g.1.push(r);
                }
            }
            None => {
                index.insert(k, groups.len());
                groups.push((v, vec![r]));
            }
        }
    }
    let mut made = Vec::with_capacity(groups.len());
    for (v, rows) in groups {
        made.push(make(RecordType::GroupTotal, rows, with(ctx, col, &v))?);
    }
    if div.get() {
        return Err(WalkError::DivisionByZero);
    }
    if !tree.spec.order.is_empty() {
        made.sort_by(|a, b| compare_keys(&a.1, &b.1, &tree.spec.order, coll));
    }
    Ok(made.into_iter().map(|(item, _)| item).collect())
}

fn period_start(d: BslDate, unit: Kw, week_start: u32) -> BslDate {
    if unit == Kw::Week {
        let day = BslDate::TICKS_PER_SECOND * 86_400;
        let start = d.ticks() - d.ticks().rem_euclid(day);
        let back = (i64::from(d.day_of_week()) - i64::from(week_start)).rem_euclid(7);
        return BslDate::from_ticks(start - back * day).unwrap_or(BslDate::EMPTY);
    }
    query_ir::fold::begin_of_period(d, unit)
}

fn next_period(p: BslDate, unit: Kw, week_start: u32) -> Option<BslDate> {
    let n = if unit == Kw::Week {
        BslDate::from_ticks(p.ticks() + 7 * 86_400 * BslDate::TICKS_PER_SECOND)?
    } else {
        let end = query_ir::fold::end_of_period(p, unit).ok()?;
        period_start(
            BslDate::from_ticks(end.ticks() + BslDate::TICKS_PER_SECOND)?,
            unit,
            week_start,
        )
    };
    (n > p).then_some(n)
}

#[allow(clippy::too_many_arguments)]
fn fill_periods(
    result: &QueryResult,
    tree: &TotalsTree,
    tree_ref: &Option<Arc<TotalsTree>>,
    t: usize,
    items: &[Item],
    ctx: &[(usize, QValue)],
    level: u32,
    coll: &dyn Collation,
) -> Result<Vec<Item>, WalkError> {
    let Some(point) = tree.spec.points.get(t) else {
        return Ok(Vec::new());
    };
    let (Some(per), Some(col)) = (point.periods, point.column) else {
        return Ok(Vec::new());
    };
    let grouping = u16::try_from(t).map_err(|_| WalkError::Later("too many groupings"))?;
    let mut present: HashSet<i64> = HashSet::new();
    let (mut least, mut greatest): (Option<BslDate>, Option<BslDate>) = (None, None);
    for item in items {
        if point_of(result, item).map(|p| &p.name) != Some(&point.name) {
            continue;
        }
        let v = if col < tree.spec.width {
            row_of(result, item).and_then(|r| r.get(col)).cloned()
        } else {
            details_of(result, item)
                .first()
                .and_then(|i| result.rows.get(*i as usize))
                .and_then(|r| r.get(col))
                .cloned()
        };
        if let Some(QValue::Date(d)) = v {
            present.insert(d.ticks());
            least = Some(least.map_or(d, |l| l.min(d)));
            greatest = Some(greatest.map_or(d, |g| g.max(d)));
        }
    }
    let (Some(begin), Some(end)) = (per.begin.or(least), per.end.or(greatest)) else {
        return Ok(Vec::new());
    };
    let nulls = vec![QValue::Null; tree.spec.aggs.len()];
    let div = std::cell::Cell::new(false);
    let mut out = Vec::new();
    let mut p = period_start(begin, per.unit, per.week_start);
    while p <= end {
        if !present.contains(&p.ticks()) {
            if out.len() >= MAX_FILL {
                return Err(WalkError::Later("ПЕРИОДАМИ: слишком много периодов"));
            }
            let inner = with(ctx, col, &QValue::Date(p));
            let row = total_values(
                &tree.spec,
                &nulls,
                &inner,
                coll,
                tree.max_number_limbs,
                &div,
            )?;
            out.push(Item::Virtual(Arc::new(Virtual {
                kind: RecordType::GroupTotal,
                tree: tree_ref.clone(),
                grouping,
                level,
                row,
                details: Vec::new(),
                ctx: inner,
            })));
        }
        match next_period(p, per.unit, per.week_start) {
            Some(n) => p = n,
            None => break,
        }
    }
    Ok(out)
}

fn unload_tree(tree: &TotalsTree, walk: Walk) -> Vec<TreeRow> {
    let points = tree.spec.points.len();
    let kids = |scope: Option<u32>| -> Vec<u32> {
        let rec = scope.and_then(|id| tree.records.get(id as usize));
        if let Some(r) = rec {
            match r.kind {
                RecordType::Detail => return Vec::new(),
                RecordType::HierarchyTotal => {
                    return match (walk, scope) {
                        (Walk::ByGroupsWithHierarchy, Some(id)) => tree.children(id).collect(),
                        _ => Vec::new(),
                    };
                }
                _ => {}
            }
        }
        let next = rec
            .and_then(|r| r.grouping)
            .map_or(0, |g| usize::from(g) + 1);
        let children: Vec<u32> = match scope {
            Some(id) => tree.children(id).collect(),
            None => tree.roots().collect(),
        };
        if walk == Walk::ByGroups && next < points {
            flatten(tree, children, next)
        } else {
            children
        }
    };
    let mut stack: Vec<Frame> = vec![Frame {
        id: None,
        kids: kids(None),
        next: 0,
        built: Vec::new(),
    }];
    loop {
        let Some(top) = stack.last_mut() else {
            return Vec::new();
        };
        if let Some(&id) = top.kids.get(top.next) {
            top.next += 1;
            let k = kids(Some(id));
            stack.push(Frame {
                id: Some(id),
                kids: k,
                next: 0,
                built: Vec::new(),
            });
            continue;
        }
        let Some(done) = stack.pop() else {
            return Vec::new();
        };
        let Some(id) = done.id else {
            return done.built;
        };
        let Some(r) = tree.records.get(id as usize) else {
            continue;
        };
        let row = TreeRow {
            row: r.row,
            children: done.built,
        };
        if let Some(parent) = stack.last_mut() {
            parent.built.push(row);
        }
    }
}

struct Frame {
    id: Option<u32>,
    kids: Vec<u32>,
    next: usize,
    built: Vec<TreeRow>,
}
