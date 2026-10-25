use super::memory::Context;
use memory_budget::Evictor;
use memory_budget::control::storage::{
    ControlReserve, DiscoverySession, InvalidStorage, Observation,
};
use std::{
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::Duration,
};

const REFRESH_PERIOD: Duration = Duration::from_secs(5);

pub fn observe(
    reserve: &mut ControlReserve,
    deployment: Option<u64>,
) -> Result<Observation, InvalidStorage> {
    let regions = reserve.regions();
    let mut session = DiscoverySession::new(
        regions.history_active,
        regions.history_scratch,
        regions.discovery,
        deployment,
    )?;
    Ok(session.discover())
}

pub fn tick(memory: &Context, observation: &Observation, evictor: &dyn Evictor) {
    let policy = memory.apply(observation);
    let state = memory.snapshot();
    if state.over_budget || policy.pressure || state.categories.iter().any(|c| c.current > c.limit)
    {
        evictor.evict_clean();
    }
}

pub struct CliMemory {
    context: Context,
    reserve: ControlReserve,
}
impl CliMemory {
    pub fn new(context: Context, reserve: ControlReserve) -> Self {
        Self { context, reserve }
    }
    pub fn refresh(&mut self) -> crate::Result<()> {
        let observation = observe(&mut self.reserve, self.context.deployment())
            .map_err(|_| "memory discovery storage is invalid")?;
        self.context.apply(&observation);
        Ok(())
    }
}
impl std::ops::Deref for CliMemory {
    type Target = Context;
    fn deref(&self) -> &Context {
        &self.context
    }
}

pub fn check_session(reserve: &mut ControlReserve, deployment: Option<u64>) -> std::io::Result<()> {
    let regions = reserve.regions();
    DiscoverySession::new(
        regions.history_active,
        regions.history_scratch,
        regions.discovery,
        deployment,
    )
    .map(drop)
    .map_err(|_| std::io::Error::other("memory discovery storage is invalid"))
}

pub struct RefreshOwner {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}
impl RefreshOwner {
    pub fn start(
        memory: Arc<Context>,
        evictor: Arc<dyn Evictor + Send>,
        mut reserve: ControlReserve,
    ) -> std::io::Result<Self> {
        check_session(&mut reserve, memory.deployment())?;
        let (stop, stopped) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("yellowcode-memory-refresh".into())
            .spawn(move || {
                let regions = reserve.regions();
                let Ok(mut session) = DiscoverySession::new(
                    regions.history_active,
                    regions.history_scratch,
                    regions.discovery,
                    memory.deployment(),
                ) else {
                    return;
                };
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    stopped.recv_timeout(REFRESH_PERIOD)
                {
                    tick(&memory, &session.discover(), &*evictor);
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for RefreshOwner {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn trim_heap() {
    crate::heap::trim();
}

pub fn start_service<B, R, L>(
    open: impl FnOnce() -> crate::Result<B>,
    trim: impl FnOnce(),
    refresher: impl FnOnce(&B) -> std::io::Result<R>,
    bind: impl FnOnce() -> std::io::Result<L>,
) -> crate::Result<(B, R, L)> {
    let base = open()?;
    trim();
    let refresh = refresher(&base)?;
    let listener = bind()?;
    Ok((base, refresh, listener))
}

#[cfg(all(test, target_os = "linux"))]
mod pressure {
    use super::*;
    use memory_budget::Category;
    use memory_budget::discovery::linux::{
        bounded::Reader,
        bounded_io::{Error, KernelFile, ProcFile},
    };
    use std::{
        cell::Cell,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    struct Fixture {
        limit: Cell<u64>,
        denied: Cell<bool>,
    }
    fn copy(value: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        out.get_mut(..value.len())
            .ok_or(Error::Capacity)?
            .copy_from_slice(value);
        Ok(value.len())
    }
    impl Reader for Fixture {
        fn proc_file(&self, file: ProcFile, out: &mut [u8]) -> Result<usize, Error> {
            match file {
                ProcFile::Membership => copy(b"0::/leaf\n", out),
                ProcFile::Mountinfo => copy(b"1 0 0:1 / /cg rw - cgroup2 cgroup rw\n", out),
                ProcFile::Meminfo => copy(b"MemTotal: 16777216 kB\n", out),
            }
        }
        fn cgroup_file(
            &self,
            _: &Path,
            relative: &Path,
            file: KernelFile,
            out: &mut [u8],
            _: &mut [u8],
        ) -> Result<usize, Error> {
            if self.denied.get() {
                return Err(Error::Io(Some(libc::EACCES)));
            }
            if relative.as_os_str().is_empty() {
                return Err(Error::Io(Some(libc::ENOENT)));
            }
            match file {
                KernelFile::MemoryMax => copy(format!("{}\n", self.limit.get()).as_bytes(), out),
                KernelFile::MemoryHigh => copy(b"max\n", out),
                _ => Err(Error::Io(Some(libc::ENOENT))),
            }
        }
    }
    struct Count(AtomicUsize);
    impl Evictor for Count {
        fn evict_clean(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn fake_ancestor_shrink_2g_512m_min_0_restore() {
        let f = Fixture {
            limit: Cell::new(2 * GIB),
            denied: Cell::new(false),
        };
        let mut reserve = ControlReserve::try_new().unwrap();
        let regions = reserve.regions();
        let mut session = DiscoverySession::new(
            regions.history_active,
            regions.history_scratch,
            regions.discovery,
            None,
        )
        .unwrap();
        let raw = super::super::memory::tests::raw(None, None);
        let memory = Context::new(&raw, &session.discover_with(&f)).unwrap();
        let startup = memory.policy().budget;
        assert_eq!(startup, 1433 * MIB);
        let startup_cache = memory.cache_bytes().unwrap();
        assert!(memory.admission().is_none());
        let evictor = Count(AtomicUsize::new(0));
        let lease = memory
            .ledger()
            .try_reserve(Category::Execution, 400 * MIB)
            .unwrap();
        f.limit.set(512 * MIB);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert!(memory.ledger().snapshot().over_budget);
        assert!(memory.ledger().try_reserve(Category::Input, 1).is_err());
        assert!(memory.admission().is_none());
        assert!(evictor.0.load(Ordering::Relaxed) > 0);
        assert!(memory.cache_bytes().unwrap() < startup_cache);
        f.limit.set(128 * MIB);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert_eq!(
            memory.admission(),
            Some(super::super::memory::Refusal::PRESSURE)
        );
        f.limit.set(0);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert_eq!(memory.ledger().snapshot().admission, 0);
        assert_eq!(lease.bytes(), 400 * MIB);
        f.denied.set(true);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert!(memory.pressured());
        assert_eq!(memory.ledger().snapshot().admission, 0);
        drop(lease);
        f.denied.set(false);
        f.limit.set(2 * GIB);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert!(memory.admission().is_none());
        assert_eq!(
            memory.ledger().snapshot().admission,
            memory.policy().admission
        );
        f.limit.set(8 * GIB);
        tick(&memory, &session.discover_with(&f), &evictor);
        assert_eq!(
            memory.ledger().snapshot().admission,
            memory.policy().admission
        );
        let small = memory.ledger().try_reserve(Category::Input, 1024).unwrap();
        drop(small);
        assert!(!memory.ledger().snapshot().over_budget);
    }
}
