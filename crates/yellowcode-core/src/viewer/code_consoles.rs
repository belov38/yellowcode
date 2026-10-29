use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use code_console::{BUSY, CodeConsole, CodeJobs};

use super::consoles::{RetiredFile, lock};

pub struct CodeSlot {
    pub jobs: CodeJobs,
    open: Mutex<Opened>,
    opens: AtomicUsize,
}

#[derive(Default)]
struct Opened {
    console: Option<Arc<CodeConsole>>,
    error: Option<String>,
    retired: Vec<RetiredFile>,
}

pub enum CodeAvailability {
    Ready(Arc<CodeConsole>),
    Failed(String),
}

pub enum StartError {
    NotOpen(String),
    Refused(String),
}

impl CodeSlot {
    fn new() -> CodeSlot {
        CodeSlot {
            jobs: CodeJobs::new(),
            open: Mutex::new(Opened::default()),
            opens: AtomicUsize::new(0),
        }
    }

    #[doc(hidden)]
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::Relaxed)
    }

    pub fn get(&self, path: &Path) -> CodeAvailability {
        let mut o = lock(&self.open);
        o.retired.retain(RetiredFile::is_open);
        if let Some(c) = &o.console {
            return CodeAvailability::Ready(Arc::clone(c));
        }
        if let Some(e) = &o.error {
            return CodeAvailability::Failed(e.clone());
        }
        self.open_into(&mut o, path)
    }

    pub fn reopen(&self, path: &Path) -> Result<CodeAvailability, String> {
        let mut o = lock(&self.open);
        if self.jobs.any_running()
            || o.console.as_ref().is_some_and(|c| c.is_busy())
            || o.retired.iter().any(RetiredFile::is_open)
        {
            return Err(BUSY.to_string());
        }
        Self::retire_open(&mut o);
        Ok(self.open_into(&mut o, path))
    }

    pub fn start(&self, path: &Path, text: String) -> Result<u64, StartError> {
        let mut o = lock(&self.open);
        o.retired.retain(RetiredFile::is_open);
        if !o.retired.is_empty() {
            return Err(StartError::Refused(BUSY.to_string()));
        }
        let console = match &o.console {
            Some(c) => Arc::clone(c),
            None => match &o.error {
                Some(e) => return Err(StartError::NotOpen(e.clone())),
                None => match self.open_into(&mut o, path) {
                    CodeAvailability::Ready(c) => c,
                    CodeAvailability::Failed(e) => return Err(StartError::NotOpen(e)),
                },
            },
        };
        self.jobs.start(console, text).map_err(StartError::Refused)
    }

    pub fn release_idle(&self) -> bool {
        let mut o = lock(&self.open);
        if self.active(&o) {
            return false;
        }
        Self::retire_open(&mut o);
        o.retired.retain(RetiredFile::is_open);
        o.retired.is_empty()
    }

    fn active(&self, o: &Opened) -> bool {
        self.jobs.any_running()
            || o.retired.iter().any(RetiredFile::is_open)
            || o.console
                .as_ref()
                .is_some_and(|c| Arc::strong_count(c) != 1 || c.is_busy())
    }

    pub fn retire(&self) {
        let mut o = lock(&self.open);
        Self::retire_open(&mut o);
    }

    fn retire_open(o: &mut Opened) {
        o.retired.retain(RetiredFile::is_open);
        if let Some(console) = o.console.take() {
            o.retired
                .push(RetiredFile::new(console.file_close_observer()));
        }
        o.error = None;
    }

    fn open_into(&self, o: &mut Opened, path: &Path) -> CodeAvailability {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let opened =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| CodeConsole::open(path)))
                .unwrap_or_else(|_| Err("internal error while opening the base".to_string()));
        match opened {
            Ok(c) => {
                let c = Arc::new(c);
                o.console = Some(Arc::clone(&c));
                CodeAvailability::Ready(c)
            }
            Err(e) => {
                o.error = Some(e.clone());
                CodeAvailability::Failed(e)
            }
        }
    }
}

type Table = Mutex<HashMap<PathBuf, Arc<CodeSlot>>>;

pub fn code_slot(source: &Path) -> Arc<CodeSlot> {
    static TABLE: OnceLock<Table> = OnceLock::new();
    let t = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
    Arc::clone(
        lock(t)
            .entry(super::consoles::source_key(source))
            .or_insert_with(|| Arc::new(CodeSlot::new())),
    )
}
