use crate::Console;
use crate::cell::Cell;
use crate::refs::{NestedOut, PresentedCell};
use crate::run::{ErrorOut, Phase, Report, RunRequest};
use serde::Serialize;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

pub const KEEP: usize = 8;
pub const MAX_PAGE: usize = 1000;
pub const MAX_RUNNING: usize = 4;
pub const RETAIN_BUDGET: usize = 512 << 20;
pub const TOO_MANY_RUNNING: &str =
    "слишком много запросов выполняется одновременно; остановите лишние";

pub enum JobState {
    Running,
    Done(Arc<Report>),
    Failed(ErrorOut),
    Cancelled,
}

pub struct Job {
    pub cancel: AtomicBool,
    pub state: Mutex<JobState>,
    started: Instant,
    retained: AtomicUsize,
}

impl Job {
    fn new() -> Job {
        Job {
            cancel: AtomicBool::new(false),
            state: Mutex::new(JobState::Running),
            started: Instant::now(),
            retained: AtomicUsize::new(0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StateName {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Serialize)]
pub struct Page {
    pub rows: Vec<Vec<Cell>>,
    pub present: Vec<Vec<Option<PresentedCell>>>,
    pub present_errors: Vec<String>,
    pub present_queries: usize,
}

#[derive(Serialize)]
pub struct JobStatus {
    pub state: StateName,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_report")]
    pub report: Option<Arc<Report>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorOut>,
    pub elapsed_ms: u64,
}

fn ser_report<S: serde::Serializer>(r: &Option<Arc<Report>>, s: S) -> Result<S::Ok, S::Error> {
    match r {
        Some(r) => r.serialize(s),
        None => s.serialize_none(),
    }
}

struct Inner {
    next: AtomicU64,
    map: Mutex<HashMap<u64, Arc<Job>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn finished(j: &Job) -> bool {
    !matches!(*lock(&j.state), JobState::Running)
}

impl Inner {
    fn evict_with(&self, budget: usize) {
        let mut map = lock(&self.map);
        loop {
            let mut done: Vec<(u64, usize)> = map
                .iter()
                .filter(|(_, j)| finished(j))
                .map(|(id, j)| (*id, j.retained.load(Ordering::Relaxed)))
                .collect();
            let total: usize = done.iter().map(|(_, b)| *b).sum();
            if done.len() <= 1 || (done.len() <= KEEP && total <= budget) {
                return;
            }
            done.sort_unstable();
            if let Some((oldest, _)) = done.first() {
                map.remove(oldest);
            }
        }
    }

    fn evict(&self) {
        self.evict_with(RETAIN_BUDGET);
    }

    fn admit(&self, max_running: usize) -> Result<(u64, Arc<Job>), String> {
        let mut map = lock(&self.map);
        let running = map.values().filter(|j| !finished(j)).count();
        if running >= max_running {
            return Err(TOO_MANY_RUNNING.to_string());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let job = Arc::new(Job::new());
        map.insert(id, Arc::clone(&job));
        Ok((id, job))
    }
}

#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs {
            inner: Arc::new(Inner {
                next: AtomicU64::new(1),
                map: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn start(&self, console: Arc<Console>, req: RunRequest) -> Result<u64, String> {
        let (id, job) = self.inner.admit(MAX_RUNNING)?;
        let inner = Arc::clone(&self.inner);
        let j = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name("x1-query".to_string())
            .spawn(move || {
                let out = catch_unwind(AssertUnwindSafe(|| console.run(&req, &j.cancel)));
                let state = match out {
                    Ok(r) if r.cancelled => JobState::Cancelled,
                    Ok(r) => {
                        j.retained.store(r.rows.approx_bytes(), Ordering::Relaxed);
                        JobState::Done(Arc::new(r))
                    }
                    Err(_) => JobState::Failed(ErrorOut::new(
                        Phase::Runtime,
                        "internal: the query thread panicked",
                    )),
                };
                *lock(&j.state) = state;
                inner.evict();
            });
        if let Err(e) = spawned {
            lock(&self.inner.map).remove(&id);
            return Err(format!("cannot start a thread: {e}"));
        }
        self.inner.evict();
        Ok(id)
    }

    fn get(&self, id: u64) -> Option<Arc<Job>> {
        lock(&self.inner.map).get(&id).cloned()
    }

    pub fn status(&self, id: u64) -> Option<JobStatus> {
        let job = self.get(id)?;
        let st = lock(&job.state);
        let (state, report, error) = match &*st {
            JobState::Running => (StateName::Running, None, None),
            JobState::Done(r) => (StateName::Done, Some(Arc::clone(r)), None),
            JobState::Failed(e) => (StateName::Failed, None, Some(e.clone())),
            JobState::Cancelled => (StateName::Cancelled, None, None),
        };
        let elapsed_ms = match &report {
            Some(r) => r.elapsed_ms,
            None => u64::try_from(job.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        Some(JobStatus {
            state,
            report,
            error,
            elapsed_ms,
        })
    }

    pub fn cancel(&self, id: u64) -> bool {
        match self.get(id) {
            Some(j) => {
                if !finished(&j) {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                true
            }
            None => false,
        }
    }

    pub fn rows(
        &self,
        id: u64,
        stmt: usize,
        from: usize,
        limit: usize,
    ) -> Result<Vec<Vec<Cell>>, String> {
        let job = self.get(id).ok_or("no such job")?;
        let st = lock(&job.state);
        match &*st {
            JobState::Done(r) => {
                if stmt >= r.rows.stmt_count() {
                    return Err("no such statement".to_string());
                }
                Ok(r.rows.slice(stmt, from, limit.min(MAX_PAGE)).to_vec())
            }
            JobState::Running => Err("the job is still running".to_string()),
            JobState::Cancelled => Err("the job was cancelled".to_string()),
            JobState::Failed(_) => Err("the job failed".to_string()),
        }
    }

    pub fn drop_job(&self, id: u64) -> bool {
        let job = lock(&self.inner.map).remove(&id);
        match job {
            Some(j) => {
                j.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn page(
        &self,
        console: &Console,
        id: u64,
        stmt: usize,
        from: usize,
        limit: usize,
    ) -> Result<Page, String> {
        let rows = self.rows(id, stmt, from, limit)?;
        let p = console.present(&rows, &AtomicBool::new(false));
        Ok(Page {
            rows,
            present: p.texts,
            present_errors: p.errors,
            present_queries: p.queries,
        })
    }

    pub fn nested(
        &self,
        console: &Console,
        id: u64,
        stmt: usize,
        row: usize,
        col: usize,
    ) -> Result<NestedOut, ErrorOut> {
        let bad = |t: &str| ErrorOut::new(Phase::Runtime, t);
        let job = self.get(id).ok_or_else(|| bad("no such job"))?;
        let report = match &*lock(&job.state) {
            JobState::Done(r) => Arc::clone(r),
            _ => return Err(bad("the job has no result")),
        };
        console.nested(&report, stmt, row, col)
    }
}
