pub mod access;
mod accounting;
mod accounting_turnovers;
pub(crate) use accounting::{check_cancel as check_accounting_cancel, check_drcr_settings_shape};
mod accounting_balance;
pub(crate) use accounting_balance::check_balance_settings_shape;
mod security;
pub(crate) use security::CheckAccess;
pub mod agg;
pub mod expr;
pub mod hier;
pub mod join;
pub mod nested;
pub mod present;
pub mod register;
pub mod running;
pub mod setops;
pub mod slice;
pub mod sort;
pub mod subq;
pub mod temp;

use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;

use memory_budget::{Category, Evictor, LimitReason, MemoryLimit, Reservation};
use query_ir::{ExecError, QValue};

use crate::model::{block, row_cap_model, row_model, value_model};
use crate::source::{DataSource, ExecEnv, Row};

pub use access::{GetById, IndexRange, Scan};
pub use agg::HashAggregate;
pub use expr::eval;
pub use hier::{HierarchyClosure, HierarchyOrder};
pub use join::{HashJoin, IndexJoin, IndexLookup, NestedLoopJoin, SemiJoin};
pub use setops::{Concat, Distinct, union};
pub use sort::{Sort, TopN};
pub use temp::{TempRange, TempScan, ValuesScan};

pub trait Op<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError>;

    fn close(&mut self, _cx: &mut Cx<'a>) {}
}

pub type BoxOp<'a> = Box<dyn Op<'a> + 'a>;

const SLACK_UNIT: u64 = 64 << 10;
const SLACK_KEEP: u64 = 128 << 10;

#[derive(Clone, Copy, Default)]
struct Held {
    persistent: u64,
    owned: u64,
}

impl Held {
    fn total(self) -> u64 {
        self.persistent.saturating_add(self.owned)
    }
}

#[derive(Debug)]
pub(crate) struct OwnedCharge {
    reservation: Option<Reservation>,
    held: Weak<Cell<Held>>,
    bytes: u64,
}

impl Drop for OwnedCharge {
    fn drop(&mut self) {
        if let Some(shared) = self.held.upgrade() {
            let mut held = shared.get();
            held.owned = held.owned.saturating_sub(self.bytes);
            shared.set(held);
        }
        self.held = Weak::new();
        self.reservation = None;
    }
}

pub struct MemBudget<'a> {
    used: Cell<u64>,
    peak: Cell<u64>,
    limit: u64,
    held: Rc<Cell<Held>>,
    lease: Option<&'a Reservation>,
    evictor: Option<&'a dyn Evictor>,
    slack: Cell<u64>,
    scratch: Cell<u64>,
    row_limit: u64,
}

impl std::fmt::Debug for MemBudget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemBudget")
            .field("used", &self.used.get())
            .field("peak", &self.peak.get())
            .field("limit", &self.limit)
            .field("held", &self.held.get().total())
            .field("leased", &self.lease.is_some())
            .field("slack", &self.slack.get())
            .field("scratch", &self.scratch.get())
            .finish()
    }
}

fn legacy_limit() -> ExecError {
    ExecError::Fatal("превышен лимит памяти запроса".to_string())
}

impl<'a> MemBudget<'a> {
    pub fn new(limit: u64) -> MemBudget<'a> {
        MemBudget {
            used: Cell::new(0),
            peak: Cell::new(0),
            limit,
            held: Rc::new(Cell::new(Held::default())),
            lease: None,
            evictor: None,
            slack: Cell::new(0),
            scratch: Cell::new(0),
            row_limit: u64::MAX,
        }
    }

    pub fn of_env(env: &ExecEnv<'a>) -> MemBudget<'a> {
        MemBudget {
            used: Cell::new(0),
            peak: Cell::new(0),
            limit: if env.lease.is_some() {
                u64::MAX
            } else {
                env.mem_limit
            },
            held: Rc::new(Cell::new(Held::default())),
            lease: env.lease,
            evictor: env.evictor,
            slack: Cell::new(0),
            scratch: Cell::new(0),
            row_limit: env.memory.map_or(u64::MAX, |m| m.row_limit),
        }
    }

    pub fn is_leased(&self) -> bool {
        self.lease.is_some()
    }

    pub fn row_limit(&self) -> u64 {
        self.row_limit
    }

    pub fn check_value(&self, bytes: u64) -> Result<(), ExecError> {
        if bytes > self.row_limit {
            return Err(ExecError::Memory(MemoryLimit {
                category: Category::Row,
                requested: bytes,
                reason: LimitReason::Category,
            }));
        }
        Ok(())
    }

    fn take(&self, n: u64) -> Result<(), ExecError> {
        let Some(lease) = self.lease else {
            let total = self
                .used
                .get()
                .saturating_add(n)
                .saturating_add(self.held.get().total());
            if total > self.limit {
                self.used.set(self.used.get().saturating_add(n));
                self.peak.set(self.peak.get().max(total));
                return Err(legacy_limit());
            }
            return Ok(());
        };
        let slack = self.slack.get();
        if n <= slack {
            self.slack.set(slack - n);
            return Ok(());
        }
        let need = n - slack;
        let unit = need.div_ceil(SLACK_UNIT).saturating_mul(SLACK_UNIT);
        match self.evictor {
            Some(e) => lease.try_grow_or_evict(unit, e),
            None => lease.try_grow(unit),
        }
        .map_err(ExecError::Memory)?;
        self.slack.set(unit - need);
        Ok(())
    }

    fn give(&self, n: u64) {
        let Some(lease) = self.lease else {
            return;
        };
        let slack = self.slack.get().saturating_add(n);
        if slack > SLACK_KEEP {
            let _ = lease.shrink(slack - SLACK_UNIT);
            self.slack.set(SLACK_UNIT);
        } else {
            self.slack.set(slack);
        }
    }

    pub fn grow(&self, n: u64) -> Result<(), ExecError> {
        self.take(n)?;
        let used = self.used.get().saturating_add(n);
        self.used.set(used);
        let total = used.saturating_add(self.held.get().total());
        self.peak.set(self.peak.get().max(total));
        Ok(())
    }

    pub fn shrink(&self, n: u64) {
        let n = n.min(self.used.get());
        self.used.set(self.used.get() - n);
        self.give(n);
    }

    pub fn grow_scratch(&self, n: u64) -> Result<(), ExecError> {
        self.check_value(self.scratch.get().saturating_add(n))?;
        self.grow(n)?;
        self.scratch.set(self.scratch.get().saturating_add(n));
        Ok(())
    }

    pub fn shrink_scratch(&self, n: u64) {
        let n = n.min(self.scratch.get());
        self.scratch.set(self.scratch.get() - n);
        self.shrink(n);
    }

    pub fn keep_scratch(&self, n: u64) -> Result<(), ExecError> {
        let scratch = self.scratch.get();
        if n <= scratch {
            self.scratch.set(scratch - n);
            return Ok(());
        }
        self.scratch.set(0);
        self.grow(n - scratch)
    }

    pub fn release_scratch(&self) {
        let s = self.scratch.replace(0);
        self.shrink(s);
    }

    pub fn scratch(&self) -> u64 {
        self.scratch.get()
    }

    pub fn mark(&self) -> Mark {
        Mark {
            used: self.used.get(),
            scratch: self.scratch.get(),
        }
    }

    pub fn reset_to(&self, mark: Mark) {
        let base = mark.used.saturating_sub(mark.scratch);
        let target = base.saturating_add(self.scratch.get());
        let used = self.used.get();
        if target < used {
            self.shrink(used - target);
        }
    }

    pub fn used(&self) -> u64 {
        self.used.get()
    }

    pub fn peak(&self) -> u64 {
        self.peak.get()
    }

    pub fn reconcile(&self, keep: u64) {
        let used = self.used.get();
        if used > keep {
            self.shrink(used - keep);
        }
        self.used.set(0);
    }

    pub(crate) fn hold(&self, n: u64) -> Result<(), ExecError> {
        self.take(n)?;
        let mut held = self.held.get();
        held.persistent = held.persistent.saturating_add(n);
        self.held.set(held);
        let total = self.used.get().saturating_add(held.total());
        self.peak.set(self.peak.get().max(total));
        Ok(())
    }

    pub(crate) fn hold_owned(&self, n: u64) -> Result<OwnedCharge, ExecError> {
        let reservation = if let Some(lease) = self.lease {
            Some(
                match self.evictor {
                    Some(evictor) => {
                        lease
                            .budget()
                            .try_reserve_or_evict(lease.category(), n, evictor)
                    }
                    None => lease.budget().try_reserve(lease.category(), n),
                }
                .map_err(ExecError::Memory)?,
            )
        } else {
            self.take(n)?;
            None
        };
        let mut held = self.held.get();
        held.owned = held.owned.saturating_add(n);
        self.held.set(held);
        self.peak.set(
            self.peak
                .get()
                .max(self.used.get().saturating_add(held.total())),
        );
        Ok(OwnedCharge {
            reservation,
            held: Rc::downgrade(&self.held),
            bytes: n,
        })
    }

    pub(crate) fn grow_owned(&self, owner: &mut OwnedCharge, n: u64) -> Result<(), ExecError> {
        if owner
            .held
            .upgrade()
            .is_none_or(|held| !Rc::ptr_eq(&held, &self.held))
        {
            return Err(fatal("input charge belongs to another execution"));
        }
        let next = crate::model::size_add(owner.bytes, n)?;
        let mut held = self.held.get();
        let owned = crate::model::size_add(held.owned, n)?;
        if let Some(reservation) = &owner.reservation {
            match self.evictor {
                Some(evictor) => reservation.try_grow_or_evict(n, evictor),
                None => reservation.try_grow(n),
            }
            .map_err(ExecError::Memory)?;
        } else {
            let total = crate::model::size_add(
                self.used.get(),
                crate::model::size_add(held.persistent, owned)?,
            )?;
            if total > self.limit {
                return Err(legacy_limit());
            }
        }
        owner.bytes = next;
        held.owned = owned;
        self.held.set(held);
        self.peak.set(
            self.peak
                .get()
                .max(self.used.get().saturating_add(held.total())),
        );
        Ok(())
    }

    pub(crate) fn release_held(&self) {
        let mut held = self.held.get();
        let h = held.persistent;
        held.persistent = 0;
        self.held.set(held);
        self.give(h);
    }

    fn child(&self) -> MemBudget<'a> {
        MemBudget {
            used: Cell::new(0),
            peak: Cell::new(0),
            limit: self.limit.saturating_sub(self.used.get()),
            held: self.held.clone(),
            lease: self.lease,
            evictor: self.evictor,
            slack: Cell::new(0),
            scratch: Cell::new(0),
            row_limit: self.row_limit,
        }
    }

    fn absorb_peak(&self, sub_peak: u64) {
        let at = self.used.get().saturating_add(sub_peak);
        self.peak.set(self.peak.get().max(at));
    }
}

impl Drop for MemBudget<'_> {
    fn drop(&mut self) {
        let held = if Rc::strong_count(&self.held) == 1 {
            let mut held = self.held.get();
            let persistent = held.persistent;
            held.persistent = 0;
            self.held.set(held);
            persistent
        } else {
            0
        };
        if let Some(lease) = self.lease {
            let n = self
                .used
                .get()
                .saturating_add(self.slack.get())
                .saturating_add(held);
            if n > 0 {
                let _ = lease.shrink(n);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    used: u64,
    scratch: u64,
}

pub struct Cx<'a> {
    pub(crate) observation: Option<crate::observe::ObservationSession>,
    pub(crate) input_payloads:
        Option<Rc<std::cell::RefCell<crate::accounting_inputs::PaidPayloads>>>,
    pub(crate) input_structure: Option<OwnedCharge>,
    pub env: &'a ExecEnv<'a>,
    pub coll: crate::coll::ExecCollation<'a>,
    pub src: &'a dyn DataSource,
    pub outer: Vec<Row>,
    pub mem: MemBudget<'a>,
    pub temps: Option<&'a crate::temp::TempTables>,
    pub(crate) subs: Rc<subq::SubCache>,
    ticks: u64,
    pub week_start: Option<u32>,
}

impl<'a> Cx<'a> {
    pub fn new(env: &'a ExecEnv<'a>, src: &'a dyn DataSource) -> Cx<'a> {
        Cx {
            env,
            coll: crate::coll::ExecCollation::Legacy(env.collation),
            src,
            outer: Vec::new(),
            mem: MemBudget::of_env(env),
            temps: None,
            subs: Rc::new(subq::SubCache::default()),
            ticks: 0,
            week_start: None,
            observation: None,
            input_payloads: None,
            input_structure: None,
        }
    }

    pub(crate) fn release_inputs(&mut self) {
        if Rc::strong_count(&self.subs) == 1 {
            self.subs.clear();
        }
        self.input_structure = None;
        self.input_payloads = None;
    }

    pub fn plan_options(&mut self, opts: crate::plan::PlanOptions) {
        self.subs = Rc::new(subq::SubCache::new(opts));
    }

    pub(crate) fn child(&self, outer: Vec<Row>) -> Cx<'a> {
        Cx {
            env: self.env,
            coll: self.coll,
            src: self.src,
            outer,
            mem: self.mem.child(),
            temps: self.temps,
            subs: self.subs.clone(),
            ticks: 0,
            week_start: self.week_start,
            observation: self.observation.clone(),
            input_payloads: self.input_payloads.clone(),
            input_structure: None,
        }
    }

    pub(crate) fn absorb(&self, child: &Cx<'_>) {
        self.mem.absorb_peak(child.mem.peak());
    }

    pub fn tick(&mut self) -> Result<(), ExecError> {
        self.mem.release_scratch();
        let t = self.ticks;
        self.ticks = t.wrapping_add(1);
        if t.is_multiple_of(1024)
            && (self.env.cancel.load(Ordering::Relaxed)
                || self.env.stop.is_some_and(|f| f.load(Ordering::Relaxed)))
        {
            return Err(ExecError::Fatal("выполнение прервано".to_string()));
        }
        Ok(())
    }
}

pub(crate) fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("ops: {what}"))
}

pub fn value_bytes(v: &QValue) -> u64 {
    value_model(v)
}

pub fn row_bytes(r: &[QValue]) -> u64 {
    row_model(r)
}

pub fn row_cap_bytes(r: &Vec<QValue>) -> u64 {
    row_cap_model(r)
}

pub(crate) fn nulls(n: usize) -> Row {
    vec![QValue::Null; n]
}

pub(crate) fn concat(l: &[QValue], r: &[QValue], cx: &Cx<'_>) -> Result<Row, ExecError> {
    cx.mem.grow_scratch(block(
        ((l.len() + r.len()) * crate::model::VALUE_SIZE) as u64,
    ))?;
    let mut v = Vec::with_capacity(l.len() + r.len());
    v.extend_from_slice(l);
    v.extend_from_slice(r);
    Ok(v)
}

pub(crate) fn rows_push_growth(rows: &[Row], cx: &Cx<'_>, cap: usize) -> Result<u64, ExecError> {
    let (grow, release) =
        crate::model::vec_push_growth(rows.len(), cap, std::mem::size_of::<Row>());
    if grow > 0 {
        cx.mem.grow(grow)?;
    }
    Ok(release)
}

pub struct Empty {
    done: bool,
}

impl Empty {
    pub fn new() -> Empty {
        Empty { done: false }
    }
}

impl Default for Empty {
    fn default() -> Self {
        Empty::new()
    }
}

impl<'a> Op<'a> for Empty {
    fn next(&mut self, _cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        Ok(if std::mem::replace(&mut self.done, true) {
            None
        } else {
            Some(Vec::new())
        })
    }
}

pub struct Filter<'a> {
    input: BoxOp<'a>,
    pred: query_ir::PExpr,
}

impl<'a> Filter<'a> {
    pub fn new(input: BoxOp<'a>, pred: query_ir::PExpr) -> Filter<'a> {
        Filter { input, pred }
    }
}

impl<'a> Op<'a> for Filter<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            if crate::eval::truth(&eval(&self.pred, &row, cx)?) == Some(true) {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }
}

pub struct Project<'a> {
    input: BoxOp<'a>,
    exprs: Vec<query_ir::PExpr>,
}

impl<'a> Project<'a> {
    pub fn new(input: BoxOp<'a>, exprs: Vec<query_ir::PExpr>) -> Project<'a> {
        Project { input, exprs }
    }
}

impl<'a> Op<'a> for Project<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let Some(row) = self.input.next(cx)? else {
            return Ok(None);
        };
        cx.tick()?;
        cx.mem.grow_scratch(crate::model::vec_block(
            self.exprs.len(),
            crate::model::VALUE_SIZE,
        ))?;
        let mut out = Vec::with_capacity(self.exprs.len());
        for e in &self.exprs {
            out.push(eval(e, &row, cx)?);
        }
        Ok(Some(out))
    }
}

pub struct Limit<'a> {
    input: BoxOp<'a>,
    left: u64,
}

impl<'a> Limit<'a> {
    pub fn new(input: BoxOp<'a>, n: u64) -> Limit<'a> {
        Limit { input, left: n }
    }
}

impl<'a> Op<'a> for Limit<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.left == 0 {
            return Ok(None);
        }
        let r = self.input.next(cx)?;
        if r.is_some() {
            self.left -= 1;
            if self.left == 0 {
                self.input.close(cx);
            }
        }
        Ok(r)
    }
}

impl Drop for Cx<'_> {
    fn drop(&mut self) {
        self.release_inputs();
    }
}
