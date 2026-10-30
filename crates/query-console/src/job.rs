use crate::Console;
use crate::cell::Cell;
use crate::refs::{Presented, PresentedCell};
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

pub struct Page {
    pub rows: JobRows,
    presentation: Presented,
    pub present_queries: usize,
}
impl Page {
    pub fn present(&self) -> &[Vec<Option<PresentedCell>>] {
        self.presentation.texts()
    }
    pub fn present_errors(&self) -> &[String] {
        self.presentation.errors()
    }
    pub fn into_presented(self) -> Presented {
        self.presentation
    }
}
impl Serialize for Page {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("Page", 4)?;
        out.serialize_field("rows", &self.rows)?;
        out.serialize_field("present", self.present())?;
        out.serialize_field("present_errors", self.present_errors())?;
        out.serialize_field("present_queries", &self.present_queries)?;
        out.end()
    }
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

#[derive(Clone, Debug)]
pub struct JobRows {
    report: Arc<Report>,
    stmt: usize,
    from: usize,
    limit: usize,
}

impl std::ops::Deref for JobRows {
    type Target = [Vec<Cell>];
    fn deref(&self) -> &Self::Target {
        self.report.rows.slice(self.stmt, self.from, self.limit)
    }
}
impl Serialize for JobRows {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref_rows().serialize(serializer)
    }
}
impl JobRows {
    fn deref_rows(&self) -> &[Vec<Cell>] {
        self.report.rows.slice(self.stmt, self.from, self.limit)
    }
}

#[derive(Clone, Debug)]
pub struct JobNested {
    report: Arc<Report>,
    stmt: usize,
    row: usize,
    col: usize,
}
impl Serialize for JobNested {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.report
            .rows
            .nested(self.stmt, self.row, self.col)
            .serialize(serializer)
    }
}
impl JobNested {
    pub fn columns(&self) -> &[crate::Column] {
        self.report
            .rows
            .nested(self.stmt, self.row, self.col)
            .map_or(&[], |n| &n.columns)
    }
    pub fn rows(&self) -> &[Vec<Cell>] {
        self.report
            .rows
            .nested(self.stmt, self.row, self.col)
            .map_or(&[], |n| &n.rows)
    }
}

#[derive(Default)]
struct Workers {
    closed: bool,
    jobs: HashMap<u64, Arc<Job>>,
    consoles: Vec<std::sync::Weak<crate::lifecycle::Lifecycle>>,
}

struct Inner {
    #[cfg(test)]
    fail_next_spawn: AtomicBool,
    workers: Mutex<Workers>,
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
            let total = done
                .iter()
                .fold(0usize, |total, (_, bytes)| total.saturating_add(*bytes));
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
        let mut workers = lock(&self.workers);
        if workers.closed {
            return Err("query session is closed".to_string());
        }
        if workers.jobs.len() >= max_running {
            return Err(TOO_MANY_RUNNING.to_string());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let job = Arc::new(Job::new());
        workers.jobs.insert(id, Arc::clone(&job));
        lock(&self.map).insert(id, Arc::clone(&job));
        Ok((id, job))
    }

    fn worker_finished(&self, id: u64) {
        lock(&self.workers).jobs.remove(&id);
    }

    fn close(&self) {
        let mut workers = lock(&self.workers);
        workers.closed = true;
        for lifecycle in workers.consoles.iter().filter_map(std::sync::Weak::upgrade) {
            lifecycle.cancel(true);
        }
        for job in workers.jobs.values() {
            job.cancel.store(true, Ordering::Release);
        }
        lock(&self.map).clear();
    }
}

struct WorkerGuard {
    inner: Arc<Inner>,
    id: u64,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.inner.worker_finished(self.id);
    }
}

struct OwnerGuard(Arc<Inner>);
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Inner>,
    _owner: Arc<OwnerGuard>,
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

impl Jobs {
    pub fn new() -> Jobs {
        let inner = Arc::new(Inner {
            #[cfg(test)]
            fail_next_spawn: AtomicBool::new(false),
            workers: Mutex::new(Workers::default()),
            next: AtomicU64::new(1),
            map: Mutex::new(HashMap::new()),
        });
        Jobs {
            _owner: Arc::new(OwnerGuard(Arc::clone(&inner))),
            inner,
        }
    }

    pub fn start(&self, console: Arc<Console>, req: RunRequest) -> Result<u64, String> {
        self.register_console(&console)?;
        let (id, job) = self.inner.admit(MAX_RUNNING)?;
        let inner = Arc::clone(&self.inner);
        let j = Arc::clone(&job);
        let worker = WorkerGuard {
            inner: Arc::clone(&inner),
            id,
        };
        let work = move || {
            let _worker = worker;
            let out = catch_unwind(AssertUnwindSafe(|| console.run_budgeted(&req, &j.cancel)));
            let state = match out {
                Ok(Ok(r)) if r.cancelled => JobState::Cancelled,
                Ok(Ok(r)) => {
                    let retained = r
                        .retained_bytes()
                        .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX))
                        .unwrap_or_else(|| r.rows.approx_bytes());
                    j.retained.store(retained, Ordering::Relaxed);
                    JobState::Done(Arc::new(r))
                }
                Ok(Err(error)) => {
                    JobState::Failed(ErrorOut::new(Phase::Runtime, error.to_string()))
                }
                Err(_) => JobState::Failed(ErrorOut::new(
                    Phase::Runtime,
                    "internal: the query thread panicked",
                )),
            };
            *lock(&j.state) = state;
            inner.evict();
        };
        #[cfg(not(test))]
        let spawned = std::thread::Builder::new()
            .name("x1-query".into())
            .spawn(work);
        if let Err(e) = spawned {
            lock(&self.inner.map).remove(&id);
            return Err(format!("cannot start a thread: {e}"));
        }
        self.inner.evict();
        Ok(id)
    }

    pub fn cancel_all(&self) {
        let workers = lock(&self.inner.workers);
        for lifecycle in workers.consoles.iter().filter_map(std::sync::Weak::upgrade) {
            lifecycle.cancel(false);
        }
        for job in workers.jobs.values() {
            job.cancel.store(true, Ordering::Release);
        }
    }

    pub fn close(&self) {
        self.inner.close();
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

    pub fn rows(&self, id: u64, stmt: usize, from: usize, limit: usize) -> Result<JobRows, String> {
        let job = self.get(id).ok_or("no such job")?;
        let st = lock(&job.state);
        match &*st {
            JobState::Done(r) => {
                if stmt >= r.rows.stmt_count() {
                    return Err("no such statement".to_string());
                }
                Ok(JobRows {
                    report: Arc::clone(r),
                    stmt,
                    from,
                    limit: limit.min(MAX_PAGE),
                })
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
        self.page_with_cancel(console, id, stmt, from, limit, &AtomicBool::new(false))
    }

    pub fn page_with_cancel(
        &self,
        console: &Console,
        id: u64,
        stmt: usize,
        from: usize,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Page, String> {
        self.register_console(console)?;
        let rows = self.rows(id, stmt, from, limit)?;
        let presentation = console.present(&rows, cancel);
        Ok(Page {
            rows,
            present_queries: presentation.queries,
            presentation,
        })
    }

    fn register_console(&self, console: &Console) -> Result<(), String> {
        let mut workers = lock(&self.inner.workers);
        if workers.closed {
            return Err("query session is closed".into());
        }
        workers.consoles.retain(|c| c.strong_count() > 0);
        let weak = Arc::downgrade(&console.lifecycle);
        if !workers.consoles.iter().any(|c| c.ptr_eq(&weak)) {
            workers.consoles.push(weak);
        }
        Ok(())
    }

    pub fn nested(
        &self,
        console: &Console,
        id: u64,
        stmt: usize,
        row: usize,
        col: usize,
    ) -> Result<JobNested, ErrorOut> {
        let bad = |t: &str| ErrorOut::new(Phase::Runtime, t);
        let job = self.get(id).ok_or_else(|| bad("no such job"))?;
        let report = match &*lock(&job.state) {
            JobState::Done(r) => Arc::clone(r),
            _ => return Err(bad("the job has no result")),
        };
        console.nested(&report, stmt, row, col)?;
        Ok(JobNested {
            report,
            stmt,
            row,
            col,
        })
    }
}
