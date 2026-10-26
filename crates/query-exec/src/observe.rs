use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::ops::register::SelectedRegister;
use crate::plan::{ClosureSeeds, Lookup, Plan};
use query_ir::{PExpr, RelL};

use ibschema::storage::TableId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationPhase {
    Precheck,
    Executing,
    Finished,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClosureStatus {
    Complete,
    TerminalPrecheck,
    Incomplete(Vec<UnresolvedNode>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedNode {
    pub path: Vec<u32>,
    pub reason: UnresolvedReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnresolvedReason {
    UnselectedRegister,
    DeferredSubplan,
    Interrupted,
    DiagnosticLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationReport {
    pub phase: ObservationPhase,
    pub closure: ClosureStatus,
    pub dependencies: BTreeSet<TableId>,
}

const MAX_NODES: usize = 4096;
const MAX_METADATA: usize = 1024 * 1024;

struct Recorder {
    phase: ObservationPhase,
    dependencies: BTreeSet<TableId>,
    unresolved: BTreeMap<Vec<u32>, UnresolvedReason>,
    deferred: BTreeMap<usize, Vec<Vec<u32>>>,
    published: BTreeMap<usize, Vec<u32>>,
    aliases: BTreeMap<Vec<u32>, Vec<u32>>,
    selected: BTreeMap<Vec<u32>, Rc<SelectedRegister>>,
    nodes: usize,
    bytes: usize,
    limited: bool,
    terminal: bool,
    max_nodes: usize,
    max_bytes: usize,
}

#[derive(Clone)]
pub(crate) struct ObservationSeed {
    report: ObservationReport,
    nodes: usize,
    bytes: usize,
    max_nodes: usize,
    max_bytes: usize,
    limited: bool,
}

#[derive(Clone)]
pub struct ObservationSession(Rc<RefCell<Recorder>>);

impl Default for ObservationSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservationSession {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Recorder {
            phase: ObservationPhase::Precheck,
            dependencies: BTreeSet::new(),
            unresolved: BTreeMap::new(),
            deferred: BTreeMap::new(),
            published: BTreeMap::new(),
            aliases: BTreeMap::new(),
            selected: BTreeMap::new(),
            nodes: 0,
            bytes: 0,
            limited: false,
            terminal: false,
            max_nodes: MAX_NODES,
            max_bytes: MAX_METADATA,
        })))
    }

    pub fn report(&self) -> ObservationReport {
        let r = self.0.borrow();
        let mut unresolved: Vec<_> = r
            .unresolved
            .iter()
            .map(|(path, reason)| UnresolvedNode {
                path: path.clone(),
                reason: *reason,
            })
            .collect();
        if r.limited {
            unresolved.push(UnresolvedNode {
                path: Vec::new(),
                reason: UnresolvedReason::DiagnosticLimit,
            });
        }
        let closure = if !unresolved.is_empty() {
            ClosureStatus::Incomplete(unresolved)
        } else if r.terminal {
            ClosureStatus::TerminalPrecheck
        } else if r.phase == ObservationPhase::Finished {
            ClosureStatus::Complete
        } else {
            ClosureStatus::Incomplete(vec![UnresolvedNode {
                path: Vec::new(),
                reason: UnresolvedReason::Interrupted,
            }])
        };
        ObservationReport {
            phase: r.phase,
            closure,
            dependencies: r.dependencies.clone(),
        }
    }

    #[doc(hidden)]
    pub fn with_limits(max_nodes: usize, max_bytes: usize) -> Self {
        let session = Self::new();
        {
            let mut r = session.0.borrow_mut();
            r.max_nodes = max_nodes.min(MAX_NODES);
            r.max_bytes = max_bytes.min(MAX_METADATA);
        }
        session
    }

    pub(crate) fn seed(&self) -> ObservationSeed {
        let r = self.0.borrow();
        ObservationSeed {
            report: self.report(),
            nodes: r.nodes,
            bytes: r.bytes,
            max_nodes: r.max_nodes,
            max_bytes: r.max_bytes,
            limited: r.limited,
        }
    }

    pub(crate) fn from_seed(seed: ObservationSeed) -> Self {
        let terminal = seed.report.closure == ClosureStatus::TerminalPrecheck;
        let unresolved = match seed.report.closure {
            ClosureStatus::Incomplete(nodes) => nodes
                .into_iter()
                .filter(|n| n.reason != UnresolvedReason::DiagnosticLimit)
                .map(|n| (n.path, n.reason))
                .collect(),
            ClosureStatus::Complete | ClosureStatus::TerminalPrecheck => BTreeMap::new(),
        };
        Self(Rc::new(RefCell::new(Recorder {
            phase: seed.report.phase,
            dependencies: seed.report.dependencies,
            unresolved,
            deferred: BTreeMap::new(),
            published: BTreeMap::new(),
            aliases: BTreeMap::new(),
            selected: BTreeMap::new(),
            nodes: seed.nodes,
            bytes: seed.bytes,
            max_nodes: seed.max_nodes,
            max_bytes: seed.max_bytes,
            limited: seed.limited,
            terminal,
        })))
    }

    pub(crate) fn begin_precheck(&self) {
        let (nodes, bytes) = {
            let r = self.0.borrow();
            (r.max_nodes, r.max_bytes)
        };
        let fresh = Self::with_limits(nodes, bytes);
        std::mem::swap(&mut *self.0.borrow_mut(), &mut *fresh.0.borrow_mut());
    }

    pub(crate) fn execution_seed(&self) -> ObservationSeed {
        if self.0.borrow().phase != ObservationPhase::Precheck {
            self.begin_precheck();
        }
        let mut seed = self.seed();
        seed.report.phase = ObservationPhase::Executing;
        seed.report.closure = ClosureStatus::Complete;
        self.executing();
        seed
    }

    pub(crate) fn publish(&self, seed: ObservationSeed) {
        let state = Self::from_seed(seed);
        std::mem::swap(&mut *self.0.borrow_mut(), &mut *state.0.borrow_mut());
    }

    fn charge(&self, bytes: usize) -> bool {
        let mut r = self.0.borrow_mut();
        if r.limited {
            return false;
        }
        if r.nodes >= r.max_nodes || bytes > r.max_bytes.saturating_sub(r.bytes) {
            r.limited = true;
            return false;
        }
        r.nodes += 1;
        r.bytes += bytes;
        true
    }

    pub(crate) fn table(&self, table: TableId) {
        if self.0.borrow().dependencies.contains(&table) {
            return;
        }
        if self.charge(64) {
            self.0.borrow_mut().dependencies.insert(table);
        }
    }

    fn unresolved(&self, path: &[u32], reason: UnresolvedReason) {
        if self.charge(path.len().saturating_mul(4).saturating_add(64)) {
            self.0.borrow_mut().unresolved.insert(path.to_vec(), reason);
        }
    }

    pub(crate) fn executing(&self) {
        self.0.borrow_mut().phase = ObservationPhase::Executing;
    }
    pub(crate) fn finish(&self, success: bool) {
        self.0.borrow_mut().phase = if success {
            ObservationPhase::Finished
        } else {
            ObservationPhase::Failed
        };
        if !success && !self.0.borrow().terminal {
            self.unresolved(&[], UnresolvedReason::Interrupted);
        }
    }
    pub(crate) fn terminal(&self) {
        let mut r = self.0.borrow_mut();
        r.phase = ObservationPhase::Failed;
        r.terminal = true;
    }

    fn deferred(&self, rel: &Arc<RelL>, path: &[u32]) {
        let id = Arc::as_ptr(rel) as usize;
        let existing = self.0.borrow().published.get(&id).cloned();
        if let Some(target) = existing {
            if self.charge(
                path.len()
                    .saturating_add(target.len())
                    .saturating_mul(4)
                    .saturating_add(64),
            ) {
                self.0.borrow_mut().aliases.insert(path.to_vec(), target);
            }
            return;
        }
        self.unresolved(path, UnresolvedReason::DeferredSubplan);
        if self.charge(path.len().saturating_mul(4).saturating_add(48)) {
            self.0
                .borrow_mut()
                .deferred
                .entry(Arc::as_ptr(rel) as usize)
                .or_default()
                .push(path.to_vec());
        }
    }

    pub(crate) fn publish_deferred(&self, id: usize, plan: &mut Plan) {
        let paths = self.0.borrow_mut().deferred.remove(&id).unwrap_or_default();
        if let Some(first) = paths.first() {
            let target = child(first, 0);
            if self.charge(target.len().saturating_mul(4).saturating_add(64)) {
                self.0.borrow_mut().published.insert(id, target.clone());
                self.plan(plan, &target);
                for alias in paths.iter().skip(1) {
                    if self.charge(
                        alias
                            .len()
                            .saturating_add(target.len())
                            .saturating_mul(4)
                            .saturating_add(64),
                    ) {
                        self.0
                            .borrow_mut()
                            .aliases
                            .insert(alias.clone(), target.clone());
                    }
                }
            }
        }
        let mut r = self.0.borrow_mut();
        for path in paths {
            r.unresolved.remove(&path);
        }
    }

    pub(crate) fn nested_slot(&self, index: usize) {
        self.unresolved(&[1, index as u32], UnresolvedReason::DeferredSubplan);
    }
    pub(crate) fn publish_nested(&self, index: usize, plan: &mut Plan) {
        let path = [1, index as u32];
        self.plan(plan, &child(&path, 0));
        self.0.borrow_mut().unresolved.remove(path.as_slice());
    }

    fn expressions<'a>(&self, exprs: impl IntoIterator<Item = &'a PExpr>, path: &[u32]) {
        for (index, expr) in exprs.into_iter().enumerate() {
            self.expr(expr, &child(path, index as u32));
        }
    }

    fn expr(&self, expr: &PExpr, path: &[u32]) {
        if !self.charge(path.len().saturating_mul(4).saturating_add(16)) {
            return;
        }
        match expr {
            PExpr::Col(_)
            | PExpr::Outer { .. }
            | PExpr::Lit(_)
            | PExpr::Set(_)
            | PExpr::Unit(_) => {}
            PExpr::Neg(x)
            | PExpr::Not(x)
            | PExpr::IsNull { l: x, .. }
            | PExpr::Refs { l: x, .. }
            | PExpr::Cast { x, .. }
            | PExpr::TypeOf { x, .. }
            | PExpr::StoredSize { x, .. } => self.expr(x, &child(path, 0)),
            PExpr::Arith { first, steps, .. } => self.expressions(
                std::iter::once(first.as_ref()).chain(steps.iter().map(|(_, e, _)| e)),
                path,
            ),
            PExpr::And(es) | PExpr::Or(es) | PExpr::Func { args: es, .. } => {
                self.expressions(es, path)
            }
            PExpr::Compare(_, l, r) | PExpr::Like { l, pat: r, .. } => {
                self.expressions([l.as_ref(), r.as_ref()], path)
            }
            PExpr::InList { l, rhs, .. } => self.expressions(l.iter().chain(rhs), path),
            PExpr::InQuery { l, q, .. } => {
                self.deferred(q, &child(path, 0));
                self.expressions(l, &child(path, 1));
            }
            PExpr::SectionAgg { owner, rel, .. } => {
                self.deferred(rel, &child(path, 0));
                self.expr(owner, &child(path, 1));
            }
            PExpr::Between { l, lo, hi, .. } => {
                self.expressions([l.as_ref(), lo.as_ref(), hi.as_ref()], path)
            }
            PExpr::Case {
                operand,
                whens,
                else_,
            } => self.expressions(
                operand
                    .iter()
                    .map(Box::as_ref)
                    .chain(whens.iter().flat_map(|(a, b)| [a, b]))
                    .chain(else_.iter().map(Box::as_ref)),
                path,
            ),
        }
    }

    pub(crate) fn plan(&self, plan: &mut Plan, path: &[u32]) {
        if !self.charge(path.len().saturating_mul(4).saturating_add(32)) {
            return;
        }
        match plan {
            Plan::Empty | Plan::Values { .. } | Plan::TempScan { .. } => {}
            Plan::Scan { table, .. } => self.table(*table),
            Plan::IdLookup { table, ids, .. } => {
                self.table(*table);
                self.expressions(ids.iter(), path);
            }
            Plan::IndexRange { table, lo, hi, .. } => {
                self.table(*table);
                self.expressions(bound_exprs(lo).chain(bound_exprs(hi)), path);
            }
            Plan::TempIndexRange { lo, hi, .. } => {
                self.expressions(bound_exprs(lo).chain(bound_exprs(hi)), path)
            }
            Plan::IndexPoints {
                table,
                prefix,
                values,
                ..
            } => {
                self.table(*table);
                self.expressions(prefix.iter().chain(values.iter()), path);
            }
            Plan::Slice {
                step, observation, ..
            } => {
                let mut outer = false;
                for e in step.bound.iter() {
                    crate::plan::walk(e, &mut |e| {
                        if matches!(
                            e,
                            PExpr::Outer { .. }
                                | PExpr::InQuery {
                                    correlated: true,
                                    ..
                                }
                        ) {
                            outer = true;
                        }
                    });
                }
                self.unresolved(path, UnresolvedReason::UnselectedRegister);
                *observation = Some(RegisterObservation {
                    path: path.to_vec(),
                    outer_dependent: outer,
                });
                self.expressions(step.bound.iter(), &child(path, 0));
            }
            Plan::RegisterRows { step, observation } => {
                let mut outer = false;
                for e in step.bounds.iter().flatten() {
                    crate::plan::walk(e, &mut |e| {
                        if matches!(
                            e,
                            PExpr::Outer { .. }
                                | PExpr::InQuery {
                                    correlated: true,
                                    ..
                                }
                        ) {
                            outer = true;
                        }
                    });
                }
                self.unresolved(path, UnresolvedReason::UnselectedRegister);
                *observation = Some(RegisterObservation {
                    path: path.to_vec(),
                    outer_dependent: outer,
                });
                self.expressions(step.bounds.iter().flatten(), &child(path, 0));
            }
            Plan::RunningBalance { input, step } => {
                self.plan(input, &child(path, 0));
                self.expressions(
                    step.bounds
                        .iter()
                        .flatten()
                        .chain(std::iter::once(&step.period))
                        .chain(step.period_fields.iter()),
                    &child(path, 1),
                );
            }
            Plan::Filter { input, pred } => {
                self.plan(input, &child(path, 0));
                self.expr(pred, &child(path, 1));
            }
            Plan::Project { input, exprs } => {
                self.plan(input, &child(path, 0));
                self.expressions(exprs.iter(), &child(path, 1));
            }
            Plan::Aggregate {
                input, keys, aggs, ..
            } => {
                self.plan(input, &child(path, 0));
                self.expressions(
                    keys.iter()
                        .chain(aggs.iter().filter_map(|a| a.arg.as_ref())),
                    &child(path, 1),
                );
            }
            Plan::CheckAccess { input, .. } | Plan::Distinct(input) | Plan::Limit { input, .. } => {
                self.plan(input, &child(path, 0))
            }
            Plan::Sort { input, keys }
            | Plan::TopN { input, keys, .. }
            | Plan::Hierarchy { input, keys, .. } => {
                self.plan(input, &child(path, 0));
                self.expressions(keys.iter().map(|(e, _)| e), &child(path, 1));
            }
            Plan::Union { parts, .. } => {
                for (index, part) in parts.iter_mut().enumerate() {
                    self.plan(part, &child(path, index as u32));
                }
            }
            Plan::SemiJoin { input, l, q, .. } => {
                self.plan(input, &child(path, 0));
                self.plan(q, &child(path, 1));
                self.expressions(l.iter(), &child(path, 2));
            }
            Plan::HierarchyClosure { tables, seeds, .. } => {
                for table in tables {
                    self.table(table.table);
                }
                match seeds {
                    ClosureSeeds::Values(es) => self.expressions(es.iter(), &child(path, 0)),
                    ClosureSeeds::Plan(p) => self.plan(p, &child(path, 0)),
                }
            }
            Plan::NestedLoopJoin { l, r, on, .. } => {
                self.plan(l, &child(path, 0));
                self.plan(r, &child(path, 1));
                self.expr(on, &child(path, 2));
            }
            Plan::HashJoin {
                l, r, lk, rk, on, ..
            } => {
                self.plan(l, &child(path, 0));
                self.plan(r, &child(path, 1));
                self.expressions(
                    lk.iter().chain(rk.iter()).chain(std::iter::once(&*on)),
                    &child(path, 2),
                );
            }
            Plan::IndexJoin {
                l,
                table,
                lookup,
                on,
                ..
            } => {
                self.table(*table);
                self.plan(l, &child(path, 0));
                match lookup {
                    Lookup::Id(e) => self.expr(e, &child(path, 1)),
                    Lookup::Prefix { keys, .. } => self.expressions(keys.iter(), &child(path, 1)),
                }
                self.expr(on, &child(path, 2));
            }
            Plan::ValuesIndexJoin {
                values,
                table,
                keys,
                on,
                ..
            } => {
                self.table(*table);
                self.plan(values, &child(path, 0));
                self.expressions(keys.iter().chain(std::iter::once(&*on)), &child(path, 1));
            }
            Plan::TempIndexJoin { l, keys, on, .. } => {
                self.plan(l, &child(path, 0));
                self.expressions(keys.iter().chain(std::iter::once(&*on)), &child(path, 1));
            }
        }
    }
}

fn child(path: &[u32], index: u32) -> Vec<u32> {
    let mut next = path.to_vec();
    next.push(index);
    next
}
fn bound_exprs(bound: &std::ops::Bound<Vec<PExpr>>) -> impl Iterator<Item = &PExpr> {
    match bound {
        std::ops::Bound::Included(es) | std::ops::Bound::Excluded(es) => es.as_slice(),
        std::ops::Bound::Unbounded => &[],
    }
    .iter()
}

#[doc(hidden)]
#[derive(Clone)]
pub struct RegisterObservation {
    path: Vec<u32>,
    outer_dependent: bool,
}
impl std::fmt::Debug for RegisterObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterObservation")
            .field("path", &self.path)
            .field("outer_dependent", &self.outer_dependent)
            .finish()
    }
}
impl RegisterObservation {
    pub(crate) fn publish_slice(&self, session: &ObservationSession, plan: &mut Plan) {
        session.plan(plan, &child(&self.path, 1));
        if !self.outer_dependent {
            session.0.borrow_mut().unresolved.remove(&self.path);
        }
    }

    pub(crate) fn publish(
        &self,
        session: &ObservationSession,
        step: &query_ir::lower::RegisterRowsL,
        selected: Rc<SelectedRegister>,
    ) {
        if !session.charge(
            selected
                .metadata_bytes()
                .saturating_add(self.path.len().saturating_mul(4))
                .saturating_add(64),
        ) {
            return;
        }
        if !selected.settings.use_totals {
            return;
        }
        for table in selected.tables(step) {
            session.table(table);
        }
        let mut r = session.0.borrow_mut();
        if !self.outer_dependent {
            r.unresolved.remove(&self.path);
        }
        r.selected.insert(self.path.clone(), selected);
    }
}
