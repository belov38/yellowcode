use memory_budget::MemoryBudget;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering::SeqCst};

pub struct Counting;
static LOCK: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicU64 = AtomicU64::new(0);
static BASE_LIVE: AtomicU64 = AtomicU64::new(0);
static BASE_PAID: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
static UNPAID: AtomicU64 = AtomicU64::new(0);
static DEFICIT_EVENT: AtomicU64 = AtomicU64::new(0);
static DEFICIT_SIZE: AtomicU64 = AtomicU64::new(0);
static DEFICIT_LIVE: AtomicU64 = AtomicU64::new(0);
static DEFICIT_PAID: AtomicU64 = AtomicU64::new(0);
static OBSERVER: AtomicPtr<MemoryBudget> = AtomicPtr::new(std::ptr::null_mut());
struct Locked;
impl Locked {
    fn new() -> Self {
        while LOCK.compare_exchange(false, true, SeqCst, SeqCst).is_err() {
            std::hint::spin_loop();
        }
        Self
    }
}
impl Drop for Locked {
    fn drop(&mut self) {
        LOCK.store(false, SeqCst);
    }
}
fn sample(event: u64, size: u64) {
    let observer = OBSERVER.load(SeqCst);
    if observer.is_null() {
        return;
    }
    let ledger = unsafe { &*observer };
    let paid = i128::from(ledger.current_bytes()) - i128::from(BASE_PAID.load(SeqCst));
    let live = i128::from(LIVE.load(SeqCst)) - i128::from(BASE_LIVE.load(SeqCst));
    let peak = u64::try_from(live.max(0)).unwrap_or(u64::MAX);
    let unpaid = u64::try_from((live - paid).max(0)).unwrap_or(u64::MAX);
    PEAK.fetch_max(peak, SeqCst);
    if unpaid > UNPAID.load(SeqCst) {
        UNPAID.store(unpaid, SeqCst);
        DEFICIT_EVENT.store(event, SeqCst);
        DEFICIT_SIZE.store(size, SeqCst);
        DEFICIT_LIVE.store(LIVE.load(SeqCst), SeqCst);
        DEFICIT_PAID.store(ledger.current_bytes(), SeqCst);
    }
}
fn add(n: usize) {
    LIVE.fetch_add(n as u64, SeqCst);
    sample(1, n as u64);
}
fn remove(n: usize) {
    LIVE.fetch_sub(n as u64, SeqCst);
    sample(2, n as u64);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _event = Locked::new();
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _event = Locked::new();
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _event = Locked::new();
        unsafe { System.dealloc(ptr, layout) };
        remove(layout.size());
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _event = Locked::new();
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            add(size);
            remove(layout.size());
        }
        next
    }
}

#[allow(
    dead_code,
    reason = "examples and the Windows join unit tests include this module without it"
)]
pub fn isolated(module: &str, name: &str, body: impl FnOnce()) {
    const CHILD: &str = "ALLOCATION_ACCOUNTING_CHILD";
    let test = match module.split_once("::") {
        Some((_, path)) => format!("{path}::{name}"),
        None => name.to_owned(),
    };
    let own_process = std::env::var("NEXTEST_EXECUTION_MODE").as_deref() == Ok("process-per-test")
        || std::env::var(CHILD).as_deref() == Ok(test.as_str());
    if own_process {
        body();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test.as_str(), "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD, &test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{test}: {stdout}{stderr}");
    assert!(
        stdout.contains(&format!("test {test} ... ok")),
        "{test} did not run alone: {stdout}"
    );
}

pub struct Sampling<'a> {
    _ledger: &'a MemoryBudget,
}
#[derive(Debug)]
pub struct Observation {
    pub peak: u64,
    pub unpaid: u64,
    pub deficit_event: &'static str,
    pub deficit_size: u64,
    pub deficit_live_delta: i128,
    pub deficit_paid_delta: i128,
    pub ending_live_delta: i128,
    pub ending_paid_delta: i128,
}
impl std::fmt::Display for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "peak={} unpaid={} event={} size={} live_delta={} paid_delta={} ending_live_delta={} ending_paid_delta={}",
            self.peak,
            self.unpaid,
            self.deficit_event,
            self.deficit_size,
            self.deficit_live_delta,
            self.deficit_paid_delta,
            self.ending_live_delta,
            self.ending_paid_delta
        )
    }
}
impl<'a> Sampling<'a> {
    pub fn start(ledger: &'a MemoryBudget) -> Self {
        assert!(OBSERVER.load(SeqCst).is_null());
        let _event = Locked::new();
        BASE_LIVE.store(LIVE.load(SeqCst), SeqCst);
        BASE_PAID.store(ledger.current_bytes(), SeqCst);
        PEAK.store(0, SeqCst);
        UNPAID.store(0, SeqCst);
        DEFICIT_EVENT.store(0, SeqCst);
        DEFICIT_SIZE.store(0, SeqCst);
        DEFICIT_LIVE.store(LIVE.load(SeqCst), SeqCst);
        DEFICIT_PAID.store(ledger.current_bytes(), SeqCst);
        OBSERVER.store(std::ptr::from_ref(ledger).cast_mut(), SeqCst);
        Self { _ledger: ledger }
    }
    pub fn finish(self) -> Observation {
        {
            let _event = Locked::new();
            sample(3, 0);
            OBSERVER.store(std::ptr::null_mut(), SeqCst);
            Observation {
                peak: PEAK.load(SeqCst),
                unpaid: UNPAID.load(SeqCst),
                deficit_event: match DEFICIT_EVENT.load(SeqCst) {
                    1 => "allocation",
                    2 => "deallocation",
                    3 => "finish",
                    _ => "none",
                },
                deficit_size: DEFICIT_SIZE.load(SeqCst),
                deficit_live_delta: i128::from(DEFICIT_LIVE.load(SeqCst))
                    - i128::from(BASE_LIVE.load(SeqCst)),
                deficit_paid_delta: i128::from(DEFICIT_PAID.load(SeqCst))
                    - i128::from(BASE_PAID.load(SeqCst)),
                ending_live_delta: i128::from(LIVE.load(SeqCst))
                    - i128::from(BASE_LIVE.load(SeqCst)),
                ending_paid_delta: i128::from(self._ledger.current_bytes())
                    - i128::from(BASE_PAID.load(SeqCst)),
            }
        }
    }
}
impl Drop for Sampling<'_> {
    fn drop(&mut self) {
        let _event = Locked::new();
        OBSERVER.store(std::ptr::null_mut(), SeqCst);
    }
}
