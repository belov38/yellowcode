use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use code_console::{BUSY, CodeConsole, CodeJobs};

use super::consoles::lock;

pub struct CodeSlot {
    pub jobs: CodeJobs,
    open: Mutex<Opened>,
    opens: AtomicUsize,
}

#[derive(Default)]
struct Opened {
    console: Option<Arc<CodeConsole>>,
    error: Option<String>,
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
        if self.jobs.any_running() || o.console.as_ref().is_some_and(|c| c.is_busy()) {
            return Err(BUSY.to_string());
        }
        *o = Opened::default();
        Ok(self.open_into(&mut o, path))
    }

    pub fn start(&self, path: &Path, text: String) -> Result<u64, StartError> {
        let mut o = lock(&self.open);
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
            .entry(source.to_path_buf())
            .or_insert_with(|| Arc::new(CodeSlot::new())),
    )
}
