use super::{
    content::{self, CopyKind, Selection},
    dt,
    stamp::Stamp,
};
use crate::viewer::{memory::Session, state::State};
use infobase::extensions::{Control, Error, FEATURE_BYTES, Inventory, ReadSource, Stop};
use memory_budget::{
    BudgetedVec, BudgetedWriter, Category, Charged, Limits, MemoryBudget, Reservation, Shared,
};
use serde::Serialize;
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, RwLock, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const WIRE: u64 = 1 << 20;
const CONTENT_BYTES: u64 = 80 << 20;
const STACK: u64 = 2 * 1024 * 1024;
const SPAWN_WORK: u64 = 64 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Unsupported,
    Unavailable,
    MemoryLimit,
    SourceChanged,
    SourceClosed,
    Stale,
    Busy,
    Cancelled,
    Deadline,
    WorkLimit,
    Invalid,
    NoContent,
    ContentLayout,
    CorruptContent,
}
impl Code {
    pub fn status(self) -> u16 {
        match self {
            Self::Stale | Self::SourceChanged | Self::SourceClosed => 409,
            Self::Invalid => 400,
            Self::Unsupported | Self::NoContent | Self::ContentLayout | Self::CorruptContent => 422,
            _ => 503,
        }
    }
}
pub(super) fn code(error: Error) -> Code {
    match error {
        Error::Changed => Code::SourceChanged,
        Error::Memory(_) => Code::MemoryLimit,
        Error::Stopped(Stop::Cancelled) => Code::Cancelled,
        Error::Stopped(Stop::Deadline) => Code::Deadline,
        Error::Stopped(_) => Code::WorkLimit,
        Error::Unsupported(_) => Code::Unsupported,
        Error::Invalid(_) | Error::Io(_) => Code::Unavailable,
        Error::Limit(_) => Code::WorkLimit,
    }
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn arc_layout<T>() -> Result<u64, Code> {
    let (layout, _) = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<T>())
        .map_err(|_| Code::MemoryLimit)?;
    u64::try_from(layout.pad_to_align().size()).map_err(|_| Code::MemoryLimit)
}
#[derive(Clone, Copy)]
enum Kind {
    OneCd,
    Dt,
}
struct Parent {
    token: Arc<AtomicBool>,
    _backing: Reservation,
}
pub(in crate::viewer) struct Prepared {
    kind: Kind,
    stamp: Stamp,
    budget: MemoryBudget,
    content_budget: Option<MemoryBudget>,
    cache: usize,
    wire: u64,
    parent: Option<Parent>,
}
pub(in crate::viewer) fn prepare(
    path: &Path,
    session: Option<&Arc<Session>>,
) -> Result<Prepared, Code> {
    static LEGACY: OnceLock<Result<MemoryBudget, Code>> = OnceLock::new();
    static CONTENT_LEGACY: OnceLock<Result<MemoryBudget, memory_budget::BudgetError>> =
        OnceLock::new();
    let budget = match session {
        Some(s) => s
            .budget()
            .try_scope(Category::Metadata, FEATURE_BYTES)
            .map_err(|_| Code::MemoryLimit)?,
        None => LEGACY
            .get_or_init(|| {
                MemoryBudget::try_new(Limits::new(FEATURE_BYTES)).map_err(|_| Code::MemoryLimit)
            })
            .as_ref()
            .map_err(|c| *c)?
            .clone(),
    };
    #[cfg(test)]
    PREPARATIONS.with(|events| {
        let (calls, legacy_roots) = events.get();
        events.set((
            calls + 1,
            legacy_roots + u64::from(budget.scope_snapshot().is_none()),
        ));
    });
    let parent = session
        .map(|s| {
            let backing = budget
                .try_reserve(Category::Metadata, arc_layout::<AtomicBool>()?)
                .map_err(|_| Code::MemoryLimit)?;
            Ok::<_, Code>(Parent {
                token: s.token().clone(),
                _backing: backing,
            })
        })
        .transpose()?;
    let _path = infobase::extensions::path_open_scratch(&budget, path).map_err(code)?;
    let mut file = File::open(path).map_err(|_| Code::Unsupported)?;
    let stamp = Stamp::file(&file).map_err(code)?;
    let mut head = [0; 8];
    file.read_exact(&mut head).map_err(|_| Code::Unsupported)?;
    let kind = if &head == b"1CDBMSV8" {
        Kind::OneCd
    } else if &head == b"1CIBDmpF" || &head == b"EVIBDmpF" {
        Kind::Dt
    } else {
        return Err(Code::Unsupported);
    };
    let content_budget = match session {
        Some(session) => session
            .budget()
            .try_scope(Category::Metadata, CONTENT_BYTES)
            .ok(),
        None => CONTENT_LEGACY
            .get_or_init(|| MemoryBudget::try_new(Limits::new(CONTENT_BYTES)))
            .as_ref()
            .ok()
            .cloned(),
    };
    Ok(Prepared {
        kind,
        stamp,
        budget,
        content_budget,
        parent,
        cache: session.map_or(8 << 20, |s| s.cache_bytes().min(8 << 20)),
        wire: session.map_or(WIRE, |s| s.response_bytes().min(WIRE)),
    })
}
#[derive(Clone)]
pub(crate) struct Availability {
    owner: Option<Shared<Controller>>,
    reason: Code,
}
impl Availability {
    pub(in crate::viewer) fn unavailable(reason: Code) -> Self {
        Self {
            owner: None,
            reason,
        }
    }
    pub(in crate::viewer) fn bind(
        path: &Path,
        before: Result<Prepared, Code>,
        fingerprint: Option<&crate::viewer::source::Fingerprint>,
        admission: Arc<RwLock<()>>,
    ) -> Self {
        let result = (|| {
            let before = before?;
            if Stamp::capture(path, &before.budget).map_err(code)? != before.stamp {
                return Err(Code::SourceChanged);
            }
            let budget = before.budget;
            let id = next_id()?;
            let charge = budget
                .try_reserve(Category::Metadata, arc_layout::<RwLock<()>>()?)
                .map_err(|_| Code::MemoryLimit)?;
            let owner = Shared::try_new(
                Controller {
                    jobs: Mutex::new(Jobs {
                        generation: 0,
                        inventory_token: None,
                        preparing: false,
                        pending: None,
                        closed: false,
                        changed: false,
                        job: None,
                    }),
                    budget,
                    content_budget: before.content_budget,
                    parent: before.parent,
                    stamp: before.stamp,
                    kind: before.kind,
                    id,
                    admission,
                    cache: before.cache,
                    wire: before.wire,
                    _config_fingerprint: match fingerprint {
                        Some(crate::viewer::source::Fingerprint::Config(fp)) => Some(*fp),
                        _ => None,
                    },
                },
                charge,
            )
            .map_err(|_| Code::MemoryLimit)?;
            drop(lock(&owner.jobs));
            Ok(owner)
        })();
        match result {
            Ok(owner) => Self {
                owner: Some(owner),
                reason: Code::Unavailable,
            },
            Err(reason) => Self::unavailable(reason),
        }
    }
    pub(in crate::viewer) fn signal(&self) {
        if let Some(owner) = &self.owner {
            owner.close(false);
        }
    }
    fn owner(&self) -> Result<&Shared<Controller>, Code> {
        self.owner.as_ref().ok_or(self.reason)
    }
}
static IDS: AtomicU64 = AtomicU64::new(0);
fn next_id() -> Result<u64, Code> {
    let mut old = IDS.load(Ordering::Relaxed);
    loop {
        let next = old.checked_add(1).ok_or(Code::Unavailable)?;
        match IDS.compare_exchange_weak(old, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(next),
            Err(now) => old = now,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Token([u8; 34]);
impl Token {
    fn new(id: u64, generation: u64) -> Self {
        let mut bytes = [b'0'; 34];
        bytes[0] = b'e';
        bytes[17] = b'-';
        for (start, value) in [(1, id), (18, generation)] {
            for (i, byte) in bytes.iter_mut().skip(start).take(16).enumerate() {
                let digit = ((value >> (4 * (15 - i))) & 15) as u8;
                *byte = if digit < 10 {
                    b'0' + digit
                } else {
                    b'a' + digit - 10
                };
            }
        }
        Self(bytes)
    }
    fn matches(&self, raw: &str) -> bool {
        self.0.as_slice() == raw.as_bytes()
    }
}
impl Serialize for Token {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(std::str::from_utf8(&self.0).map_err(serde::ser::Error::custom)?)
    }
}
#[cfg(test)]
thread_local! {
    static PREPARATIONS:std::cell::Cell<(u64,u64)>=const {std::cell::Cell::new((0,0))};
    static BEFORE_SPAWN:std::cell::Cell<Option<fn(&Controller)>>=const {std::cell::Cell::new(None)};
    static WORKER_HOOK:std::cell::Cell<Option<fn()>>=const {std::cell::Cell::new(None)};
    static SPAWN_DISPATCHES:std::cell::Cell<u64>=const {std::cell::Cell::new(0)};
}
struct Controller {
    jobs: Mutex<Jobs>,
    budget: MemoryBudget,
    content_budget: Option<MemoryBudget>,
    parent: Option<Parent>,
    stamp: Stamp,
    kind: Kind,
    id: u64,
    admission: Arc<RwLock<()>>,
    cache: usize,
    wire: u64,
    _config_fingerprint: Option<[u8; 32]>,
}
struct Jobs {
    generation: u64,
    inventory_token: Option<Token>,
    preparing: bool,
    pending: Option<Control>,
    closed: bool,
    changed: bool,
    job: Option<Job>,
}
struct Completion {
    control: Control,
    outcome: Mutex<Outcome>,
}
#[derive(Clone)]
enum Outcome {
    Loading,
    Ready(Shared<Vec<u8>>),
    Failed(Code),
    Cancelled,
}
#[derive(Clone)]
struct EncodedPath(Shared<Vec<u8>>);
impl EncodedPath {
    #[allow(
        unsafe_code,
        reason = "borrows the exact immutable trusted OsStr encoding copied in this live process"
    )]
    fn path(&self) -> &Path {
        Path::new(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(&self.0) })
    }
}
struct Job {
    handle: Option<JoinHandle<()>>,
    completion: Shared<Completion>,
    token: Token,
    inventory: bool,
    _path: EncodedPath,
    _envelope: Reservation,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.completion.control.cancel();
        if let Some(handle) = self.handle.take() {
            let _joined = handle.join();
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        let jobs = self.jobs.get_mut().unwrap_or_else(|e| e.into_inner());
        let job = jobs.job.take();
        if let Some(job) = job {
            job.completion.control.cancel();
            drop(job);
        }
    }
}
struct Preparing<'a>(&'a Controller);
impl Drop for Preparing<'_> {
    fn drop(&mut self) {
        let mut jobs = lock(&self.0.jobs);
        jobs.preparing = false;
        jobs.pending = None;
    }
}
#[derive(Serialize)]
struct Wire<'a> {
    scan: &'a Token,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<&'a Inventory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<Code>,
}
fn json(
    budget: &MemoryBudget,
    limit: u64,
    value: &impl Serialize,
) -> Result<BudgetedVec<u8>, Code> {
    let mut writer = BudgetedWriter::new(budget, limit).map_err(|_| Code::MemoryLimit)?;
    let result = serde_json::to_writer(writer.serialization_sink(), value);
    if writer.error().is_some() {
        return Err(Code::MemoryLimit);
    }
    result.map_err(|_| Code::Unavailable)?;
    Ok(writer.into_buffer())
}
fn message(
    owner: &Controller,
    token: &Token,
    state: &'static str,
    code: Option<Code>,
) -> Result<Charged<Vec<u8>>, Code> {
    Ok(json(
        &owner.budget,
        owner.wire,
        &Wire {
            scan: token,
            state,
            items: None,
            code,
        },
    )?
    .into_charged_vec())
}
impl Controller {
    fn close(&self, changed: bool) {
        let (pending, completion) = {
            let mut jobs = lock(&self.jobs);
            jobs.changed |= changed;
            jobs.closed |= !changed;
            (
                jobs.pending.clone(),
                jobs.job.as_ref().map(|j| j.completion.clone()),
            )
        };
        if let Some(pending) = pending {
            pending.cancel();
        }
        if let Some(c) = completion {
            c.control.cancel();
            *lock(&c.outcome) = Outcome::Cancelled;
        }
    }
    fn check(&self, path: &Path) -> Result<(), Code> {
        {
            let jobs = lock(&self.jobs);
            if jobs.changed {
                return Err(Code::SourceChanged);
            }
            if jobs.closed {
                return Err(Code::SourceClosed);
            }
        }
        if Stamp::capture(path, &self.budget).map_err(code)? != self.stamp {
            self.close(true);
            return Err(Code::SourceChanged);
        }
        let jobs = lock(&self.jobs);
        if jobs.changed {
            return Err(Code::SourceChanged);
        }
        if jobs.closed {
            return Err(Code::SourceClosed);
        }
        Ok(())
    }
    fn completion(&self, token: &str) -> Result<(Token, Shared<Completion>), Code> {
        let jobs = lock(&self.jobs);
        if jobs.changed {
            return Err(Code::SourceChanged);
        }
        if jobs.closed {
            return Err(Code::SourceClosed);
        }
        let j = jobs
            .job
            .as_ref()
            .filter(|j| j.token.matches(token))
            .ok_or(Code::Stale)?;
        Ok((j.token, j.completion.clone()))
    }
    fn acknowledge_inventory(&self, token: Token) {
        let mut jobs = lock(&self.jobs);
        if let Some(job) = &jobs.job
            && job.token == token
            && job.inventory
            && matches!(&*lock(&job.completion.outcome), Outcome::Ready(_))
        {
            jobs.inventory_token = Some(token);
        }
    }

    fn start(
        &self,
        state: &State,
        request: Option<(&str, Selection)>,
    ) -> Result<Charged<Vec<u8>>, Code> {
        let details = request
            .map(|(_, selection)| {
                self.content_budget
                    .clone()
                    .map(|budget| (selection, budget))
                    .ok_or(Code::MemoryLimit)
            })
            .transpose()?;
        let old = {
            let mut jobs = lock(&self.jobs);
            if jobs.changed {
                return Err(Code::SourceChanged);
            }
            if jobs.closed {
                return Err(Code::SourceClosed);
            }
            if let Some((generation, _)) = request
                && !jobs
                    .inventory_token
                    .is_some_and(|token| token.matches(generation))
            {
                return Err(Code::Stale);
            }
            if jobs.preparing {
                return Err(Code::Busy);
            }
            if let Some(job) = &jobs.job {
                job.completion.control.cancel();
                *lock(&job.completion.outcome) = Outcome::Cancelled;
                if job.handle.as_ref().is_none_or(|h| !h.is_finished()) {
                    return Err(Code::Busy);
                }
            }
            jobs.preparing = true;
            if request.is_none() {
                jobs.inventory_token = None;
            }
            jobs.job.take()
        };
        let _preparing = Preparing(self);
        drop(old);
        self.check(&state.source)?;
        let token = {
            let mut jobs = lock(&self.jobs);
            jobs.generation = jobs.generation.checked_add(1).ok_or(Code::Unavailable)?;
            Token::new(self.id, jobs.generation)
        };
        let control = match &self.parent {
            Some(p) => Control::with_cancellation(&self.budget, p.token.clone()),
            None => Control::new(&self.budget),
        }
        .map_err(code)?;
        {
            let mut jobs = lock(&self.jobs);
            if jobs.changed {
                return Err(Code::SourceChanged);
            }
            if jobs.closed {
                return Err(Code::SourceClosed);
            }
            jobs.pending = Some(control.clone());
        }
        control.check().map_err(|stop| code(Error::Stopped(stop)))?;
        let envelope = self
            .budget
            .try_reserve(Category::ReaderWorking, STACK + SPAWN_WORK)
            .map_err(|_| Code::MemoryLimit)?;
        let mut path = BudgetedVec::try_with_capacity(
            &self.budget,
            Category::Metadata,
            state.source.as_os_str().as_encoded_bytes().len(),
        )
        .map_err(|_| Code::MemoryLimit)?;
        path.try_extend_from_slice(state.source.as_os_str().as_encoded_bytes())
            .map_err(|_| Code::MemoryLimit)?;
        let path = EncodedPath(path.try_into_shared().map_err(|_| Code::MemoryLimit)?);
        let completion = Shared::try_new(
            Completion {
                control,
                outcome: Mutex::new(Outcome::Loading),
            },
            self.budget
                .try_reserve(Category::Metadata, 0)
                .map_err(|_| Code::MemoryLimit)?,
        )
        .map_err(|_| Code::MemoryLimit)?;
        drop(lock(&completion.outcome));
        let output = message(self, &token, "loading", None)?;
        let worker_completion = completion.clone();
        let worker_path = path.clone();
        let admission = self.admission.clone();
        let kind = self.kind;
        let stamp = self.stamp;
        let cache = self.cache;
        let wire = self.wire;
        #[cfg(test)]
        BEFORE_SPAWN.with(|hook| {
            if let Some(hook) = hook.get() {
                hook(self)
            }
        });
        completion
            .control
            .check()
            .map_err(|s| code(Error::Stopped(s)))?;
        #[cfg(test)]
        SPAWN_DISPATCHES.with(|count| count.set(count.get() + 1));
        let handle = thread::Builder::new()
            .stack_size(STACK as usize)
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    #[cfg(test)]
                    if let Some(hook) = worker_hook {
                        hook();
                    }
                    let path = worker_path.path();
                    run(
                        path,
                        ScanOptions {
                            kind,
                            stamp,
                            cache,
                            wire,
                            details,
                        },
                        &token,
                        &admission,
                        &worker_completion.control,
                    )
                }));
                let result = match outcome {
                    Ok(Ok(bytes)) => Outcome::Ready(bytes),
                    Ok(Err(Code::Cancelled)) => Outcome::Cancelled,
                    Ok(Err(c)) => Outcome::Failed(c),
                    Err(_) => Outcome::Failed(Code::Unavailable),
                };
                let mut slot = lock(&worker_completion.outcome);
                *slot = if let Err(stop) = worker_completion.control.check() {
                    match stop {
                        Stop::Cancelled => Outcome::Cancelled,
                        _ => Outcome::Failed(code(Error::Stopped(stop))),
                    }
                } else {
                    result
                };
            })
            .map_err(|_| Code::Unavailable)?;
        let job = Job {
            handle: Some(handle),
            completion,
            token,
            inventory: request.is_none(),
            _path: path,
            _envelope: envelope,
        };
        let mut jobs = lock(&self.jobs);
        if jobs.closed || jobs.changed {
            drop(jobs);
            drop(job);
            return Err(Code::SourceClosed);
        }
        jobs.job = Some(job);
        drop(jobs);
        Ok(output)
    }
}
struct ScanOptions {
    kind: Kind,
    stamp: Stamp,
    cache: usize,
    wire: u64,
    details: Option<(Selection, MemoryBudget)>,
}
fn run(
    path: &Path,
    options: ScanOptions,
    token: &Token,
    admission: &RwLock<()>,
    control: &Control,
) -> Result<Shared<Vec<u8>>, Code> {
    let ScanOptions {
        kind,
        stamp,
        cache,
        wire,
        details,
    } = options;
    let _read = loop {
        control.check().map_err(|s| code(Error::Stopped(s)))?;
        match admission.try_read() {
            Ok(guard) => break guard,
            Err(TryLockError::Poisoned(_)) => return Err(Code::Unavailable),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(5)),
        }
    };
    if Stamp::capture(path, control.budget()).map_err(code)? != stamp {
        return Err(Code::SourceChanged);
    }
    let bytes = if let Some((selection, budget)) = details {
        let options = content::Options {
            stamp,
            cache,
            limit: wire,
            selection,
        };
        match kind {
            Kind::Dt => content::dt(path, control, &budget, options, token),
            Kind::OneCd => content::onecd(path, control, &budget, options, token),
        }?
    } else {
        let inventory = read_inventory(path, kind, stamp, cache, control).map_err(code)?;
        control.check().map_err(|s| code(Error::Stopped(s)))?;
        if Stamp::capture(path, control.budget()).map_err(code)? != stamp {
            return Err(Code::SourceChanged);
        }
        json(
            control.budget(),
            wire,
            &Wire {
                scan: token,
                state: "ready",
                items: Some(&inventory),
                code: None,
            },
        )?
    };
    control.check().map_err(|s| code(Error::Stopped(s)))?;
    if Stamp::capture(path, control.budget()).map_err(code)? != stamp {
        return Err(Code::SourceChanged);
    }
    bytes.try_into_shared().map_err(|_| Code::MemoryLimit)
}
fn read_inventory(
    path: &Path,
    kind: Kind,
    stamp: Stamp,
    cache: usize,
    control: &Control,
) -> Result<Inventory, Error> {
    match kind {
        Kind::Dt => dt::scan_bound(path, control, stamp),
        Kind::OneCd => {
            ReadSource::open_checked(path, control.clone(), cache, |file| stamp.check(file))
                .and_then(|source| source.inventory())
        }
    }
}
fn owner(state: &State) -> Result<&Controller, Code> {
    let owner = state.extensions.owner()?;
    if owner
        .parent
        .as_ref()
        .is_some_and(|p| p.token.load(Ordering::Acquire))
    {
        owner.close(false);
        return Err(Code::SourceClosed);
    }
    Ok(owner)
}
pub fn admission_error(state: &State) -> Option<Code> {
    state.extensions.owner().err()
}
pub fn start(state: &State) -> Result<Charged<Vec<u8>>, Code> {
    owner(state)?.start(state, None)
}
pub fn start_content(
    state: &State,
    scan: &str,
    id: &str,
    copy: Option<CopyKind>,
) -> Result<Charged<Vec<u8>>, Code> {
    let id = brace::Guid::parse(id)
        .filter(|id| !id.is_nil())
        .ok_or(Code::Invalid)?;
    owner(state)?.start(state, Some((scan, Selection { id, copy })))
}
pub fn status(state: &State, token: &str) -> Result<Charged<Vec<u8>>, Code> {
    let owner = owner(state)?;
    let (token, c) = owner.completion(token)?;
    owner.check(&state.source)?;
    let outcome = lock(&c.outcome).clone();
    let was_ready = matches!(&outcome, Outcome::Ready(_));
    let was_open = was_ready || matches!(&outcome, Outcome::Loading);
    let output = match outcome {
        Outcome::Ready(bytes) => {
            let mut copy =
                BudgetedVec::try_with_capacity(&owner.budget, Category::Response, bytes.len())
                    .map_err(|_| Code::MemoryLimit)?;
            if bytes.len() as u64 > owner.wire {
                return Err(Code::MemoryLimit);
            }
            copy.try_extend_from_slice(&bytes)
                .map_err(|_| Code::MemoryLimit)?;
            copy.into_charged_vec()
        }
        Outcome::Loading => message(owner, &token, "loading", None)?,
        Outcome::Cancelled => message(owner, &token, "cancelled", Some(Code::Cancelled))?,
        Outcome::Failed(c) => message(owner, &token, "failed", Some(c))?,
    };
    owner.check(&state.source)?;
    owner.completion(std::str::from_utf8(&token.0).map_err(|_| Code::Stale)?)?;
    if was_open && let Some(stop) = c.control.stop_reason() {
        return Err(code(Error::Stopped(stop)));
    }
    if was_ready {
        owner.acknowledge_inventory(token);
    }
    Ok(output)
}
pub fn cancel(state: &State, token: &str) -> Result<Charged<Vec<u8>>, Code> {
    let owner = owner(state)?;
    let (token, c) = owner.completion(token)?;
    c.control.cancel();
    *lock(&c.outcome) = Outcome::Cancelled;
    {
        let mut jobs = lock(&owner.jobs);
        if jobs.inventory_token == Some(token) {
            jobs.inventory_token = None;
        }
    }
    message(owner, &token, "cancelled", Some(Code::Cancelled))
}

#[cfg(all(test, unix))]
mod binding_tests {
    use super::*;
    use memory_budget::desktop::{Facts, Selection, resolve};

    #[test]
    fn status_refuses_completed_bytes_after_control_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let base = yellowcode_testkit::base::create(dir.path());
        let viewer = crate::viewer::Viewer::open(&base, None, crate::viewer::Lang::En).unwrap();
        let state = viewer.snapshot();
        let opening: serde_json::Value = serde_json::from_slice(&start(&state).unwrap()).unwrap();
        let token = opening["scan"].as_str().unwrap();
        let owner = owner(&state).unwrap();
        let handle = lock(&owner.jobs)
            .job
            .as_mut()
            .unwrap()
            .handle
            .take()
            .unwrap();
        handle.join().unwrap();
        let (_, completion) = owner.completion(token).unwrap();
        assert!(matches!(&*lock(&completion.outcome), Outcome::Ready(_)));
        completion.control.cancel();
        assert!(matches!(status(&state, token), Err(Code::Cancelled)));
        viewer.shutdown();
    }

    #[test]
    fn inventory_checks_the_open_descriptor_before_reading_replacement_bytes() {
        let budget = MemoryBudget::new(Limits::new(FEATURE_BYTES));
        let control = Control::new(&budget).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let base = yellowcode_testkit::base::create(dir.path());
        let path = base.join("1Cv8.1CD");
        let stamp = Stamp::capture(&path, &budget).unwrap();
        let replacement = tempfile::NamedTempFile::new_in(base).unwrap();
        replacement.persist(&path).unwrap();
        for kind in [Kind::OneCd, Kind::Dt] {
            assert!(matches!(
                read_inventory(&path, kind, stamp, 8 << 20, &control),
                Err(Error::Changed)
            ));
        }
    }
    #[test]
    fn readonly_refresh_never_constructs_another_feature_root_or_dispatches_preparation() {
        for mode in ["root-denied", "parent-cancelled", "unavailable"] {
            let process = crate::viewer::memory::test_process();
            let session = process
                .open(resolve(Selection::Unlimited, Facts::default()).unwrap())
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let base = yellowcode_testkit::base::create(dir.path());
            let mut state = crate::viewer::State::load_stages_with_session(
                &base,
                None,
                crate::viewer::Lang::En,
                crate::viewer::Viewer::OPEN,
                session.clone(),
                &mut |_| Ok(()),
            )
            .unwrap();
            if mode == "unavailable" {
                state.extensions = Availability::unavailable(Code::Unsupported);
            }
            let viewer = crate::viewer::Viewer::new(state);
            let old = viewer.snapshot();
            let original = old
                .extensions
                .owner
                .as_ref()
                .map(|o| (o.id, o.budget.clone()));
            PREPARATIONS.with(|events| events.set((0, 0)));
            let snapshot = session.budget().snapshot();
            let limits = Limits {
                admission: snapshot.admission,
                categories: snapshot.categories.map(|category| category.limit),
            };
            if mode == "root-denied" {
                session.budget().set_limits(Limits::new(0));
            }
            if mode == "parent-cancelled" {
                session.cancel();
            }
            let result = {
                let _exclusive = old.users_control.admission.write().unwrap();
                viewer.refresh_after_users_write()
            };
            session.budget().set_limits(limits);
            assert_eq!(
                PREPARATIONS.with(|events| events.get()),
                (0, 0),
                "{mode}: refresh must bypass feature preparation/root selection before main reload"
            );
            if result.is_ok() {
                let next = viewer.snapshot();
                match original {
                    Some((id, budget)) => {
                        let owner = next.extensions.owner().unwrap();
                        assert_eq!(owner.id, id);
                        assert!(owner.budget.same_instance(&budget));
                        assert!(owner.budget.same_scope(&budget));
                    }
                    None => {
                        assert!(next.extensions.owner.is_none());
                        assert_eq!(next.extensions.reason, Code::Unsupported);
                    }
                }
            }
        }
    }

    #[test]
    fn successful_bind_moves_the_exact_prepared_budget_without_a_second_scope() {
        let process = crate::viewer::memory::test_process();
        let session = process
            .open(resolve(Selection::Unlimited, Facts::default()).unwrap())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let base = yellowcode_testkit::base::create(dir.path());
        let path = base.join("1Cv8.1CD");
        let prepared = prepare(&path, Some(&session)).unwrap();
        let exact = prepared.budget.clone();
        let bound = Availability::bind(&path, Ok(prepared), None, Arc::new(RwLock::new(())));
        let owner = bound.owner().unwrap();
        assert!(owner.budget.same_instance(&exact));
        assert!(owner.budget.same_scope(&exact));
        assert!(owner.budget.same_instance(session.budget()));
        assert!(owner.parent.is_some());
    }
    #[test]
    fn shutdown_during_preparation_cancels_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let base = yellowcode_testkit::base::create(dir.path());
        let state = crate::viewer::State::load_stages_with_progress(
            &base,
            None,
            crate::viewer::Lang::En,
            crate::viewer::Viewer::OPEN,
            &mut |_| Ok(()),
        )
        .unwrap();
        let owner = state.extensions.owner().unwrap();
        fn close(owner: &Controller) {
            owner.close(false);
        }
        BEFORE_SPAWN.with(|hook| hook.set(Some(close)));
        let before = SPAWN_DISPATCHES.with(|count| count.get());
        let result = owner.start(&state, None);
        BEFORE_SPAWN.with(|hook| hook.set(None));
        assert!(matches!(
            result,
            Err(Code::Cancelled) | Err(Code::SourceClosed)
        ));
        let jobs = lock(&owner.jobs);
        assert!(!jobs.preparing);
        assert!(
            jobs.job.is_none(),
            "closed preparation must not dispatch a worker"
        );
        assert_eq!(
            SPAWN_DISPATCHES.with(|count| count.get()),
            before,
            "shutdown before spawn must cancel the pending control"
        );
        assert!(!state.nodes.is_empty());
    }

    #[test]
    fn worker_unwind_publishes_fixed_failure_and_releases_its_job_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let base = yellowcode_testkit::base::create(dir.path());
        let state = crate::viewer::State::load_stages_with_progress(
            &base,
            None,
            crate::viewer::Lang::En,
            crate::viewer::Viewer::OPEN,
            &mut |_| Ok(()),
        )
        .unwrap();
        fn fail() {
            panic!("test worker failure")
        }
        WORKER_HOOK.with(|hook| hook.set(Some(fail)));
        let opening = start(&state).unwrap();
        let opening: serde_json::Value = serde_json::from_slice(&opening).unwrap();
        let token = opening["scan"].as_str().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let reply = status(&state, token).unwrap();
            let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
            if reply["state"] != "loading" {
                assert_eq!(reply["state"], "failed");
                assert_eq!(reply["code"], "unavailable");
                assert!(reply.get("items").is_none());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        drop(state);
        std::fs::remove_file(base.join("1Cv8.1CD")).unwrap();
    }

    #[test]
    fn wire_limit_counts_escaping_and_the_complete_envelope_at_equality() {
        use brace::{
            Guid,
            binary::{Atom, BinNode},
        };
        let budget = MemoryBudget::new(Limits::new(FEATURE_BYTES));
        let mut inventory = Inventory::new(&budget).unwrap();
        let row = [
            BinNode::Atom(Atom::guid(
                Guid::parse("00000001-0000-0000-0000-000000000000").unwrap(),
            )),
            BinNode::Atom(Atom::int(1)),
            BinNode::Atom(Atom::str("\u{0001}\\\"йе\u{0308}")),
            BinNode::Atom(Atom::int(0)),
            BinNode::Atom(Atom::int(77)),
            BinNode::Atom(Atom::int(1)),
            BinNode::List(vec![BinNode::Atom(Atom::int(0))]),
            BinNode::Atom(Atom::str("")),
            BinNode::Atom(Atom::int(1)),
        ]
        .into_iter()
        .flat_map(|v| [BinNode::Atom(Atom::int(1)), v])
        .collect::<Vec<_>>();
        inventory.push_dt_row(&row).unwrap();
        let token = Token::new(1, 1);
        let wire = Wire {
            scan: &token,
            state: "ready",
            items: Some(&inventory),
            code: None,
        };
        let expected = serde_json::to_vec(&wire).unwrap();
        assert!(expected.windows(6).any(|s| s == br"\u0001"));
        let exact = json(&budget, expected.len() as u64, &wire).unwrap();
        assert_eq!(exact.as_slice(), expected);
        assert_eq!(
            json(&budget, expected.len() as u64 - 1, &wire).unwrap_err(),
            Code::MemoryLimit
        );
        drop(exact);
        drop(inventory);
        assert_eq!(budget.current_bytes(), 0);
    }
}
