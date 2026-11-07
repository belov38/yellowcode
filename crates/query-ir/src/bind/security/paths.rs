use super::{Collector, Path, Root};
use crate::ir::{Expr, ExprKind as E, Rel, SrcId};
use crate::schema::{QFieldId, QTableId};
use crate::tir::Deref;
use std::collections::{HashMap, HashSet, hash_map::RandomState};
use std::hash::{BuildHasher, Hash, Hasher};

fn root(c: &Collector<'_, '_>, expr: &Expr) -> Option<Root> {
    match &expr.kind {
        E::Col { src, col } => Some(Root::Column(c.semantic_source(*src), *col)),
        E::Param(i) => Some(Root::Param(*i)),
        E::Value(i) => Some(Root::Value(*i)),
        E::Lit(crate::QValue::Ref { table, id }) => Some(Root::Reference(*table, *id)),
        E::Lit(_)
        | E::KeyRef(_)
        | E::AggRef(_)
        | E::Out(_)
        | E::Unit(_)
        | E::Deref { .. }
        | E::Cast { .. }
        | E::LocalKey { .. }
        | E::OuterKey { .. }
        | E::Neg(_)
        | E::Not(_)
        | E::Arith { .. }
        | E::And(_)
        | E::Or(_)
        | E::Compare(..)
        | E::InList { .. }
        | E::InQuery { .. }
        | E::SectionAgg { .. }
        | E::Between { .. }
        | E::IsNull { .. }
        | E::Refs { .. }
        | E::Like { .. }
        | E::Case { .. }
        | E::Func { .. } => None,
    }
}

pub(super) fn single(c: &mut Collector<'_, '_>, mut expr: &Expr) -> Path {
    let mut segments = Vec::new();
    loop {
        #[cfg(test)]
        observe_spine();
        match &expr.kind {
            E::Cast { x, .. } => expr = x,
            E::LocalKey { orig, .. } | E::OuterKey { orig, .. } => expr = orig,
            E::Deref { base, steps } => {
                if !steps.is_empty() {
                    segments.push(steps.as_slice());
                }
                expr = base;
            }
            _ => break,
        }
    }
    let mut path = root(c, expr).map_or_else(
        || c.opaque(),
        |root| Path {
            root,
            steps: Vec::new(),
        },
    );
    for segment in segments.into_iter().rev() {
        path.steps
            .extend(segment.iter().map(|step| step.targets.clone()));
    }
    path
}

#[derive(Clone, Default)]
struct Summary {
    root: Option<Root>,
    tail: Option<usize>,
}

struct Arena<T> {
    entries: Vec<T>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    fn push(&mut self, value: T) -> usize {
        let i = self.entries.len();
        self.entries.push(value);
        i
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "index is returned by push on this private arena; entries are never removed"
    )]
    fn at(&self, i: usize) -> &T {
        debug_assert!(i < self.entries.len());
        &self.entries[i]
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "index is returned by push on this private arena; entries are never removed"
    )]
    fn at_mut(&mut self, i: usize) -> &mut T {
        debug_assert!(i < self.entries.len());
        &mut self.entries[i]
    }
}

struct Link<'a> {
    previous: Option<usize>,
    steps: &'a [Deref],
}

enum Frame<'a> {
    Expr(&'a Expr, usize),
    Rel(&'a Rel),
    Finish {
        slot: usize,
        child: usize,
        steps: &'a [Deref],
    },
}

#[derive(Default)]
struct Walk<'a> {
    todo: Vec<Frame<'a>>,
    summaries: Arena<Summary>,
    links: Arena<Link<'a>>,
    outputs: Vec<usize>,
}

impl<'a> Walk<'a> {
    fn push_expr(&mut self, expr: &'a Expr) -> usize {
        let slot = self.summaries.push(Summary::default());
        self.todo.push(Frame::Expr(expr, slot));
        slot
    }

    fn spine(&mut self, slot: usize, child: &'a Expr, steps: &'a [Deref]) {
        let child_slot = self.summaries.push(Summary::default());
        self.todo.push(Frame::Finish {
            slot,
            child: child_slot,
            steps,
        });
        self.todo.push(Frame::Expr(child, child_slot));
    }

    fn expr(&mut self, c: &Collector<'_, '_>, expr: &'a Expr, slot: usize) {
        #[cfg(test)]
        observe_expr();
        self.summaries.at_mut(slot).root = root(c, expr);
        match &expr.kind {
            E::Deref { base, steps } => {
                self.outputs.push(slot);
                self.spine(slot, base, steps);
            }
            E::Cast { x, .. } | E::LocalKey { orig: x, .. } | E::OuterKey { orig: x, .. } => {
                self.spine(slot, x, &[]);
            }
            E::Neg(x) | E::Not(x) | E::IsNull { l: x, .. } | E::Refs { l: x, .. } => {
                self.push_expr(x);
            }
            E::Arith { first, steps } => {
                for (_, expr, _) in steps.iter().rev() {
                    self.push_expr(expr);
                }
                self.push_expr(first);
            }
            E::And(values) | E::Or(values) | E::Func { args: values, .. } => {
                for expr in values.iter().rev() {
                    self.push_expr(expr);
                }
            }
            E::Compare(_, left, right) => {
                self.push_expr(right);
                self.push_expr(left);
            }
            E::InList { l, rhs, .. } => {
                for expr in rhs.iter().rev().chain(l.iter().rev()) {
                    self.push_expr(expr);
                }
            }
            E::InQuery { l, q, .. } => {
                self.todo.push(Frame::Rel(q));
                for expr in l.iter().rev() {
                    self.push_expr(expr);
                }
            }
            E::SectionAgg { owner, rel, .. } => {
                self.todo.push(Frame::Rel(rel));
                self.push_expr(owner);
            }
            E::Between { l, lo, hi, .. } => {
                self.push_expr(hi);
                self.push_expr(lo);
                self.push_expr(l);
            }
            E::Like { l, pat, .. } => {
                self.push_expr(pat);
                self.push_expr(l);
            }
            E::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(expr) = else_ {
                    self.push_expr(expr);
                }
                for (when, then) in whens.iter().rev() {
                    self.push_expr(then);
                    self.push_expr(when);
                }
                if let Some(expr) = operand {
                    self.push_expr(expr);
                }
            }
            E::Col { .. }
            | E::Lit(_)
            | E::Param(_)
            | E::Value(_)
            | E::KeyRef(_)
            | E::AggRef(_)
            | E::Out(_)
            | E::Unit(_) => {}
        }
    }

    fn rel(&mut self, rel: &'a Rel) {
        #[cfg(test)]
        observe_ir_rel(None);
        match rel {
            Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => {}
            Rel::Subquery { input, .. } | Rel::Distinct(input) | Rel::Top { input, .. } => {
                self.todo.push(Frame::Rel(input));
            }
            Rel::Join { l, r, on, .. } => {
                self.push_expr(on);
                self.todo.push(Frame::Rel(r));
                self.todo.push(Frame::Rel(l));
            }
            Rel::Filter { input, pred } => {
                self.push_expr(pred);
                self.todo.push(Frame::Rel(input));
            }
            Rel::Aggregate {
                input, keys, aggs, ..
            } => {
                #[cfg(test)]
                observe_ir_rel(Some(aggs));
                for call in aggs.iter().rev() {
                    if let Some(expr) = &call.arg {
                        self.push_expr(expr);
                    }
                }
                for expr in keys.iter().rev() {
                    self.push_expr(expr);
                }
                self.todo.push(Frame::Rel(input));
            }
            Rel::Project { input, exprs } => {
                for expr in exprs.iter().rev() {
                    self.push_expr(expr);
                }
                self.todo.push(Frame::Rel(input));
            }
            Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
                for key in keys.iter().rev() {
                    self.push_expr(&key.expr);
                }
                self.todo.push(Frame::Rel(input));
            }
            Rel::Union { parts, .. } => {
                for part in parts.iter().rev() {
                    self.todo.push(Frame::Rel(part));
                }
            }
            Rel::RegisterVt(view) | Rel::Slice(view) | Rel::RunningBalance(view) => {
                if let Some(key) = &view.order {
                    self.push_expr(&key.expr);
                }
                if let Some(expr) = &view.access {
                    self.push_expr(expr);
                }
                if let Some(area) = &view.area {
                    self.push_expr(&area.pred);
                }
                if let Some(expr) = &view.cond {
                    self.push_expr(expr);
                }
                for expr in view.bounds.iter().rev().flatten() {
                    self.push_expr(expr);
                }
            }
        }
    }
}

pub(super) fn collect(c: &mut Collector<'_, '_>, expr: &Expr, out: &mut Vec<Path>) {
    let mut walk = Walk::default();
    walk.push_expr(expr);
    while let Some(frame) = walk.todo.pop() {
        match frame {
            Frame::Expr(expr, slot) => walk.expr(c, expr, slot),
            Frame::Rel(rel) => walk.rel(rel),
            Frame::Finish { slot, child, steps } => {
                let mut summary = walk.summaries.at(child).clone();
                if !steps.is_empty() {
                    let tail = walk.links.push(Link {
                        previous: summary.tail,
                        steps,
                    });
                    summary.tail = Some(tail);
                }
                *walk.summaries.at_mut(slot) = summary;
            }
        }
    }
    let mut segments = Vec::new();
    for slot in walk.outputs {
        let summary = walk.summaries.at(slot);
        let mut path = summary.root.clone().map_or_else(
            || c.opaque(),
            |root| Path {
                root,
                steps: Vec::new(),
            },
        );
        let mut tail = summary.tail;
        while let Some(index) = tail {
            segments.push(index);
            tail = walk.links.at(index).previous;
        }
        for index in segments.drain(..).rev() {
            path.steps.extend(
                walk.links
                    .at(index)
                    .steps
                    .iter()
                    .map(|step| step.targets.clone()),
            );
        }
        out.push(path);
    }
}

type Step = [(QTableId, QFieldId)];

struct Edge<'a> {
    step: &'a Step,
    child: usize,
}

#[derive(Default)]
struct Node<'a> {
    tables: HashSet<QTableId>,
    edges: HashMap<u64, Vec<Edge<'a>>>,
}

pub(super) struct RestrictedPaths<'a> {
    roots: HashMap<&'a Root, usize>,
    nodes: Arena<Node<'a>>,
    fingerprints: RandomState,
}

impl<'a> RestrictedPaths<'a> {
    pub(super) fn new(
        targets: &'a HashMap<(Path, QTableId), SrcId>,
        conditions: &HashMap<SrcId, Expr>,
    ) -> Self {
        let mut index = Self {
            roots: HashMap::new(),
            nodes: Arena::default(),
            fingerprints: RandomState::new(),
        };
        for ((path, table), source) in targets {
            if !conditions.contains_key(source) {
                continue;
            }
            let mut node = *index
                .roots
                .entry(&path.root)
                .or_insert_with(|| index.nodes.push(Node::default()));
            for step in &path.steps {
                #[cfg(test)]
                observe(|work| {
                    for _ in step {
                        work.indexed_pairs += 1;
                    }
                });
                let fingerprint = index.fingerprint(step);
                let child = index
                    .nodes
                    .at(node)
                    .edges
                    .get(&fingerprint)
                    .and_then(|bucket| {
                        bucket
                            .iter()
                            .find(|edge| same_step(edge.step, step))
                            .map(|edge| edge.child)
                    });
                if let Some(child) = child {
                    node = child;
                } else {
                    let child = index.nodes.push(Node::default());
                    index
                        .nodes
                        .at_mut(node)
                        .edges
                        .entry(fingerprint)
                        .or_default()
                        .push(Edge { step, child });
                    node = child;
                }
            }
            index.nodes.at_mut(node).tables.insert(*table);
        }
        index
    }

    fn fingerprint(&self, step: &Step) -> u64 {
        let mut hasher = self.fingerprints.build_hasher();
        step.len().hash(&mut hasher);
        for pair in step {
            #[cfg(test)]
            observe(|work| work.fingerprint_pairs += 1);
            pair.hash(&mut hasher);
        }
        let value = hasher.finish();
        value
    }

    pub(super) fn hits(&self, path: &Path) -> bool {
        let Some(mut node) = self.roots.get(&path.root).copied() else {
            return false;
        };
        for step in &path.steps {
            for (table, _) in step {
                #[cfg(test)]
                observe(|work| work.checked_pairs += 1);
                if self.nodes.at(node).tables.contains(table) {
                    return true;
                }
            }
            let fingerprint = self.fingerprint(step);
            let Some(edge) = self
                .nodes
                .at(node)
                .edges
                .get(&fingerprint)
                .and_then(|bucket| bucket.iter().find(|edge| same_step(edge.step, step)))
            else {
                return false;
            };
            node = edge.child;
        }
        false
    }
}

fn same_step(left: &Step, right: &Step) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            #[cfg(test)]
            observe(|work| work.equality_pairs += 1);
            left == right
        })
}

#[cfg(test)]
thread_local! {
    static WORK: RefCell<Option<Work>> = const { RefCell::new(None) };
    static FORCE_COLLISION: Cell<bool> = const { Cell::new(false) };
}
