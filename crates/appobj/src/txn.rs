use crate::Application;
use crate::errors::{self, keys};
use crate::locks::{Conflict, Held, LockControl, LockMode, LockTable};
use crate::session::call_ctx;
use bsl_vm::{CallError, Category, CategorySet, Execution, HostError};
use ibschema::store::{ReadTxn, StoreError, StoreResult, WriteTxn};
use memory_budget::{Limits, MemoryBudget};
use platform_catalog::messages::Lang;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub trait TxnEnv {
    fn txn_writer(&self) -> StoreResult<Box<dyn WriteTxn>>;
    fn txn_reader(&self) -> StoreResult<Box<dyn ReadTxn + Send>>;
    fn txn_data_changed(&self);
}

impl TxnEnv for Application {
    fn txn_writer(&self) -> StoreResult<Box<dyn WriteTxn>> {
        let wait: Duration = self.options().lock_timeout;
        self.store().write(wait)
    }
    fn txn_reader(&self) -> StoreResult<Box<dyn ReadTxn + Send>> {
        self.store().read()
    }
    fn txn_data_changed(&self) {
        self.bump_write_epoch();
    }
}

#[derive(Debug)]
pub enum TxnError {
    NotActive,
    Poisoned,
    InUse,
    Store(StoreError),
}

impl std::fmt::Display for TxnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxnError::NotActive => {
                f.write_str(&errors::render(keys::TRANSACTION_NOT_ACTIVE, &[], Lang::Ru))
            }
            TxnError::Poisoned => {
                f.write_str(&errors::render(keys::TRANSACTION_POISONED, &[], Lang::Ru))
            }
            TxnError::InUse => f.write_str("the write transaction of the call is in use"),
            TxnError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TxnError {}

fn l5(key: &'static str) -> HostError {
    HostError::text(
        &errors::render(key, &[], Lang::Ru),
        &errors::render(key, &[], Lang::En),
    )
}

pub(crate) fn lock_conflict() -> HostError {
    let mut e = l5(keys::DB_LOCKING);
    e.categories = Some(CategorySet::default());
    e
}

impl TxnError {
    pub fn host(&self) -> HostError {
        match self {
            TxnError::NotActive => l5(keys::TRANSACTION_NOT_ACTIVE),
            TxnError::Poisoned => {
                let mut e = l5(keys::TRANSACTION_POISONED);
                e.categories = Some(CategorySet::of(&[Category::OtherError]));
                e
            }
            TxnError::InUse => {
                let t = "internal: the write transaction of the call is in use";
                HostError::text(t, t)
            }
            TxnError::Store(StoreError::WriterBusy) => lock_conflict(),
            TxnError::Store(e) => crate::read::store_error(e),
        }
    }

    pub(crate) fn call_error(self, ex: &Execution) -> CallError {
        match self {
            TxnError::Store(e @ StoreError::Memory(_)) => crate::read::store_call_error(ex, e),
            other => other.host().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    Inner,
    Committed,
    RolledBack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rollback {
    Inner,
    Ended,
}

thread_local! {
    static WRITERS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn thread_holds_writer() -> bool {
    WRITERS.with(|w| w.get() > 0)
}

fn writer_opened() {
    WRITERS.with(|w| w.set(w.get().saturating_add(1)));
}

fn writer_closed() {
    WRITERS.with(|w| w.set(w.get().saturating_sub(1)));
}

#[derive(Default)]
pub struct CallTxn {
    depth: Cell<u32>,
    poisoned: Cell<bool>,
    id: Cell<u64>,
    last_id: Cell<u64>,
    rolled_back: RefCell<Vec<(u64, u64)>>,
    writer: RefCell<Option<Box<dyn WriteTxn>>>,
    undo: RefCell<Vec<Box<dyn FnOnce()>>>,
    held: RefCell<Option<Held>>,
    control: Cell<Option<LockControl>>,
}

impl Drop for CallTxn {
    fn drop(&mut self) {
        if self.writer.get_mut().take().is_some() {
            writer_closed();
        }
        for undo in std::mem::take(self.undo.get_mut()).into_iter().rev() {
            undo();
        }
    }
}

impl CallTxn {
    pub fn of_call(ex: &mut Execution) -> Option<Rc<CallTxn>> {
        call_ctx(ex).map(|c| c.txn.clone())
    }

    pub fn active(&self) -> bool {
        self.depth.get() > 0
    }

    pub fn depth(&self) -> u32 {
        self.depth.get()
    }

    pub fn poisoned(&self) -> bool {
        self.depth.get() > 0 && self.poisoned.get()
    }

    pub fn id(&self) -> Option<u64> {
        (self.depth.get() > 0).then(|| self.id.get())
    }

    pub fn was_rolled_back(&self, id: u64) -> bool {
        self.rolled_back.try_borrow().is_ok_and(|ranges| {
            let at = ranges.partition_point(|&(_, last)| last < id);
            ranges.get(at).is_some_and(|&(first, _)| first <= id)
        })
    }

    pub fn has_writer(&self) -> bool {
        match self.writer.try_borrow() {
            Ok(w) => w.is_some(),
            Err(_) => true,
        }
    }

    pub fn begin(&self) {
        let depth = self.depth.get();
        if depth == 0 {
            let id = self.last_id.get().saturating_add(1);
            self.last_id.set(id);
            self.id.set(id);
            self.poisoned.set(false);
        }
        self.depth.set(depth.saturating_add(1));
    }

    pub fn begin_with(&self, control: Option<LockControl>) {
        if self.depth.get() == 0 {
            self.control.set(control);
        }
        self.begin();
    }

    pub fn control(&self) -> Option<LockControl> {
        if self.depth.get() > 0 {
            self.control.get()
        } else {
            None
        }
    }

    pub fn lock(
        &self,
        table: &Arc<LockTable>,
        key: &str,
        mode: LockMode,
        wait: Duration,
    ) -> Result<(), Conflict> {
        let owner = {
            let mut held = self.held.try_borrow_mut().map_err(|_| Conflict)?;
            held.get_or_insert_with(|| table.new_owner()).owner()
        };
        table.acquire(owner, key, mode, wait)
    }

    pub fn holds_locks(&self) -> bool {
        self.held
            .try_borrow()
            .is_ok_and(|held| held.as_ref().is_some_and(Held::holds_any))
    }

    pub fn commit(&self, env: &(impl TxnEnv + ?Sized)) -> Result<Commit, TxnError> {
        let depth = self.depth.get();
        if depth == 0 {
            return Err(TxnError::NotActive);
        }
        if depth > 1 {
            self.depth.set(depth - 1);
            return Ok(Commit::Inner);
        }
        let writer = self.take_writer()?;
        let id = self.id.get();
        let poisoned = self.poisoned.get();
        self.reset();
        if poisoned {
            let wrote = writer.is_some();
            drop(writer);
            self.mark_rolled_back(id);
            self.ended(true);
            if wrote {
                env.txn_data_changed();
            }
            return Ok(Commit::RolledBack);
        }
        let Some(writer) = writer else {
            self.ended(false);
            return Ok(Commit::Committed);
        };
        match writer.commit() {
            Ok(()) => {
                env.txn_data_changed();
                self.ended(false);
                Ok(Commit::Committed)
            }
            Err(e) => {
                self.mark_rolled_back(id);
                self.ended(true);
                env.txn_data_changed();
                Err(TxnError::Store(e))
            }
        }
    }

    pub fn rollback(&self, env: &(impl TxnEnv + ?Sized)) -> Result<Rollback, TxnError> {
        let depth = self.depth.get();
        if depth == 0 {
            return Err(TxnError::NotActive);
        }
        if depth > 1 {
            self.depth.set(depth - 1);
            self.poisoned.set(true);
            return Ok(Rollback::Inner);
        }
        let writer = self.take_writer()?;
        let id = self.id.get();
        self.reset();
        self.mark_rolled_back(id);
        let wrote = writer.is_some();
        drop(writer);
        self.ended(true);
        if wrote {
            env.txn_data_changed();
        }
        Ok(Rollback::Ended)
    }

    pub fn on_rollback(&self, undo: impl FnOnce() + 'static) -> bool {
        if self.depth.get() == 0 {
            return false;
        }
        match self.undo.try_borrow_mut() {
            Ok(mut registered) => {
                registered.push(Box::new(undo));
                true
            }
            Err(_) => false,
        }
    }

    pub fn poison(&self) {
        if self.depth.get() > 0 {
            self.poisoned.set(true);
        }
    }

    pub fn check(&self) -> Result<(), TxnError> {
        if self.poisoned() {
            Err(TxnError::Poisoned)
        } else {
            Ok(())
        }
    }

    pub fn with_write<R>(
        &self,
        env: &(impl TxnEnv + ?Sized),
        f: impl FnOnce(&mut dyn WriteTxn) -> StoreResult<R>,
    ) -> Result<R, TxnError> {
        if self.depth.get() == 0 {
            return Err(TxnError::NotActive);
        }
        self.check()?;
        let mut slot = self.writer.try_borrow_mut().map_err(|_| TxnError::InUse)?;
        if slot.is_none() {
            match env.txn_writer() {
                Ok(w) => {
                    writer_opened();
                    *slot = Some(w);
                }
                Err(e) => {
                    drop(slot);
                    if matches!(e, StoreError::WriterBusy) {
                        self.poison();
                    }
                    return Err(TxnError::Store(e));
                }
            }
        }
        let Some(w) = slot.as_mut() else {
            return Err(TxnError::InUse);
        };
        let result = f(&mut **w);
        drop(slot);
        env.txn_data_changed();
        result.map_err(TxnError::Store)
    }

    pub(crate) fn finish(&self, env: &(impl TxnEnv + ?Sized)) -> u32 {
        let open = self.depth.get();
        let id = self.id.get();
        let writer = self.take_writer().unwrap_or(None);
        self.reset();
        let wrote = writer.is_some();
        drop(writer);
        if open > 0 {
            self.mark_rolled_back(id);
            self.ended(true);
        } else {
            self.ended(false);
        }
        if wrote {
            env.txn_data_changed();
        }
        open
    }

    pub(crate) fn view(&self, env: &(impl TxnEnv + ?Sized)) -> Result<View<'_>, TxnError> {
        self.check()?;
        match self.writer.try_borrow_mut() {
            Ok(guard) => {
                if let Ok(own) = RefMut::filter_map(guard, Option::as_mut) {
                    return Ok(View::Own(own));
                }
            }
            Err(_) => return Err(TxnError::InUse),
        }
        env.txn_reader()
            .map(View::Snapshot)
            .map_err(TxnError::Store)
    }

    pub(crate) fn writer_ref(&self) -> Option<Ref<'_, Box<dyn WriteTxn>>> {
        Ref::filter_map(self.writer.try_borrow().ok()?, Option::as_ref).ok()
    }

    fn take_writer(&self) -> Result<Option<Box<dyn WriteTxn>>, TxnError> {
        let mut slot = self.writer.try_borrow_mut().map_err(|_| TxnError::InUse)?;
        let writer = slot.take();
        if writer.is_some() {
            writer_closed();
        }
        Ok(writer)
    }

    fn reset(&self) {
        self.depth.set(0);
        self.poisoned.set(false);
        self.id.set(0);
        self.control.set(None);
    }

    fn ended(&self, rolled_back: bool) {
        if rolled_back {
            self.run_undo();
        } else {
            self.forget_undo();
        }
        if let Ok(mut held) = self.held.try_borrow_mut() {
            drop(held.take());
        }
    }

    fn run_undo(&self) {
        let undo = self
            .undo
            .try_borrow_mut()
            .map(|mut registered| std::mem::take(&mut *registered))
            .unwrap_or_default();
        for undo in undo.into_iter().rev() {
            undo();
        }
    }

    fn forget_undo(&self) {
        if let Ok(mut registered) = self.undo.try_borrow_mut() {
            registered.clear();
        }
    }

    fn mark_rolled_back(&self, id: u64) {
        if id == 0 {
            return;
        }
        let Ok(mut ranges) = self.rolled_back.try_borrow_mut() else {
            return;
        };
        match ranges.last_mut() {
            Some((_, last)) if last.checked_add(1) == Some(id) => *last = id,
            _ => ranges.push((id, id)),
        }
    }
}

pub(crate) enum View<'a> {
    Snapshot(Box<dyn ReadTxn + Send>),
    Own(RefMut<'a, Box<dyn WriteTxn>>),
}

impl View<'_> {
    pub(crate) fn read_memory(&mut self, limit: u64) -> StoreResult<MemoryBudget> {
        match self {
            View::Snapshot(t) => t.read_memory(limit),
            View::Own(_) => {
                MemoryBudget::try_new(Limits::new(limit)).map_err(StoreError::from_budget)
            }
        }
    }

    pub(crate) fn txn(&mut self) -> &mut dyn ReadTxn {
        match self {
            View::Snapshot(t) => {
                let r: &mut dyn ReadTxn = &mut **t;
                r
            }
            View::Own(w) => {
                let r: &mut dyn ReadTxn = &mut ***w;
                r
            }
        }
    }

    pub(crate) fn is_own(&self) -> bool {
        matches!(self, View::Own(_))
    }

    pub(crate) fn into_snapshot(self) -> Option<Box<dyn ReadTxn + Send>> {
        match self {
            View::Snapshot(t) => Some(t),
            View::Own(_) => None,
        }
    }
}
