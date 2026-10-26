mod access;
mod joins;
mod temp;

use std::fmt::Write as _;
use std::ops::Bound;
use std::sync::Arc;

use ibschema::storage::{FieldId, IndexId, StorageMap, TableId};
use query_ir::{AggL, ExecError, PExpr, StmtL};
use query_syntax::Func;
use query_syntax::ast::JoinKind;

use crate::ops::{
    BoxOp, Distinct, Empty, Filter, HashAggregate, HashJoin, HierarchyClosure, HierarchyOrder,
    IndexJoin, IndexLookup, IndexRange, Limit, NestedLoopJoin, Project, Scan, SemiJoin, Sort,
    TempRange, TempScan, TopN, ValuesScan, union,
};
use crate::source::DataSource;
use crate::temp::{TempTableId, TempTables};

pub use access::{IdLookup, IndexPoints};

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanOptions {
    pub force: Option<Force>,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Force {
    Scan,
    Index,
    Hash,
    NestedLoop,
}

#[derive(Clone, Debug)]
pub enum ClosureSeeds {
    Values(Vec<PExpr>),
    Plan(Box<Plan>),
}

#[derive(Clone, Debug)]
pub enum Lookup {
    Id(PExpr),
    Prefix {
        index: IndexId,
        index_name: String,
        keys: Vec<PExpr>,
        skip: usize,
    },
}

#[derive(Clone, Debug)]
pub enum Plan {
    Empty,
    Scan {
        table: TableId,
        name: String,
        fields: Vec<FieldId>,
    },
    IdLookup {
        table: TableId,
        name: String,
        ids: Vec<PExpr>,
        fields: Vec<FieldId>,
    },
    IndexRange {
        table: TableId,
        name: String,
        index: IndexId,
        index_name: String,
        lo: Bound<Vec<PExpr>>,
        hi: Bound<Vec<PExpr>>,
        fields: Vec<FieldId>,
        skip: usize,
    },
    IndexPoints {
        table: TableId,
        name: String,
        index: IndexId,
        index_name: String,
        prefix: Vec<PExpr>,
        values: Vec<PExpr>,
        fields: Vec<FieldId>,
        skip: usize,
    },
    Values {
        rows: Arc<query_ir::ValueRows>,
    },
    RegisterRows {
        step: Box<query_ir::lower::RegisterRowsL>,
        #[doc(hidden)]
        observation: Option<crate::observe::RegisterObservation>,
    },
    Slice {
        input: Box<Plan>,
        totals: Option<Box<Plan>>,
        step: Box<query_ir::lower::SliceL>,
        observation: Option<crate::observe::RegisterObservation>,
    },
    RunningBalance {
        input: Box<Plan>,
        step: Box<query_ir::lower::RunningBalanceL>,
    },
    TempScan {
        table: TempTableId,
        name: String,
    },
    TempIndexRange {
        table: TempTableId,
        name: String,
        index: usize,
        index_name: String,
        lo: Bound<Vec<PExpr>>,
        hi: Bound<Vec<PExpr>>,
    },
    CheckAccess {
        input: Box<Plan>,
        signal: u16,
    },
    Filter {
        input: Box<Plan>,
        pred: PExpr,
    },
    Project {
        input: Box<Plan>,
        exprs: Vec<PExpr>,
    },
    Aggregate {
        input: Box<Plan>,
        keys: Vec<PExpr>,
        sets: Option<Vec<Vec<u16>>>,
        aggs: Vec<AggL>,
    },
    Distinct(Box<Plan>),
    Sort {
        input: Box<Plan>,
        keys: Vec<(PExpr, bool)>,
    },
    TopN {
        input: Box<Plan>,
        keys: Vec<(PExpr, bool)>,
        n: u64,
    },
    Limit {
        input: Box<Plan>,
        n: u64,
    },
    Union {
        parts: Vec<Plan>,
        all: Vec<bool>,
    },
    SemiJoin {
        input: Box<Plan>,
        l: Vec<PExpr>,
        q: Box<Plan>,
        not: bool,
        correlated: bool,
    },
    Hierarchy {
        input: Box<Plan>,
        id: u16,
        parent: u16,
        keys: Vec<(PExpr, bool)>,
    },
    HierarchyClosure {
        tables: Vec<query_ir::HierTable>,
        seeds: ClosureSeeds,
        index: bool,
    },
    NestedLoopJoin {
        kind: JoinKind,
        l: Box<Plan>,
        r: Box<Plan>,
        on: PExpr,
        lw: usize,
        rw: usize,
    },
    HashJoin {
        kind: JoinKind,
        l: Box<Plan>,
        r: Box<Plan>,
        lk: Vec<PExpr>,
        rk: Vec<PExpr>,
        on: PExpr,
        lw: usize,
        rw: usize,
    },
    IndexJoin {
        kind: JoinKind,
        l: Box<Plan>,
        table: TableId,
        name: String,
        lookup: Lookup,
        fields: Vec<FieldId>,
        on: PExpr,
    },
    ValuesIndexJoin {
        values: Box<Plan>,
        table: TableId,
        name: String,
        index: IndexId,
        index_name: String,
        keys: Vec<PExpr>,
        skip: usize,
        fields: Vec<FieldId>,
        on: PExpr,
        vw: usize,
    },
    TempIndexJoin {
        kind: JoinKind,
        l: Box<Plan>,
        table: TempTableId,
        name: String,
        index: usize,
        index_name: String,
        keys: Vec<PExpr>,
        width: u16,
        on: PExpr,
    },
}

pub(crate) fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("plan: {what}"))
}

pub fn plan(stmt: &StmtL, src: &dyn DataSource) -> Result<Plan, ExecError> {
    plan_with(stmt, src, PlanOptions::default())
}

#[doc(hidden)]
pub fn plan_with(stmt: &StmtL, src: &dyn DataSource, opts: PlanOptions) -> Result<Plan, ExecError> {
    plan_temps(stmt, src, None, opts)
}

#[doc(hidden)]
pub fn plan_temps(
    stmt: &StmtL,
    src: &dyn DataSource,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<Plan, ExecError> {
    plan_rel(&stmt.root, stmt.max_depth, src, temps, opts)
}

#[doc(hidden)]
pub fn plan_rel(
    rel: &query_ir::RelL,
    max_depth: u32,
    src: &dyn DataSource,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<Plan, ExecError> {
    let map = src.map();
    query_syntax::with_deep_stack(max_depth, move || {
        Planner {
            map,
            temps,
            opts,
            drive_values: false,
        }
        .rel(rel, false, Vec::new())
    })
}

pub(crate) fn plan_rel_driven(
    rel: &query_ir::RelL,
    max_depth: u32,
    src: &dyn DataSource,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<Plan, ExecError> {
    let map = src.map();
    query_syntax::with_deep_stack(max_depth, move || {
        Planner {
            map,
            temps,
            opts,
            drive_values: true,
        }
        .rel(rel, false, Vec::new())
    })
}

pub fn explain(stmt: &StmtL, src: &dyn DataSource) -> String {
    explain_with(stmt, src, PlanOptions::default())
}

#[doc(hidden)]
pub fn explain_with(stmt: &StmtL, src: &dyn DataSource, opts: PlanOptions) -> String {
    explain_temps(stmt, src, None, opts)
}

pub fn explain_temps(
    stmt: &StmtL,
    src: &dyn DataSource,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> String {
    let planned = plan_temps(stmt, src, temps, opts);
    query_syntax::with_deep_stack(stmt.max_depth, move || match planned {
        Ok(p) => {
            let mut s = String::new();
            p.show(0, &mut s);
            s
        }
        Err(e) => format!("error: {e:?}"),
    })
}

pub(crate) struct Planner<'m> {
    pub(crate) map: &'m StorageMap,
    pub(crate) temps: Option<&'m TempTables>,
    pub(crate) opts: PlanOptions,
    pub(crate) drive_values: bool,
}

impl Planner<'_> {
    fn rel(&self, r: &query_ir::RelL, rep: bool, pending: Vec<PExpr>) -> Result<Plan, ExecError> {
        use query_ir::RelL as R;
        Ok(match r {
            R::Empty => wrap(Plan::Empty, pending),
            R::Scan { table, fields } => self.access(*table, fields, rep, pending)?,
            R::TempScan { name, .. } => self.temp_access(name, pending)?,
            R::Values { rows } => wrap(Plan::Values { rows: rows.clone() }, pending),
            R::Slice {
                input,
                totals,
                step,
            } => wrap(
                Plan::Slice {
                    input: Box::new(self.rel(input, false, Vec::new())?),
                    totals: totals
                        .as_ref()
                        .map(|t| self.rel(t, false, Vec::new()).map(Box::new))
                        .transpose()?,
                    step: step.clone(),
                    observation: None,
                },
                pending,
            ),
            R::RegisterRows(step) => wrap(
                Plan::RegisterRows {
                    step: step.clone(),
                    observation: None,
                },
                pending,
            ),
            R::RunningBalance { input, step } => wrap(
                Plan::RunningBalance {
                    input: Box::new(self.rel(input, false, Vec::new())?),
                    step: step.clone(),
                },
                pending,
            ),
            R::CheckAccess { input, signal } => wrap(
                Plan::CheckAccess {
                    input: Box::new(self.rel(input, rep, Vec::new())?),
                    signal: *signal,
                },
                pending,
            ),
            R::Filter { input, pred } => {
                let mut p = pending;
                conjuncts(pred, &mut p);
                self.rel(input, rep, p)?
            }
            R::Join {
                kind,
                l,
                r,
                on,
                eq,
                by_id,
            } => self.join(*kind, l, r, on, eq, *by_id, rep, pending)?,
            R::Project { input, exprs } => wrap(
                Plan::Project {
                    input: Box::new(self.rel(input, rep, Vec::new())?),
                    exprs: exprs.clone(),
                },
                pending,
            ),
            R::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => {
                let picks =
                    !keys.is_empty() || aggs.iter().any(|a| matches!(a.f, Func::Min | Func::Max));
                wrap(
                    Plan::Aggregate {
                        input: Box::new(self.rel(input, picks, Vec::new())?),
                        keys: keys.clone(),
                        sets: sets.clone(),
                        aggs: aggs.clone(),
                    },
                    pending,
                )
            }
            R::Distinct(x) => wrap(
                Plan::Distinct(Box::new(self.rel(x, true, Vec::new())?)),
                pending,
            ),
            R::Sort { input, keys } => wrap(
                Plan::Sort {
                    input: Box::new(self.rel(input, rep, Vec::new())?),
                    keys: keys.clone(),
                },
                pending,
            ),
            R::Top { input, n } => {
                let p = if let R::Sort { input: si, keys } = &**input {
                    Plan::TopN {
                        input: Box::new(self.rel(si, rep, Vec::new())?),
                        keys: keys.clone(),
                        n: *n,
                    }
                } else {
                    Plan::Limit {
                        input: Box::new(self.rel(input, rep, Vec::new())?),
                        n: *n,
                    }
                };
                wrap(p, pending)
            }
            R::Union { parts, all } => {
                let rep = rep || all.iter().any(|a| !a);
                let mut ps = Vec::with_capacity(parts.len());
                for p in parts {
                    ps.push(self.rel(p, rep, Vec::new())?);
                }
                wrap(
                    Plan::Union {
                        parts: ps,
                        all: all.clone(),
                    },
                    pending,
                )
            }
            R::SemiJoin {
                input,
                l,
                q,
                not,
                correlated,
            } => Plan::SemiJoin {
                input: Box::new(self.rel(input, rep, pending)?),
                l: l.clone(),
                q: Box::new(self.rel(q, false, Vec::new())?),
                not: *not,
                correlated: *correlated,
            },
            R::HierarchyClosure { tables, seeds } => {
                let seeds = match seeds {
                    query_ir::HierSeeds::Values(v) => ClosureSeeds::Values(v.clone()),
                    query_ir::HierSeeds::Rel(r) => {
                        ClosureSeeds::Plan(Box::new(self.rel(r, false, Vec::new())?))
                    }
                };
                wrap(
                    Plan::HierarchyClosure {
                        tables: tables.clone(),
                        seeds,
                        index: self.use_index(),
                    },
                    pending,
                )
            }
            R::HierarchyOrder {
                input,
                id,
                parent,
                keys,
            } => wrap(
                Plan::Hierarchy {
                    input: Box::new(self.rel(input, rep, Vec::new())?),
                    id: *id,
                    parent: *parent,
                    keys: keys.clone(),
                },
                pending,
            ),
        })
    }

    pub(crate) fn use_index(&self) -> bool {
        matches!(self.opts.force, None | Some(Force::Index))
    }

    pub(crate) fn table_name(&self, t: TableId) -> String {
        match self.map.try_table(t) {
            Some(i) if !i.query_name.trim().is_empty() => i.query_name.clone(),
            Some(i) => i.logical.clone(),
            None => format!("table#{}", t.0),
        }
    }
}

pub(crate) fn wrap(p: Plan, mut pending: Vec<PExpr>) -> Plan {
    match pending.len() {
        0 => p,
        1 => Plan::Filter {
            input: Box::new(p),
            pred: pending.remove(0),
        },
        _ => Plan::Filter {
            input: Box::new(p),
            pred: PExpr::And(pending),
        },
    }
}

pub(crate) fn conjuncts(e: &PExpr, out: &mut Vec<PExpr>) {
    match e {
        PExpr::And(v) => v.iter().for_each(|x| conjuncts(x, out)),
        other => out.push(other.clone()),
    }
}

pub(crate) fn walk(e: &PExpr, f: &mut dyn FnMut(&PExpr)) {
    f(e);
    let mut go = |x: &PExpr| walk(x, f);
    match e {
        PExpr::Col(_) | PExpr::Outer { .. } | PExpr::Lit(_) | PExpr::Set(_) | PExpr::Unit(_) => {}
        PExpr::Neg(x) | PExpr::Not(x) => go(x),
        PExpr::Arith { first, steps, .. } => {
            go(first);
            steps.iter().for_each(|(_, x, _)| go(x));
        }
        PExpr::And(v) | PExpr::Or(v) => v.iter().for_each(go),
        PExpr::Compare(_, a, b) => {
            go(a);
            go(b);
        }
        PExpr::InList { l, rhs, .. } => {
            l.iter().for_each(&mut go);
            rhs.iter().for_each(go);
        }
        PExpr::InQuery { l, .. } => l.iter().for_each(go),
        PExpr::SectionAgg { owner, .. } => go(owner),
        PExpr::Between { l, lo, hi, .. } => {
            go(l);
            go(lo);
            go(hi);
        }
        PExpr::IsNull { l, .. } | PExpr::Refs { l, .. } => go(l),
        PExpr::Like { l, pat, .. } => {
            go(l);
            go(pat);
        }
        PExpr::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                go(o);
            }
            for (w, t) in whens {
                go(w);
                go(t);
            }
            if let Some(x) = else_ {
                go(x);
            }
        }
        PExpr::Cast { x, .. } | PExpr::TypeOf { x, .. } | PExpr::StoredSize { x, .. } => go(x),
        PExpr::Func { args, .. } => args.iter().for_each(go),
    }
}

pub(crate) fn cols_end(e: &PExpr) -> usize {
    let mut end = 0usize;
    walk(e, &mut |x| match x {
        PExpr::Col(c) => end = end.max(usize::from(*c) + 1),
        PExpr::InQuery {
            correlated: true, ..
        } => end = usize::MAX,
        _ => {}
    });
    end
}

pub(crate) fn width(r: &query_ir::RelL) -> usize {
    use query_ir::RelL as R;
    match r {
        R::Empty => 0,
        R::Scan { fields, .. } => fields.len(),
        R::TempScan { width, .. } => usize::from(*width),
        R::Values { rows } => rows.width,
        R::Join { l, r, .. } => width(l) + width(r),
        R::CheckAccess { input, .. }
        | R::Filter { input, .. }
        | R::Sort { input, .. }
        | R::Top { input, .. }
        | R::SemiJoin { input, .. }
        | R::HierarchyOrder { input, .. } => width(input),
        R::Distinct(x) => width(x),
        R::Project { exprs, .. } => exprs.len(),
        R::Aggregate { keys, aggs, .. } => keys.len() + aggs.len(),
        R::Union { parts, .. } => parts.first().map_or(0, width),
        R::HierarchyClosure { .. } => 1,
        R::RunningBalance { step, .. } => step.width(),
        R::RegisterRows(s) => query_ir::ir::raw_layout::width(s.dims.len(), s.resources.len()),
        R::Slice { step, .. } => step.width,
    }
}

fn skip_text(skip: usize) -> String {
    if skip == 0 {
        String::new()
    } else {
        format!(" skip {skip}")
    }
}

pub fn build<'a>(p: &Plan) -> BoxOp<'a> {
    match p {
        Plan::Empty => Box::new(Empty::new()),
        Plan::Scan { table, fields, .. } => Box::new(Scan::new(*table, fields.clone())),
        Plan::TempScan { table, .. } => Box::new(TempScan::new(*table)),
        Plan::Values { rows } => Box::new(ValuesScan::new(rows.clone())),
        Plan::Slice {
            input,
            totals,
            step,
            observation,
        } => Box::new(crate::ops::slice::Slice::new(
            (**step).clone(),
            (**input).clone(),
            totals.as_ref().map(|t| (**t).clone()),
            observation.clone(),
        )),
        Plan::RegisterRows { step, observation } => Box::new(
            crate::ops::register::RegisterRows::observed((**step).clone(), observation.clone()),
        ),
        Plan::TempIndexRange {
            table,
            index,
            lo,
            hi,
            ..
        } => Box::new(TempRange::new(*table, *index, lo.clone(), hi.clone())),
        Plan::IdLookup {
            table, ids, fields, ..
        } => Box::new(IdLookup::new(*table, ids.clone(), fields.clone())),
        Plan::IndexRange {
            table,
            index,
            lo,
            hi,
            fields,
            skip,
            ..
        } => Box::new(
            IndexRange::new(
                *table,
                *index,
                lo.clone(),
                hi.clone(),
                false,
                fields.clone(),
            )
            .skipping(*skip),
        ),
        Plan::IndexPoints {
            table,
            index,
            prefix,
            values,
            fields,
            skip,
            ..
        } => Box::new(
            IndexPoints::new(
                *table,
                *index,
                prefix.clone(),
                values.clone(),
                fields.clone(),
            )
            .skipping(*skip),
        ),
        Plan::RunningBalance { input, step } => Box::new(crate::ops::running::RunningBalance::new(
            build(input),
            (**step).clone(),
        )),
        Plan::CheckAccess { input, signal } => {
            Box::new(crate::ops::CheckAccess::new(build(input), *signal))
        }
        Plan::Filter { input, pred } => Box::new(Filter::new(build(input), pred.clone())),
        Plan::Project { input, exprs } => Box::new(Project::new(build(input), exprs.clone())),
        Plan::Aggregate {
            input,
            keys,
            sets,
            aggs,
        } => Box::new(HashAggregate::new(
            build(input),
            keys.clone(),
            sets.clone(),
            aggs.clone(),
        )),
        Plan::Distinct(x) => Box::new(Distinct::new(build(x))),
        Plan::Sort { input, keys } => Box::new(Sort::new(build(input), keys.clone())),
        Plan::TopN { input, keys, n } => Box::new(TopN::new(build(input), keys.clone(), *n)),
        Plan::Limit { input, n } => Box::new(Limit::new(build(input), *n)),
        Plan::Union { parts, all } => union(parts.iter().map(build).collect(), all),
        Plan::SemiJoin {
            input,
            l,
            q,
            not,
            correlated,
        } => {
            if *correlated {
                let q = (**q).clone();
                Box::new(SemiJoin::correlated(
                    build(input),
                    l.clone(),
                    Box::new(move || build(&q)),
                    *not,
                ))
            } else {
                Box::new(SemiJoin::hashed(build(input), l.clone(), build(q), *not))
            }
        }
        Plan::HierarchyClosure {
            tables,
            seeds,
            index,
        } => Box::new(HierarchyClosure::new(
            tables.clone(),
            *index,
            match seeds {
                ClosureSeeds::Values(v) => crate::ops::hier::Seeds::Values(v.clone()),
                ClosureSeeds::Plan(p) => crate::ops::hier::Seeds::Rel(Some(build(p))),
            },
        )),
        Plan::Hierarchy {
            input,
            id,
            parent,
            keys,
        } => Box::new(HierarchyOrder::new(
            build(input),
            *id,
            *parent,
            keys.clone(),
        )),
        Plan::NestedLoopJoin {
            kind,
            l,
            r,
            on,
            lw,
            rw,
        } => Box::new(NestedLoopJoin::new(
            *kind,
            build(l),
            build(r),
            Some(on.clone()),
            *lw,
            *rw,
        )),
        Plan::HashJoin {
            kind,
            l,
            r,
            lk,
            rk,
            on,
            lw,
            rw,
        } => Box::new(HashJoin::new(
            *kind,
            build(l),
            build(r),
            lk.clone(),
            rk.clone(),
            Some(on.clone()),
            *lw,
            *rw,
        )),
        Plan::TempIndexJoin {
            kind,
            l,
            table,
            index,
            keys,
            width,
            on,
            ..
        } => Box::new(IndexJoin::new(
            *kind,
            build(l),
            IndexLookup::Temp {
                table: *table,
                index: *index,
                keys: keys.clone(),
            },
            (0..*width).map(FieldId).collect(),
            Some(on.clone()),
        )),
        Plan::IndexJoin {
            kind,
            l,
            table,
            lookup,
            fields,
            on,
            ..
        } => {
            let lookup = match lookup {
                Lookup::Id(key) => IndexLookup::Id {
                    table: *table,
                    key: key.clone(),
                },
                Lookup::Prefix {
                    index, keys, skip, ..
                } => IndexLookup::Prefix {
                    table: *table,
                    index: *index,
                    keys: keys.clone(),
                    skip: *skip,
                },
            };
            Box::new(IndexJoin::new(
                *kind,
                build(l),
                lookup,
                fields.clone(),
                Some(on.clone()),
            ))
        }
        Plan::ValuesIndexJoin {
            values,
            table,
            index,
            keys,
            skip,
            fields,
            on,
            vw,
            ..
        } => Box::new(crate::ops::join::Swapped::new(
            Box::new(IndexJoin::new(
                JoinKind::Inner,
                build(values),
                IndexLookup::Prefix {
                    table: *table,
                    index: *index,
                    keys: keys.clone(),
                    skip: *skip,
                },
                fields.clone(),
                None,
            )),
            *vw,
            on.clone(),
        )),
    }
}

impl Plan {
    fn show(&self, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        let kids: Vec<&Plan>;
        let line = match self {
            Plan::Empty => {
                kids = vec![];
                "empty".to_string()
            }
            Plan::Scan { name, .. } => {
                kids = vec![];
                format!("scan {name}")
            }
            Plan::TempScan { name, .. } => {
                kids = vec![];
                format!("scan {name}")
            }
            Plan::Values { rows } => {
                kids = vec![];
                format!("values {}x{}", rows.rows.len(), rows.width)
            }
            Plan::Slice {
                input,
                totals,
                step,
                ..
            } => {
                kids = std::iter::once(&**input)
                    .chain(totals.iter().map(|t| &**t))
                    .collect();
                format!("slice first={}", step.first)
            }
            Plan::RunningBalance { input, .. } => {
                kids = vec![input];
                "running-balance".to_string()
            }
            Plan::RegisterRows { step, .. } => {
                kids = vec![];
                format!("register-rows {:?}", step.kind)
            }
            Plan::TempIndexRange {
                name, index_name, ..
            } => {
                kids = vec![];
                format!("index-range {index_name} on {name}")
            }
            Plan::TempIndexJoin {
                kind,
                l,
                name,
                index_name,
                keys,
                ..
            } => {
                kids = vec![l];
                format!(
                    "index-join {kind:?} {index_name} of {name} ({} key(s))",
                    keys.len()
                )
            }
            Plan::IdLookup { name, ids, .. } => {
                kids = vec![];
                format!("get-by-id {name} ({} key(s))", ids.len())
            }
            Plan::IndexRange {
                name,
                index_name,
                skip,
                ..
            } => {
                kids = vec![];
                format!("index-range {index_name} on {name}{}", skip_text(*skip))
            }
            Plan::IndexPoints {
                name,
                index_name,
                prefix,
                values,
                skip,
                ..
            } => {
                kids = vec![];
                format!(
                    "index-range {index_name} on {name} (points: {} prefix, {} value(s)){}",
                    prefix.len(),
                    values.len(),
                    skip_text(*skip)
                )
            }
            Plan::CheckAccess { input, signal } => {
                kids = vec![input];
                format!("check-access hidden={signal}")
            }
            Plan::Filter { input, .. } => {
                kids = vec![input];
                "filter".to_string()
            }
            Plan::Project { input, exprs } => {
                kids = vec![input];
                format!("project {}", exprs.len())
            }
            Plan::Aggregate { input, keys, .. } => {
                kids = vec![input];
                format!("hash-aggregate {} key(s)", keys.len())
            }
            Plan::Distinct(x) => {
                kids = vec![x];
                "distinct".to_string()
            }
            Plan::Sort { input, .. } => {
                kids = vec![input];
                "sort".to_string()
            }
            Plan::TopN { input, n, .. } => {
                kids = vec![input];
                format!("top-n {n}")
            }
            Plan::Limit { input, n } => {
                kids = vec![input];
                format!("limit {n}")
            }
            Plan::Union { parts, .. } => {
                kids = parts.iter().collect();
                "union".to_string()
            }
            Plan::SemiJoin {
                input,
                q,
                not,
                correlated,
                ..
            } => {
                kids = vec![input, q];
                format!(
                    "semi-join {}{}",
                    if *correlated { "correlated" } else { "hashed" },
                    if *not { " not" } else { "" }
                )
            }
            Plan::Hierarchy { input, .. } => {
                kids = vec![input];
                "hierarchy-order".to_string()
            }
            Plan::HierarchyClosure { tables, seeds, .. } => {
                kids = match seeds {
                    ClosureSeeds::Values(_) => Vec::new(),
                    ClosureSeeds::Plan(p) => vec![p],
                };
                format!("hierarchy-closure {} table(s)", tables.len())
            }
            Plan::NestedLoopJoin { kind, l, r, .. } => {
                kids = vec![l, r];
                format!("nested-loop-join {kind:?}")
            }
            Plan::HashJoin { kind, l, r, lk, .. } => {
                kids = vec![l, r];
                format!("hash-join {kind:?} ({} key(s))", lk.len())
            }
            Plan::IndexJoin {
                kind,
                l,
                name,
                lookup,
                ..
            } => {
                kids = vec![l];
                match lookup {
                    Lookup::Id(_) => format!("index-join {kind:?} ID of {name}"),
                    Lookup::Prefix {
                        index_name,
                        keys,
                        skip,
                        ..
                    } => format!(
                        "index-join {kind:?} {index_name} of {name} ({} key(s)){}",
                        keys.len(),
                        skip_text(*skip)
                    ),
                }
            }
            Plan::ValuesIndexJoin {
                values,
                name,
                index_name,
                keys,
                skip,
                ..
            } => {
                kids = vec![values];
                format!(
                    "values-index-join {index_name} of {name} ({} key(s)){}",
                    keys.len(),
                    skip_text(*skip)
                )
            }
        };
        let _ = writeln!(out, "{pad}{line}");
        for k in kids {
            k.show(depth + 1, out);
        }
    }
}
