use crate::console::{BUSY, CodeConsole, INTERNAL};
use crate::report::{Outcome, RunReport};
use crate::{lock, millis};
use serde::Serialize;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const KEEP: usize = 8;
pub const MAX_TEXT_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeJobState {
    Running,
    Done,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeJobStatus {
    pub state: CodeJobState,
    pub report: Option<RunReport>,
    pub elapsed_ms: u64,
}

struct Job {
    console: usize,
    cancel: Arc<AtomicBool>,
    started: Instant,
    report: Mutex<Option<RunReport>>,
}

impl Job {
    fn running(&self) -> bool {
        lock(&self.report).is_none()
    }
}

struct Inner {
    next: AtomicU64,
    map: Mutex<HashMap<u64, Arc<Job>>>,
}

impl Inner {
    fn admit(&self, console: usize) -> Result<(u64, Arc<Job>), String> {
        let mut map = lock(&self.map);
        if map.values().any(|j| j.console == console && j.running()) {
            return Err(BUSY.to_string());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let job = Arc::new(Job {
            console,
            cancel: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            report: Mutex::new(None),
        });
        map.insert(id, Arc::clone(&job));
        Ok((id, job))
    }

    fn evict(&self) {
        let mut map = lock(&self.map);
        let mut done: Vec<u64> = map
            .iter()
            .filter(|(_, j)| !j.running())
            .map(|(id, _)| *id)
            .collect();
        if done.len() <= KEEP {
            return;
        }
        done.sort_unstable();
        let over = done.len() - KEEP;
        for id in done.into_iter().take(over) {
            map.remove(&id);
        }
    }
}

#[derive(Clone)]
pub struct CodeJobs {
    inner: Arc<Inner>,
}

impl Default for CodeJobs {
    fn default() -> CodeJobs {
        CodeJobs::new()
    }
}

impl CodeJobs {
    pub fn new() -> CodeJobs {
        CodeJobs {
            inner: Arc::new(Inner {
                next: AtomicU64::new(1),
                map: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn start(&self, console: Arc<CodeConsole>, text: String) -> Result<u64, String> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(format!(
                "текст фрагмента длиннее {} МиБ",
                MAX_TEXT_BYTES >> 20
            ));
        }
        let (id, job) = self.inner.admit(Arc::as_ptr(&console).addr())?;
        let inner = Arc::clone(&self.inner);
        let j = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name("x4-code-job".to_string())
            .spawn(move || {
                let report =
                    catch_unwind(AssertUnwindSafe(|| console.run(&text, j.cancel.clone())))
                        .unwrap_or_else(|_| RunReport {
                            elapsed_ms: millis(j.started),
                            ..RunReport::bare(Outcome::Internal, INTERNAL)
                        });
                *lock(&j.report) = Some(report);
                drop(console);
                inner.evict();
            });
        if let Err(e) = spawned {
            lock(&self.inner.map).remove(&id);
            return Err(format!("cannot start a thread: {e}"));
        }
        Ok(id)
    }

    pub fn any_running(&self) -> bool {
        lock(&self.inner.map).values().any(|j| j.running())
    }

    fn get(&self, id: u64) -> Option<Arc<Job>> {
        lock(&self.inner.map).get(&id).cloned()
    }

    pub fn status(&self, id: u64) -> Option<CodeJobStatus> {
        let job = self.get(id)?;
        let report = lock(&job.report).clone();
        Some(match report {
            Some(r) => CodeJobStatus {
                state: CodeJobState::Done,
                elapsed_ms: r.elapsed_ms,
                report: Some(r),
            },
            None => CodeJobStatus {
                state: CodeJobState::Running,
                report: None,
                elapsed_ms: millis(job.started),
            },
        })
    }

    pub fn cancel(&self, id: u64) -> bool {
        match self.get(id) {
            Some(j) => {
                if j.running() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                true
            }
            None => false,
        }
    }
}
