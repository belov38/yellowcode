use super::{Category, Limits, Mutex, State};
use crate::BudgetError;
use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::fmt;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};

struct LedgerNode {
    owners: AtomicUsize,
    state: Mutex<State>,
    over: AtomicBool,
}

pub(super) struct LedgerRef {
    pointer: NonNull<LedgerNode>,
}
unsafe impl Send for LedgerRef {}
unsafe impl Sync for LedgerRef {}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Mutex<State>>();
};

impl LedgerRef {
    pub(super) fn new(limits: Limits) -> Self {
        match Self::allocate(State::new(limits)) {
            Some(owner) => owner,
            None => handle_alloc_error(Layout::new::<LedgerNode>()),
        }
    }
    pub(super) fn try_new(limits: Limits) -> Result<Self, BudgetError> {
        let mut state = State::new(limits);
        let bytes =
            u64::try_from(Layout::new::<LedgerNode>().size()).map_err(|_| BudgetError::Overflow)?;
        state.add(Category::Metadata, bytes)?;
        Self::allocate(state).ok_or(BudgetError::AllocationFailed)
    }
    fn allocate(state: State) -> Option<Self> {
        let layout = Layout::new::<LedgerNode>();
        let raw = unsafe { alloc(layout) }.cast::<LedgerNode>();
        let pointer = NonNull::new(raw)?;
        unsafe {
            raw.write(LedgerNode {
                owners: AtomicUsize::new(1),
                state: Mutex::new(state),
                over: AtomicBool::new(false),
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
        unsafe { std::ptr::drop_in_place(self.pointer.as_ptr()) };
        unsafe { dealloc(self.pointer.as_ptr().cast(), Layout::new::<LedgerNode>()) };
    }
}
