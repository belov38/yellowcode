use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

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
    pub jobs: query_console::Jobs,
    open: Mutex<Opened>,
    opens: AtomicUsize,
}

#[derive(Default)]
struct Opened {
    console: Option<Arc<query_console::Console>>,
    error: Option<String>,
}

pub enum Availability {
    Ready(Arc<query_console::Console>),
    Failed(String),
}

impl ConsoleSlot {
    fn new() -> ConsoleSlot {
        ConsoleSlot {
            jobs: query_console::Jobs::new(),
            open: Mutex::new(Opened::default()),
            opens: AtomicUsize::new(0),
        }
    }

    #[doc(hidden)]
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::Relaxed)
    }

    pub fn get(&self, path: &Path) -> Availability {
        let mut o = lock(&self.open);
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
        *o = Opened::default();
        self.open_into(&mut o, path)
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
            .entry(source.to_path_buf())
            .or_insert_with(|| Arc::new(ConsoleSlot::new())),
    )
}
