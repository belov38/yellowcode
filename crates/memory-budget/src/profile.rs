use std::cell::RefCell;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Startup,
    InfobaseOpen,
    ConfigRead,
    CfOpen,
    DtExtract,
    SpoolIndex,
    Inventory,
    Index,
    Predefined,
    Settings,
    Shells,
    Objects,
    Modules,
    Forms,
    Parts,
    Rights,
    PredefinedData,
    ConfigurationParts,
    MiscParts,
    Templates,
    Flowcharts,
    EntriesDrop,
    Metadata,
    Tree,
    Statistics,
    Validation,
    Ranges,
    Drop,
    DtTables,
}
impl Phase {
    const ALL: [Phase; 29] = [
        Self::Startup,
        Self::InfobaseOpen,
        Self::ConfigRead,
        Self::CfOpen,
        Self::DtExtract,
        Self::SpoolIndex,
        Self::Inventory,
        Self::Index,
        Self::Predefined,
        Self::Settings,
        Self::Shells,
        Self::Objects,
        Self::Modules,
        Self::Forms,
        Self::Parts,
        Self::Rights,
        Self::PredefinedData,
        Self::ConfigurationParts,
        Self::MiscParts,
        Self::Templates,
        Self::Flowcharts,
        Self::EntriesDrop,
        Self::Metadata,
        Self::Tree,
        Self::Statistics,
        Self::Validation,
        Self::Ranges,
        Self::Drop,
        Self::DtTables,
    ];
    fn from_index(index: u8) -> Phase {
        Self::ALL
            .get(usize::from(index))
            .copied()
            .unwrap_or(Self::Startup)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::InfobaseOpen => "infobase-open",
            Self::ConfigRead => "config-read",
            Self::CfOpen => "cf-open",
            Self::DtTables => "dt-tables",
            Self::DtExtract => "dt-extract",
            Self::SpoolIndex => "spool-index",
            Self::Inventory => "inventory",
            Self::Index => "index",
            Self::Predefined => "predefined",
            Self::Settings => "settings",
            Self::Shells => "shells",
            Self::Objects => "objects",
            Self::Modules => "modules",
            Self::Forms => "forms",
            Self::Parts => "parts",
            Self::Rights => "rights",
            Self::PredefinedData => "predefined-data",
            Self::ConfigurationParts => "configuration-parts",
            Self::MiscParts => "misc-parts",
            Self::Templates => "templates",
            Self::Flowcharts => "flowcharts",
            Self::EntriesDrop => "entries-drop",
            Self::Metadata => "metadata",
            Self::Tree => "tree",
            Self::Statistics => "statistics",
            Self::Validation => "validation",
            Self::Ranges => "ranges",
            Self::Drop => "drop",
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Counters {
    rss: Option<u64>,
    uordblks: Option<u64>,
    fordblks: Option<u64>,
    hblkhd: Option<u64>,
}
impl Counters {
    fn heap(self) -> Option<u64> {
        self.uordblks?.checked_add(self.hblkhd?)
    }
}
struct Number(Option<u64>);
impl std::fmt::Display for Number {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(n) => write!(f, "{n}"),
            None => f.write_str("null"),
        }
    }
}
impl std::fmt::Display for Counters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{{\"rss\":{},\"uordblks\":{},\"fordblks\":{},\"hblkhd\":{}}}",
            Number(self.rss),
            Number(self.uordblks),
            Number(self.fordblks),
            Number(self.hblkhd)
        )
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Observation {
    at_us: u128,
    probe_us: u128,
    counters: Counters,
}
#[derive(Debug)]
struct Interval {
    phase: Phase,
    entry: Observation,
    exit: Observation,
    rss_peak: Observation,
    heap_peak: Observation,
    samples: u64,
    interior_samples: u64,
    max_probe_us: u128,
    max_gap_us: u128,
}
impl Interval {
    fn new(phase: Phase, entry: Observation) -> Self {
        Self {
            phase,
            entry,
            exit: entry,
            rss_peak: entry,
            heap_peak: entry,
            samples: 1,
            interior_samples: 0,
            max_probe_us: entry.probe_us,
            max_gap_us: 0,
        }
    }
    fn record(&mut self, sample: Observation, interior: bool) {
        self.max_gap_us = self
            .max_gap_us
            .max(sample.at_us.saturating_sub(self.exit.at_us));
        self.max_probe_us = self.max_probe_us.max(sample.probe_us);
        self.samples += 1;
        self.interior_samples += u64::from(interior);
        if sample.counters.rss > self.rss_peak.counters.rss {
            self.rss_peak = sample;
        }
        if sample.counters.heap() > self.heap_peak.counters.heap() {
            self.heap_peak = sample;
        }
        self.exit = sample;
    }
    fn write(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(
            out,
            "D7PROFILE {{\"phase\":\"{}\",\"start_us\":{},\"end_us\":{},\"entry\":{},\"exit\":{},\"samples\":{},\"interior_samples\":{},\"rss_peak\":{},\"heap_peak\":{},\"rss_peak_counters\":{},\"max_probe_us\":{},\"max_gap_us\":{}}}",
            self.phase.name(),
            self.entry.at_us,
            self.exit.at_us,
            self.entry.counters,
            self.exit.counters,
            self.samples,
            self.interior_samples,
            Number(self.rss_peak.counters.rss),
            Number(self.heap_peak.counters.heap()),
            self.rss_peak.counters,
            self.max_probe_us,
            self.max_gap_us
        )
    }
}
const MAX_INTERVALS: usize = 512;
struct State {
    start: Instant,
    probe: fn() -> Counters,
    current: Interval,
    intervals: Vec<Interval>,
    overflow: u64,
    stopped: bool,
}
impl State {
    fn sample(&self) -> Observation {
        let before = self.start.elapsed().as_micros();
        let counters = (self.probe)();
        Observation {
            at_us: before,
            probe_us: self.start.elapsed().as_micros().saturating_sub(before),
            counters,
        }
    }
    fn transition(&mut self, phase: Phase) -> Phase {
        let previous = self.current.phase;
        if !self.stopped {
            let sample = self.sample();
            self.current.record(sample, false);
            let old = std::mem::replace(&mut self.current, Interval::new(phase, sample));
            if self.intervals.len() < MAX_INTERVALS {
                self.intervals.push(old);
            } else {
                self.overflow += 1;
            }
        }
        previous
    }
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FOREIGN: AtomicU64 = AtomicU64::new(0);
thread_local! { static OWNER: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) }; }

static WATCH: AtomicU8 = AtomicU8::new(0);
const SESSION: u8 = 1;
const PROGRESS: u8 = 2;
static CURRENT: AtomicU8 = AtomicU8::new(Phase::Startup as u8);
static TRANSITIONS: AtomicU64 = AtomicU64::new(0);
static ITEMS_DONE: AtomicU64 = AtomicU64::new(0);
static ITEMS_TOTAL: AtomicU64 = AtomicU64::new(0);

fn progress_enter(next: Phase) -> Phase {
    ITEMS_TOTAL.store(0, Ordering::Relaxed);
    ITEMS_DONE.store(0, Ordering::Relaxed);
    let previous = CURRENT.swap(next as u8, Ordering::Relaxed);
    TRANSITIONS.fetch_add(1, Ordering::Release);
    Phase::from_index(previous)
}

#[must_use]
pub struct PhaseGuard {
    owner: Option<Arc<Shared>>,
    previous: Phase,
    progress: Option<Phase>,
    _thread: PhantomData<Rc<()>>,
}
pub fn phase(next: Phase) -> PhaseGuard {
    let watch = WATCH.load(Ordering::Acquire);
    if watch == 0 {
        return PhaseGuard {
            owner: None,
            previous: Phase::Startup,
            progress: None,
            _thread: PhantomData,
        };
    }
    let progress = (watch & PROGRESS != 0).then(|| progress_enter(next));
    let owner = if watch & SESSION != 0 {
        OWNER.with(|owner| owner.borrow().clone())
    } else {
        None
    };
    let previous = if let Some(owner) = &owner {
        owner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transition(next)
    } else {
        if ACTIVE.load(Ordering::Acquire) {
            FOREIGN.fetch_add(1, Ordering::Relaxed);
        }
        Phase::Startup
    };
    PhaseGuard {
        owner,
        previous,
        progress,
        _thread: PhantomData,
    }
}
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.progress
            && WATCH.load(Ordering::Acquire) & PROGRESS != 0
        {
            progress_enter(previous);
        }
        if let Some(owner) = &self.owner {
            owner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .transition(self.previous);
        }
    }
}

pub fn progress_total(total: usize) {
    if WATCH.load(Ordering::Relaxed) & PROGRESS != 0 {
        ITEMS_DONE.store(0, Ordering::Relaxed);
        ITEMS_TOTAL.store(u64::try_from(total).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

pub fn progress_step() {
    if WATCH.load(Ordering::Relaxed) & PROGRESS != 0 {
        ITEMS_DONE.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub phase: Phase,
    pub transitions: u64,
    pub done: u64,
    pub total: u64,
}

pub struct ProgressObserver {
    _private: (),
}
impl ProgressObserver {
    pub fn install() -> Option<Self> {
        if WATCH.load(Ordering::Acquire) & PROGRESS != 0 {
            return None;
        }
        CURRENT.store(Phase::Startup as u8, Ordering::Relaxed);
        TRANSITIONS.store(0, Ordering::Relaxed);
        ITEMS_DONE.store(0, Ordering::Relaxed);
        ITEMS_TOTAL.store(0, Ordering::Relaxed);
        if WATCH.fetch_or(PROGRESS, Ordering::AcqRel) & PROGRESS != 0 {
            return None;
        }
        Some(Self { _private: () })
    }
    pub fn snapshot(&self) -> ProgressSnapshot {
        let transitions = TRANSITIONS.load(Ordering::Acquire);
        ProgressSnapshot {
            phase: Phase::from_index(CURRENT.load(Ordering::Relaxed)),
            transitions,
            done: ITEMS_DONE.load(Ordering::Relaxed),
            total: ITEMS_TOTAL.load(Ordering::Relaxed),
        }
    }
}
impl Drop for ProgressObserver {
    fn drop(&mut self) {
        WATCH.fetch_and(!PROGRESS, Ordering::AcqRel);
    }
}

pub struct Session {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    _thread: PhantomData<Rc<()>>,
}
impl Session {
    pub fn from_env() -> io::Result<Option<Self>> {
        Self::start(
            std::env::var_os("YELLOWCODE_MEMORY_PROFILE").is_some_and(|v| v == "1"),
            probe,
        )
    }
    fn start(enabled: bool, probe: fn() -> Counters) -> io::Result<Option<Self>> {
        if !enabled {
            return Ok(None);
        }
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "memory profiling session already active",
                )
            })?;
        FOREIGN.store(0, Ordering::Relaxed);
        let start = Instant::now();
        let counters = probe();
        let entry = Observation {
            at_us: 0,
            probe_us: start.elapsed().as_micros(),
            counters,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                start,
                probe,
                current: Interval::new(Phase::Startup, entry),
                intervals: Vec::with_capacity(MAX_INTERVALS),
                overflow: 0,
                stopped: false,
            }),
            wake: Condvar::new(),
        });
        let sampler = shared.clone();
        let worker = match std::thread::Builder::new()
            .name("memory-profile".into())
            .spawn(move || {
                let mut state = sampler.state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let (next, _) = sampler
                        .wake
                        .wait_timeout(state, Duration::from_millis(10))
                        .unwrap_or_else(|e| e.into_inner());
                    state = next;
                    if state.stopped {
                        break;
                    }
                    let sample = state.sample();
                    state.current.record(sample, true);
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                ACTIVE.store(false, Ordering::Release);
                return Err(error);
            }
        };
        OWNER.with(|owner| *owner.borrow_mut() = Some(shared.clone()));
        WATCH.fetch_or(SESSION, Ordering::AcqRel);
        Ok(Some(Self {
            shared,
            worker: Some(worker),
            _thread: PhantomData,
        }))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        WATCH.fetch_and(!SESSION, Ordering::AcqRel);
        OWNER.with(|owner| *owner.borrow_mut() = None);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let sample = state.sample();
            state.current.record(sample, false);
            state.stopped = true;
        }
        self.shared.wake.notify_all();
        let joined = self
            .worker
            .take()
            .is_none_or(|worker| worker.join().is_ok());
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = io::stderr().lock();
        for interval in state
            .intervals
            .iter()
            .chain(std::iter::once(&state.current))
        {
            let _ = interval.write(&mut out);
        }
        let foreign = FOREIGN.load(Ordering::Relaxed);
        let _ = writeln!(
            out,
            "D7PROFILE_END {{\"foreign_transitions\":{foreign},\"overflow\":{},\"sampler_joined\":{joined}}}",
            state.overflow
        );
        ACTIVE.store(false, Ordering::Release);
    }
}

fn probe() -> Counters {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut counters = Counters::default();
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut buf = [0u8; 128];
        counters.rss = std::fs::File::open("/proc/self/statm")
            .ok()
            .and_then(|mut file| {
                let n = file.read(&mut buf).ok()?;
                let pages = std::str::from_utf8(buf.get(..n)?)
                    .ok()?
                    .split_whitespace()
                    .nth(1)?
                    .parse::<u64>()
                    .ok()?;
                let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                pages.checked_mul(u64::try_from(size).ok()?)
            });
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let info = unsafe { libc::mallinfo2() };
        counters.uordblks = u64::try_from(info.uordblks).ok();
        counters.fordblks = u64::try_from(info.fordblks).ok();
        counters.hblkhd = u64::try_from(info.hblkhd).ok();
    }
    counters
}
