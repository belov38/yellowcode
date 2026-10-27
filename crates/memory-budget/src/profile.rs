use std::cell::RefCell;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Startup,
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
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Startup => "startup",
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

#[must_use]
pub struct PhaseGuard {
    owner: Option<Arc<Shared>>,
    previous: Phase,
    _thread: PhantomData<Rc<()>>,
}
pub fn phase(next: Phase) -> PhaseGuard {
    let owner = OWNER.with(|owner| owner.borrow().clone());
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
        _thread: PhantomData,
    }
}
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Some(owner) = &self.owner {
            owner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .transition(self.previous);
        }
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
        Ok(Some(Self {
            shared,
            worker: Some(worker),
            _thread: PhantomData,
        }))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
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
