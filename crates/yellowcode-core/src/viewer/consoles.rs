use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

use super::state::State;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub enum NotBase {
    Cf,
    Dt,
    XmlDir,
    Unknown(String),
}

pub fn infobase_source(state: &State) -> Result<(), NotBase> {
    use crate::viewer::source::{self, SourceKind};
    match source::detect(&state.source) {
        Ok(SourceKind::Infobase) => Ok(()),
        Ok(SourceKind::Cf) => Err(NotBase::Cf),
        Ok(SourceKind::Dt) => Err(NotBase::Dt),
        Ok(SourceKind::XmlDir) => Err(NotBase::XmlDir),
        Err(e) => Err(NotBase::Unknown(e.to_string())),
    }
}

pub struct ConsoleSlot {
    pub admission: Arc<RwLock<()>>,
    pub jobs: query_console::Jobs,
    open: Mutex<Opened>,
    sessions: Mutex<usize>,
    opens: AtomicUsize,
}

#[derive(Default)]
struct Opened {
    console: Option<Arc<query_console::Console>>,
    error: Option<String>,
    retired: Vec<RetiredFile>,
}

pub(super) struct RetiredFile(Option<onecd::FileCloseObserver>);

impl RetiredFile {
    pub(super) fn new(observer: Option<onecd::FileCloseObserver>) -> Self {
        Self(observer)
    }

    pub(super) fn is_open(&self) -> bool {
        !self.0.as_ref().is_some_and(|observer| observer.is_closed())
    }
}

pub enum Availability {
    Ready(Arc<query_console::Console>),
    Failed(String),
}

impl ConsoleSlot {
    fn new() -> ConsoleSlot {
        ConsoleSlot {
            admission: Arc::new(RwLock::new(())),
            jobs: query_console::Jobs::new(),
            open: Mutex::new(Opened::default()),
            sessions: Mutex::new(0),
            opens: AtomicUsize::new(0),
        }
    }

    #[doc(hidden)]
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::Relaxed)
    }

    pub(super) fn register_session(&self) {
        *lock(&self.sessions) += 1;
    }

    pub(super) fn unregister_session(&self, code: &super::code_consoles::CodeSlot) {
        let mut sessions = lock(&self.sessions);
        if *sessions == 0 {
            return;
        }
        *sessions -= 1;
        if *sessions == 0 {
            self.retire();
            code.retire();
        }
    }

    pub fn get(&self, path: &Path) -> Availability {
        let mut o = lock(&self.open);
        o.retired.retain(RetiredFile::is_open);
        if let Some(c) = &o.console {
            return Availability::Ready(Arc::clone(c));
        }
        if let Some(e) = &o.error {
            return Availability::Failed(e.clone());
        }
        self.open_into(&mut o, path)
    }

    pub fn reopen(&self, path: &Path) -> Availability {
        let mut o = lock(&self.open);
        Self::retire_open(&mut o);
        self.open_into(&mut o, path)
    }

    fn active(o: &Opened) -> bool {
        o.retired.iter().any(RetiredFile::is_open)
            || o.console
                .as_ref()
                .is_some_and(|c| Arc::strong_count(c) != 1)
    }

    pub fn retire(&self) {
        let mut o = lock(&self.open);
        Self::retire_open(&mut o);
    }

    fn retire_open(o: &mut Opened) {
        o.retired.retain(RetiredFile::is_open);
        if let Some(console) = o.console.take() {
            o.retired.push(RetiredFile(console.file_close_observer()));
        }
        o.error = None;
    }

    pub fn release_idle(&self) -> bool {
        let mut o = lock(&self.open);
        if Self::active(&o) {
            return false;
        }
        Self::retire_open(&mut o);
        o.retired.retain(RetiredFile::is_open);
        o.retired.is_empty()
    }

    fn open_into(&self, o: &mut Opened, path: &Path) -> Availability {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            query_console::Console::open_profile(path, query_console::LoadProfile::Query)
        }))
        .unwrap_or_else(|_| Err("internal error while opening the base".to_string()));
        match opened {
            Ok(c) => {
                let c = Arc::new(c);
                o.console = Some(Arc::clone(&c));
                Availability::Ready(c)
            }
            Err(e) => {
                o.error = Some(e.clone());
                Availability::Failed(e)
            }
        }
    }
}

type Table = Mutex<HashMap<PathBuf, Arc<ConsoleSlot>>>;

pub fn slot(source: &Path) -> Arc<ConsoleSlot> {
    static TABLE: OnceLock<Table> = OnceLock::new();
    let t = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
    Arc::clone(
        lock(t)
            .entry(source_key(source))
            .or_insert_with(|| Arc::new(ConsoleSlot::new())),
    )
}

pub(super) fn source_key(source: &Path) -> PathBuf {
    let path = if source.is_dir() && source.join("1Cv8.1CD").is_file() {
        source.join("1Cv8.1CD")
    } else {
        source.to_path_buf()
    };
    std::fs::canonicalize(&path).unwrap_or(path)
}
