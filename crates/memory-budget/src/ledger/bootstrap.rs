use super::{Category, Limits, MemoryBudget, Mutex, Reservation, State};
use crate::BudgetError;
use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::fmt;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};

struct LedgerNode {
    owners: AtomicUsize,
    state: Mutex<State>,
    over: AtomicBool,
    current: AtomicU64,
    parent: Option<Reservation>,
}

pub(super) struct LedgerRef {
    pointer: NonNull<LedgerNode>,
}
unsafe impl Send for LedgerRef {}
unsafe impl Sync for LedgerRef {}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Mutex<State>>();
    send_sync::<Option<Reservation>>();
};

impl LedgerRef {
    pub(super) fn new(limits: Limits) -> Self {
        match Self::allocate(State::new(limits), None) {
            Some(owner) => owner,
            None => handle_alloc_error(Layout::new::<LedgerNode>()),
        }
    }
    pub(super) fn try_new(limits: Limits) -> Result<Self, BudgetError> {
        let mut state = State::new(limits);
        let bytes =
            u64::try_from(Layout::new::<LedgerNode>().size()).map_err(|_| BudgetError::Overflow)?;
        state.add(Category::Metadata, bytes)?;
        Self::allocate(state, None).ok_or(BudgetError::AllocationFailed)
    }
    pub(super) fn try_child(limits: Limits, parent: &MemoryBudget) -> Result<Self, BudgetError> {
        let mut state = State::new(limits);
        let bytes =
            u64::try_from(Layout::new::<LedgerNode>().size()).map_err(|_| BudgetError::Overflow)?;
        state.add(Category::Metadata, bytes)?;
        let parent = parent.try_reserve(Category::Metadata, bytes)?;
        Self::allocate(state, Some(parent)).ok_or(BudgetError::AllocationFailed)
    }
    pub(super) fn parent(&self) -> Option<&MemoryBudget> {
        self.node().parent.as_ref().map(Reservation::budget)
    }
    fn allocate(state: State, parent: Option<Reservation>) -> Option<Self> {
        let layout = Layout::new::<LedgerNode>();
        let current = state.current;
        let raw = unsafe { alloc(layout) }.cast::<LedgerNode>();
        let pointer = NonNull::new(raw)?;
        unsafe {
            raw.write(LedgerNode {
                owners: AtomicUsize::new(1),
                state: Mutex::new(state),
                over: AtomicBool::new(false),
                current: AtomicU64::new(current),
                parent,
            });
        }
        Some(Self { pointer })
    }
    fn node(&self) -> &LedgerNode {
        unsafe { self.pointer.as_ref() }
    }
    pub(super) fn over(&self) -> &AtomicBool {
        &self.node().over
    }
    pub(super) fn current(&self) -> &AtomicU64 {
        &self.node().current
    }
    pub(super) fn ptr_eq(first: &Self, second: &Self) -> bool {
        first.pointer == second.pointer
    }
}
impl Clone for LedgerRef {
    fn clone(&self) -> Self {
        let old = self.node().owners.fetch_add(1, Ordering::Relaxed);
        if old > isize::MAX as usize {
            std::process::abort();
        }
        Self {
            pointer: self.pointer,
        }
    }
}
impl Deref for LedgerRef {
    type Target = Mutex<State>;
    fn deref(&self) -> &Self::Target {
        &self.node().state
    }
}
impl fmt::Debug for LedgerRef {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(output)
    }
}
impl Drop for LedgerRef {
    fn drop(&mut self) {
        if self.node().owners.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
        let parent = unsafe { std::ptr::read(std::ptr::addr_of!((*self.pointer.as_ptr()).parent)) };
        let guard = FreeLedger {
            pointer: self.pointer,
            _parent: parent,
        };
        unsafe { std::ptr::drop_in_place(std::ptr::addr_of_mut!((*self.pointer.as_ptr()).state)) };
        drop(guard);
    }
}

struct FreeLedger {
    pointer: NonNull<LedgerNode>,
    _parent: Option<Reservation>,
}
impl Drop for FreeLedger {
    fn drop(&mut self) {
        unsafe { dealloc(self.pointer.as_ptr().cast(), Layout::new::<LedgerNode>()) };
    }
}
