use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock, Weak};

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
    open: Arc<Mutex<Opened>>,
    session_readers: Mutex<SessionReaders>,
    sessions: Mutex<usize>,
    opens: AtomicUsize,
    closed: AtomicBool,
    memory: Option<(memory_budget::MemoryBudget, usize)>,
    source: Mutex<Option<PathBuf>>,
    #[cfg(test)]
    handoff: Mutex<Option<Arc<TestHandoff>>>,
}

#[derive(Default)]
struct Opened {
    console: Option<Arc<query_console::Console>>,
    error: Option<String>,
    retired: Vec<RetiredFile>,
}

#[derive(Default)]
struct SessionReaders {
    slots: Vec<Weak<Mutex<Opened>>>,
    files: Vec<RetiredFile>,
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
            open: Arc::new(Mutex::new(Opened::default())),
            session_readers: Mutex::new(SessionReaders::default()),
            sessions: Mutex::new(0),
            opens: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            memory: None,
            source: Mutex::new(None),
            #[cfg(test)]
            handoff: Mutex::new(None),
        }
    }

    pub(crate) fn with_memory(budget: memory_budget::MemoryBudget, cache: usize) -> Self {
        Self {
            memory: Some((budget, cache)),
            ..Self::new()
        }
    }

    pub fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.memory.as_ref().map(|(budget, _)| budget)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.jobs.close();
        let detached = match self.open.try_lock() {
            Ok(mut opened) => Self::detach(&mut opened),
            Err(std::sync::TryLockError::Poisoned(error)) => Self::detach(&mut error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => (None, Vec::new()),
        };
        self.hand_over(detached);
    }

    fn detach(opened: &mut Opened) -> (Option<Arc<query_console::Console>>, Vec<RetiredFile>) {
        opened.error = None;
        (opened.console.take(), std::mem::take(&mut opened.retired))
    }

    fn hand_over(
        &self,
        (console, mut retired): (Option<Arc<query_console::Console>>, Vec<RetiredFile>),
    ) {
        if let Some(console) = &console {
            retired.push(RetiredFile::new(console.file_close_observer()));
            console.close();
        }
        retired.retain(RetiredFile::is_open);
        if retired.is_empty() || self.memory.is_none() {
            return;
        }
        if let Some(source) = lock(&self.source).clone() {
            let destination = slot(&source);
            let mut opened = lock(&destination.open);
            opened.retired.retain(RetiredFile::is_open);
            opened.retired.append(&mut retired);
        }
    }

    fn track_session_reader(&self, source: &Path, console: &query_console::Console) {
        let destination = slot(source);
        let mut readers = lock(&destination.session_readers);
        readers.slots.retain(|slot| slot.strong_count() != 0);
        let slot = Arc::downgrade(&self.open);
        if !readers
            .slots
            .iter()
            .any(|registered| registered.ptr_eq(&slot))
        {
            readers.slots.push(slot);
        }
        readers.files.retain(RetiredFile::is_open);
        readers
            .files
            .push(RetiredFile::new(console.file_close_observer()));
    }

    fn release_session_readers(&self) -> bool {
        let slots: Vec<_> = lock(&self.session_readers)
            .slots
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for slot in slots {
            if !Self::release_opened(&mut lock(&slot)) {
                return false;
            }
        }
        let mut readers = lock(&self.session_readers);
        readers.slots.retain(|slot| slot.strong_count() != 0);
        readers.files.retain(RetiredFile::is_open);
        readers.files.is_empty()
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
        let result = {
            let mut o = lock(&self.open);
            o.retired.retain(RetiredFile::is_open);
            if let Some(c) = &o.console {
                let ready = Availability::Ready(Arc::clone(c));
                #[cfg(test)]
                self.pause_handoff("ready-locked");
                ready
            } else if let Some(e) = &o.error {
                Availability::Failed(e.clone())
            } else {
                self.open_into(&mut o, path)
            }
        };
        self.after_open(result)
    }

    fn after_open(&self, result: Availability) -> Availability {
        #[cfg(test)]
        self.pause_handoff("after-lock");
        if self.closed.load(Ordering::Acquire) {
            let detached = Self::detach(&mut lock(&self.open));
            self.hand_over(detached);
            if let Availability::Ready(console) = result {
                self.hand_over((Some(console), Vec::new()));
            }
            Availability::Failed("opening cancelled".into())
        } else {
            result
        }
    }

    pub fn reopen(&self, path: &Path) -> Availability {
        let result = {
            let mut o = lock(&self.open);
            Self::retire_open(&mut o);
            self.open_into(&mut o, path)
        };
        self.after_open(result)
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
        Self::release_opened(&mut lock(&self.open))
    }

    fn release_opened(o: &mut Opened) -> bool {
        if Self::active(o) {
            return false;
        }
        Self::retire_open(o);
        o.retired.retain(RetiredFile::is_open);
        o.retired.is_empty()
    }

    fn open_into(&self, o: &mut Opened, path: &Path) -> Availability {
        if self.closed.load(Ordering::Acquire) {
            return Availability::Failed("opening cancelled".into());
        }
        self.opens.fetch_add(1, Ordering::Relaxed);
        if self.memory.is_some() {
            *lock(&self.source) = Some(path.to_path_buf());
        }
        let opened =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &self.memory {
                Some((budget, cache)) => {
                    query_console::Console::open_with_memory(path, budget.clone(), *cache)
                        .map_err(|e| e.to_string())
                }
                None => {
                    query_console::Console::open_profile(path, query_console::LoadProfile::Query)
                }
            }))
            .unwrap_or_else(|_| Err("internal error while opening the base".to_string()));
        match opened {
            Ok(c) => {
                if self.memory.is_some() {
                    self.track_session_reader(path, &c);
                }
                if self.closed.load(Ordering::Acquire) {
                    c.close();
                    return Availability::Failed("opening cancelled".into());
                }
                let c = Arc::new(c);
                o.console = Some(Arc::clone(&c));
                #[cfg(test)]
                self.pause_handoff("installed-locked");
                Availability::Ready(c)
            }
            Err(e) => {
                if self.memory.is_none() {
                    o.error = Some(e.clone());
                }
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

pub fn slot_for(state: &State) -> Arc<ConsoleSlot> {
    state
        .session
        .as_ref()
        .map_or_else(|| slot(&state.source), |session| session.console())
}

pub fn release_idle_readers(state: &State) -> bool {
    let session = state.session.as_ref().map(|session| session.console());
    release_idle(&state.source, session.as_deref())
}

fn release_idle(source: &Path, session: Option<&ConsoleSlot>) -> bool {
    let source_slot = slot(source);
    session.is_none_or(ConsoleSlot::release_idle)
        && source_slot.release_session_readers()
        && source_slot.release_idle()
        && super::code_consoles::code_slot(source).release_idle()
}
