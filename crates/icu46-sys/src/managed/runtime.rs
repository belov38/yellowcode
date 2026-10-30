use super::abi::NativeOwner;
use super::allocation::{AllocationScope, FailureCause, release};
use memory_budget::{Lock, MemoryBudget};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

unsafe extern "C" {
    fn u_setMemoryFunctions_46(
        context: *const c_void,
        allocate: unsafe extern "C" fn(*const c_void, usize) -> *mut c_void,
        reallocate: unsafe extern "C" fn(*const c_void, *mut c_void, usize) -> *mut c_void,
        free: unsafe extern "C" fn(*const c_void, *mut c_void),
        status: *mut i32,
    );
    fn managed_owned_install_46(
        alloc: unsafe extern "C" fn(*const c_void, NativeOwner, usize) -> *mut c_void,
        failed: unsafe extern "C" fn(*const c_void) -> i8,
        status: *mut i32,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Init(u8),
    CollatorOpen,
    Strength,
    SortKey,
    SortKeyLength,
    RegexOpen,
    RegexCall,
    Other(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCause {
    Allocation(FailureCause),
    Status(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFailure {
    pub cause: NativeCause,
    pub owner: NativeOwner,
    pub boundary: Boundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    LateBinding,
    ForeignRoot,
    ManagedContextRequired,
    Unbound,
    Reentrant,
    Native(i32),
    Cancelled,
    RestartRequired(NativeFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Unbound,
    Legacy,
    Managed,
    Quarantined(NativeFailure),
}

struct State {
    mode: Mode,
    root: Option<MemoryBudget>,
    initialized: bool,
}

static RUNTIME: Lock<State> = Lock::new(State {
    mode: Mode::Unbound,
    root: None,
    initialized: false,
});
static ACTIVE: AtomicPtr<NativeCall> = AtomicPtr::new(ptr::null_mut());
static HOLDER: AtomicUsize = AtomicUsize::new(0);
static WAITING: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static STRAY: AtomicUsize = AtomicUsize::new(0);
static LEGACY_REFUSALS: AtomicUsize = AtomicUsize::new(0);
static QUARANTINED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static MARK: u8 = const { 0 };
}
fn thread_mark() -> usize {
    MARK.with(|mark| ptr::from_ref(mark) as usize)
}

pub struct NativeCall {
    scope: AllocationScope,
    boundary: Cell<Boundary>,
    thread: usize,
}

impl NativeCall {
    pub fn set_boundary(&self, boundary: Boundary) {
        self.boundary.set(boundary);
    }
    pub fn failed(&self) -> bool {
        self.scope.failure().is_some()
    }
}

struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.store(ptr::null_mut(), Ordering::Release);
        HOLDER.store(0, Ordering::Release);
    }
}

fn active() -> Option<&'static NativeCall> {
    let call = ACTIVE.load(Ordering::Acquire);
    if call.is_null() {
        return None;
    }
    let call = unsafe { &*call };
    (call.thread == thread_mark()).then_some(call)
}

#[cfg(feature = "testing")]
static REFUSE_AT: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "testing")]
static PAUSE_AT: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "testing")]
static PAUSED: AtomicBool = AtomicBool::new(false);
struct NativeHeap;
unsafe impl GlobalAlloc for NativeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let attempt = CALLBACKS.fetch_add(1, Ordering::SeqCst) + 1;
        #[cfg(feature = "testing")]
        if attempt == PAUSE_AT.load(Ordering::SeqCst) {
            PAUSED.store(true, Ordering::SeqCst);
            while PAUSED.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
        }
        #[cfg(feature = "testing")]
        if attempt == REFUSE_AT.load(Ordering::SeqCst) {
            return ptr::null_mut();
        }
        #[cfg(not(feature = "testing"))]
        let _ = attempt;
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

unsafe extern "C" fn allocate(context: *const c_void, size: usize) -> *mut c_void {
    unsafe { owned_allocate(context, NativeOwner { role: 0, site: 0 }, size) }
}
unsafe extern "C" fn owned_allocate(
    _: *const c_void,
    owner: NativeOwner,
    size: usize,
) -> *mut c_void {
    let Some(call) = active() else {
        STRAY.fetch_add(1, Ordering::SeqCst);
        return ptr::null_mut();
    };
    unsafe { call.scope.allocate(&NativeHeap, owner, size) }
}
unsafe extern "C" fn reallocate(
    _: *const c_void,
    pointer: *mut c_void,
    size: usize,
) -> *mut c_void {
    if size == 0 {
        unsafe { release(&NativeHeap, pointer) };
        return ptr::null_mut();
    }
    let Some(call) = active() else {
        STRAY.fetch_add(1, Ordering::SeqCst);
        return ptr::null_mut();
    };
    unsafe {
        call.scope
            .reallocate(&NativeHeap, NativeOwner { role: 0, site: 0 }, pointer, size)
    }
}
unsafe extern "C" fn free(_: *const c_void, pointer: *mut c_void) {
    unsafe { release(&NativeHeap, pointer) };
}
unsafe extern "C" fn failed(_: *const c_void) -> i8 {
    i8::from(active().is_some_and(NativeCall::failed))
}

fn reject_reentry() -> Result<(), RuntimeError> {
    if HOLDER.load(Ordering::Acquire) == thread_mark() {
        return Err(RuntimeError::Reentrant);
    }
    Ok(())
}

pub fn bind(budget: &MemoryBudget) -> Result<Mode, RuntimeError> {
    reject_reentry()?;
    let root = budget.root_budget();
    let mut state = RUNTIME.lock();
    match state.mode {
        Mode::Legacy => Err(RuntimeError::LateBinding),
        Mode::Unbound => {
            let mut status = 0;
            unsafe {
                u_setMemoryFunctions_46(ptr::null(), allocate, reallocate, free, &mut status);
                managed_owned_install_46(owned_allocate, failed, &mut status);
            }
            if status > 0 {
                state.mode = Mode::Legacy;
                return Err(RuntimeError::LateBinding);
            }
            state.root = Some(root);
            state.mode = Mode::Managed;
            Ok(state.mode)
        }
        Mode::Managed | Mode::Quarantined(_) => match &state.root {
            Some(bound) if bound.same_instance(&root) => Ok(state.mode),
            _ => Err(RuntimeError::ForeignRoot),
        },
    }
}

pub fn legacy_entry() -> Result<(), RuntimeError> {
    reject_reentry()?;
    let mut state = RUNTIME.lock();
    match state.mode {
        Mode::Unbound => {
            state.mode = Mode::Legacy;
            Ok(())
        }
        Mode::Legacy => Ok(()),
        Mode::Managed => {
            LEGACY_REFUSALS.fetch_add(1, Ordering::SeqCst);
            Err(RuntimeError::ManagedContextRequired)
        }
        Mode::Quarantined(first) => {
            LEGACY_REFUSALS.fetch_add(1, Ordering::SeqCst);
            Err(RuntimeError::RestartRequired(first))
        }
    }
}

static NEVER: AtomicBool = AtomicBool::new(false);

pub fn enter<T>(
    operation: &MemoryBudget,
    boundary: Boundary,
    work: impl FnOnce(&NativeCall) -> Result<T, i32>,
) -> Result<T, RuntimeError> {
    enter_cancellable(operation, &NEVER, boundary, work)
}

pub fn enter_cancellable<T>(
    operation: &MemoryBudget,
    cancel: &AtomicBool,
    boundary: Boundary,
    work: impl FnOnce(&NativeCall) -> Result<T, i32>,
) -> Result<T, RuntimeError> {
    reject_reentry()?;
    WAITING.fetch_add(1, Ordering::SeqCst);
    let mut state = RUNTIME.lock();
    WAITING.fetch_sub(1, Ordering::SeqCst);
    match state.mode {
        Mode::Unbound => return Err(RuntimeError::Unbound),
        Mode::Legacy => return Err(RuntimeError::LateBinding),
        Mode::Quarantined(first) => return Err(RuntimeError::RestartRequired(first)),
        Mode::Managed => {}
    }
    if !state
        .root
        .as_ref()
        .is_some_and(|root| root.same_instance(&operation.root_budget()))
    {
        return Err(RuntimeError::ForeignRoot);
    }
    if cancel.load(Ordering::Acquire) {
        return Err(RuntimeError::Cancelled);
    }
    let call = NativeCall {
        scope: AllocationScope::new(operation),
        boundary: Cell::new(boundary),
        thread: thread_mark(),
    };
    let frame = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _active = Active;
        HOLDER.store(call.thread, Ordering::Release);
        ACTIVE.store(ptr::from_ref(&call).cast_mut(), Ordering::Release);
        let init = if state.initialized {
            Ok(())
        } else {
            let result = crate::init_managed(&call);
            if result.is_ok() && !call.failed() {
                call.set_boundary(boundary);
            }
            result
        };
        match init {
            Ok(()) if call.failed() => Some(Err(0)),
            Ok(()) => {
                state.initialized = true;
                if cancel.load(Ordering::Acquire) {
                    None
                } else {
                    Some(work(&call))
                }
            }
            Err(code) => Some(Err(code)),
        }
    }));
    let outcome = match frame {
        Ok(outcome) => outcome,
        Err(payload) => {
            if let Some(failure) = call.scope.failure() {
                quarantine(
                    &mut state,
                    NativeFailure {
                        cause: NativeCause::Allocation(failure.cause),
                        owner: failure.owner,
                        boundary: call.boundary.get(),
                    },
                );
            }
            drop(state);
            std::panic::resume_unwind(payload);
        }
    };
    let first = match (call.scope.failure(), &outcome) {
        (Some(failure), _) => Some(NativeFailure {
            cause: NativeCause::Allocation(failure.cause),
            owner: failure.owner,
            boundary: call.boundary.get(),
        }),
        (None, Some(Err(code))) if *code == crate::ffi::U_MEMORY_ALLOCATION_ERROR => {
            Some(NativeFailure {
                cause: NativeCause::Status(*code),
                owner: NativeOwner::default(),
                boundary: call.boundary.get(),
            })
        }
        _ => None,
    };
    if let Some(first) = first {
        quarantine(&mut state, first);
        drop(state);
        drop(outcome);
        return Err(RuntimeError::RestartRequired(first));
    }
    if !state.initialized {
        let code = match outcome {
            Some(Err(code)) => code,
            _ => 0,
        };
        let first = NativeFailure {
            cause: NativeCause::Status(code),
            owner: NativeOwner::default(),
            boundary: call.boundary.get(),
        };
        quarantine(&mut state, first);
        return Err(RuntimeError::RestartRequired(first));
    }
    drop(state);
    match outcome {
        Some(outcome) if !cancel.load(Ordering::Acquire) => outcome.map_err(RuntimeError::Native),
        _ => Err(RuntimeError::Cancelled),
    }
}

fn quarantine(state: &mut State, first: NativeFailure) {
    state.mode = Mode::Quarantined(first);
    QUARANTINED.store(true, Ordering::Release);
}

pub fn quarantined() -> Option<NativeFailure> {
    if !QUARANTINED.load(Ordering::Acquire) {
        return None;
    }
    match RUNTIME.lock().mode {
        Mode::Quarantined(first) => Some(first),
        _ => None,
    }
}

pub fn mode() -> Mode {
    RUNTIME.lock().mode
}

pub fn process_root() -> Option<MemoryBudget> {
    RUNTIME.lock().root.as_ref().map(MemoryBudget::root_budget)
}
pub fn waiting() -> usize {
    WAITING.load(Ordering::SeqCst)
}
pub fn callbacks() -> usize {
    CALLBACKS.load(Ordering::SeqCst)
}
pub fn legacy_refusals() -> usize {
    LEGACY_REFUSALS.load(Ordering::SeqCst)
}
pub fn stray() -> usize {
    STRAY.load(Ordering::SeqCst)
}
#[cfg(feature = "testing")]
pub fn refuse_at(n: usize) {
    REFUSE_AT.store(n, Ordering::SeqCst);
}
#[cfg(feature = "testing")]
pub fn pause_at(n: usize) {
    PAUSE_AT.store(n, Ordering::SeqCst);
}
#[cfg(feature = "testing")]
pub fn paused() -> bool {
    PAUSED.load(Ordering::SeqCst)
}
#[cfg(feature = "testing")]
pub fn resume() {
    PAUSE_AT.store(0, Ordering::SeqCst);
    PAUSED.store(false, Ordering::SeqCst);
}
