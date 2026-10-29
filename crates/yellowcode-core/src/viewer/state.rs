use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use mdconf::{Configuration, Stages};
use serde_json::Value;

use crate::Result;
use crate::viewer::source::{self, Fingerprint, OpenError};
use crate::viewer::tree::{self, TreeNode};
use crate::viewer::{Lang, stats};

pub use crate::viewer::check::CheckCache;

pub struct FlatNode {
    pub node: TreeNode,
    pub children: Vec<usize>,
}

pub struct State {
    pub users_control: Arc<super::users_control::UsersControl>,
    pub skins: super::skin::Store,
    pub cfg: Configuration,
    pub source: PathBuf,
    pub names: Option<PathBuf>,
    pub lang: Lang,
    pub nodes: Arc<Vec<FlatNode>>,
    pub stats: Value,
    pub check: Arc<CheckCache>,
    pub stages: Stages,
    pub fingerprint: Option<Fingerprint>,
    pub memory: Option<source::OpenMemory>,
    pub config_entries: Option<source::ConfigEntries>,
    pub code_analysis_active: Arc<std::sync::atomic::AtomicUsize>,
}

fn flatten(mut n: TreeNode, out: &mut Vec<FlatNode>) -> usize {
    let kids = std::mem::take(&mut n.children);
    let id = out.len();
    out.push(FlatNode {
        node: n,
        children: Vec::new(),
    });
    let ids: Vec<usize> = kids.into_iter().map(|k| flatten(k, out)).collect();
    if let Some(f) = out.get_mut(id) {
        f.children = ids;
    }
    id
}

impl State {
    pub fn load(source: &Path, names: Option<&Path>, lang: Lang) -> Result<State> {
        Self::load_with_progress(source, names, lang, &mut |_| Ok(()))
    }

    pub fn load_with_progress(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        let memory = default_memory(source)?;
        Self::load_inner(source, names, lang, memory.as_ref(), progress)
    }

    pub fn load_with_memory(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        memory: &source::OpenMemory,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        Self::load_inner(source, names, lang, Some(memory), progress)
    }
    fn load_inner(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        memory: Option<&source::OpenMemory>,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        let source = &source::resolve(source);
        let opts = mdconf::LoadOptions {
            keep_source: false,
            ..Default::default()
        };
        let (cfg, entries) = if source::detect(source)? == source::SourceKind::Dt {
            let memory = memory.ok_or("DT opening memory context missing")?;
            let (cfg, _, entries) = source::open_retained_dt(source, &opts, memory, progress)?;
            (cfg, Some(entries))
        } else {
            let cfg = match memory {
                Some(memory) => source::open_with_memory(source, &opts, memory, progress),
                None => source::open_with_progress(source, &opts, progress),
            }?;
            (cfg, None)
        };
        let mut state = Self::build(source, names, lang, cfg, Stages::ALL, None, progress)?;
        state.memory = memory.cloned();
        state.config_entries = entries;
        Ok(state)
    }

    pub fn load_stages_with_progress(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        extra: Stages,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        let memory = default_memory(source)?;
        Self::load_stages_inner(source, names, lang, extra, memory.as_ref(), progress)
    }

    pub fn load_stages_with_memory(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        extra: Stages,
        memory: &source::OpenMemory,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        Self::load_stages_inner(source, names, lang, extra, Some(memory), progress)
    }
    fn load_stages_inner(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        extra: Stages,
        memory: Option<&source::OpenMemory>,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        let source = &source::resolve(source);
        let (cfg, fp, entries) = if source::detect(source)? == source::SourceKind::Dt {
            let memory = memory.ok_or("DT opening memory context missing")?;
            let (cfg, fp, entries) =
                source::open_retained_dt(source, &stage_options(extra), memory, progress)?;
            (cfg, fp, Some(entries))
        } else {
            let (cfg, fp) = match memory {
                Some(memory) => source::open_snapshot_with_memory(
                    source,
                    &stage_options(extra),
                    None,
                    memory,
                    progress,
                ),
                None => source::open_snapshot(source, &stage_options(extra), None, progress),
            }
            .map_err(|e| crate::Error::from(e.to_string()))?;
            (cfg, fp, None)
        };
        let mut state = Self::build(source, names, lang, cfg, extra, Some(fp), progress)?;
        state.memory = memory.cloned();
        state.config_entries = entries;
        Ok(state)
    }

    fn build(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        cfg: Configuration,
        stages: Stages,
        fingerprint: Option<Fingerprint>,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<State> {
        progress("tree")?;
        let mut nodes = Vec::new();
        {
            let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::Tree);
            flatten(tree::tree(&cfg, None, &[], lang), &mut nodes);
        }
        progress("statistics")?;
        let stats = {
            let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::Statistics);
            serde_json::to_value(stats::stats(&cfg))?
        };
        Ok(State {
            users_control: Arc::new(super::users_control::UsersControl::new(source)),
            skins: super::skin::Skins::store(),
            cfg,
            source: source.to_path_buf(),
            names: names.map(Path::to_path_buf),
            lang,
            nodes: Arc::new(nodes),
            stats,
            check: Arc::new(CheckCache::default()),
            stages,
            fingerprint,
            memory: None,
            config_entries: None,
            code_analysis_active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }
}

fn default_memory(path: &Path) -> Result<Option<source::OpenMemory>> {
    if source::detect(&source::resolve(path))? == source::SourceKind::Dt {
        Ok(Some(source::OpenMemory::resolve(None)?))
    } else {
        Ok(None)
    }
}

fn stage_options(stages: Stages) -> mdconf::LoadOptions {
    mdconf::LoadOptions {
        keep_source: false,
        profile: mdconf::LoadProfile::Query,
        extra_stages: stages,
        ..Default::default()
    }
}

pub type Loader = Arc<
    dyn Fn(
            &Path,
            Stages,
            Option<&Fingerprint>,
        ) -> std::result::Result<(Configuration, Fingerprint), OpenError>
        + Send
        + Sync,
>;

pub fn source_loader() -> Loader {
    source_loader_with_memory(None)
}
fn source_loader_with_memory(opening: Option<source::OpenMemory>) -> Loader {
    source_loader_with_entries(opening, None)
}
fn source_loader_with_entries(
    opening: Option<source::OpenMemory>,
    entries: Option<source::ConfigEntries>,
) -> Loader {
    let memory = std::sync::OnceLock::new();
    if let Some(opening) = opening {
        let _ = memory.set(Ok(opening));
    }
    Arc::new(move |path, stages, expect| {
        if let Some(entries) = &entries {
            return entries.load(path, &stage_options(stages), expect);
        }
        if memory.get().is_some()
            || source::detect(&source::resolve(path))
                .map_err(|e| OpenError::Decode(e.to_string()))?
                == source::SourceKind::Dt
        {
            let opening =
                memory.get_or_init(|| source::OpenMemory::resolve(None).map_err(|e| e.to_string()));
            let opening = opening.as_ref().map_err(|e| OpenError::Decode(e.clone()))?;
            source::open_snapshot_with_memory(
                path,
                &stage_options(stages),
                expect,
                opening,
                &mut |_| Ok(()),
            )
        } else {
            source::open_snapshot(path, &stage_options(stages), expect, &mut |_| Ok(()))
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Stages(Stages),
    CheckNames,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    Pending {
        what: String,
    },
    Changed,
    Failed {
        what: String,
        error: String,
        retry_in: Option<Duration>,
    },
}

pub struct Viewer {
    inner: Arc<Inner>,
}

pub const OLD_SNAPSHOT_WAIT: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_secs(10);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

struct Inner {
    current: RwLock<Arc<State>>,
    up: Mutex<Upgrade>,
    done: Condvar,
    loader: Loader,
    alive: std::sync::atomic::AtomicUsize,
    most_alive: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    panic_in_write_names: std::sync::atomic::AtomicBool,
}

struct Failure {
    target: Stages,
    error: String,
    retry_at: Option<Instant>,
}

#[derive(Default)]
struct Upgrade {
    running: bool,
    loading: Option<Stages>,
    pending: Stages,
    split: Vec<Stages>,
    names_pending: bool,
    changed: bool,
    failures: Vec<Failure>,
    names_failure: Option<Failure>,
    delays: Vec<(Stages, Duration)>,
    retry_delay: Option<Duration>,
    loads: usize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn singles(s: Stages) -> impl Iterator<Item = (Stages, &'static str)> {
    Stages::EACH
        .into_iter()
        .filter(move |(x, _)| s.contains(*x))
}

fn what(missing: Stages) -> String {
    let names: Vec<&str> = singles(missing).map(|(_, n)| n).collect();
    if names.len() > 2 {
        "all parts".to_string()
    } else {
        names.join(", ")
    }
}

impl Upgrade {
    fn failure(&self, target: Stages) -> Option<&Failure> {
        self.failures.iter().find(|f| f.target == target)
    }

    fn next_delay(&mut self, s: Stages) -> Duration {
        let first = self.retry_delay.unwrap_or(RETRY_DELAY);
        match self.delays.iter_mut().find(|(x, _)| *x == s) {
            Some((_, d)) => {
                *d = (*d * 2).min(MAX_RETRY_DELAY.max(first));
                *d
            }
            None => {
                self.delays.push((s, first));
                first
            }
        }
    }
}

struct LoadingGuard {
    inner: Arc<Inner>,
    armed: bool,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.inner
            .alive
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        let mut g = lock(&self.inner.up);
        g.running = false;
        g.loading = None;
        drop(g);
        self.inner.done.notify_all();
    }
}

impl Viewer {
    pub const OPEN: Stages = Stages::MODULES.union(Stages::RIGHTS);

    pub fn new(state: State) -> Viewer {
        Viewer::from_arc(Arc::new(state))
    }

    pub fn from_arc(state: Arc<State>) -> Viewer {
        let loader = source_loader_with_entries(state.memory.clone(), state.config_entries.clone());
        Viewer::build(state, loader)
    }

    fn build(state: Arc<State>, loader: Loader) -> Viewer {
        Viewer {
            inner: Arc::new(Inner {
                current: RwLock::new(state),
                up: Mutex::new(Upgrade::default()),
                done: Condvar::new(),
                loader,
                alive: Default::default(),
                most_alive: Default::default(),
                #[cfg(test)]
                panic_in_write_names: Default::default(),
            }),
        }
    }

    pub fn open(source: &Path, names: Option<&Path>, lang: Lang) -> Result<Viewer> {
        Viewer::open_with_progress(source, names, lang, &mut |_| Ok(()))
    }

    pub fn open_with_progress(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<Viewer> {
        let state = State::load_stages_with_progress(source, names, lang, Viewer::OPEN, progress)?;
        Ok(Viewer::from_loaded(state))
    }

    pub fn open_with_memory(
        source: &Path,
        names: Option<&Path>,
        lang: Lang,
        memory: &source::OpenMemory,
        progress: &mut impl FnMut(&'static str) -> Result<()>,
    ) -> Result<Viewer> {
        let state =
            State::load_stages_with_memory(source, names, lang, Viewer::OPEN, memory, progress)?;
        Ok(Viewer::from_loaded(state))
    }

    pub fn from_loaded(state: State) -> Viewer {
        crate::heap::trim();
        Viewer::new(state)
    }

    #[doc(hidden)]
    pub fn with_loader(state: State, loader: Loader) -> Viewer {
        Viewer::build(Arc::new(state), loader)
    }

    #[doc(hidden)]
    pub fn set_retry_delay(&self, delay: Duration) {
        lock(&self.inner.up).retry_delay = Some(delay);
    }

    pub fn snapshot(&self) -> Arc<State> {
        self.inner.snapshot()
    }

    pub fn shutdown(&self) {
        let control = self.snapshot().users_control.clone();
        match control.admission.write() {
            Ok(_exclusive) => control.revoke(),
            Err(poisoned) => {
                let _exclusive = poisoned.into_inner();
                control.revoke();
                control.admission.clear_poison();
            }
        };
    }

    pub fn refresh_after_users_write(&self) -> Result<()> {
        let old = self.snapshot();
        let mut next = match State::load_stages_inner(
            &old.source,
            old.names.as_deref(),
            old.lang,
            old.stages,
            old.memory.as_ref(),
            &mut |_| Ok(()),
        ) {
            Ok(next) => next,
            Err(_) => {
                old.users_control.revoke();
                return Err("viewer refresh failed".into());
            }
        };
        next.users_control = old.users_control.clone();
        next.skins = old.skins.clone();
        *self
            .inner
            .current
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        let mut up = lock(&self.inner.up);
        up.changed = false;
        up.failures.clear();
        up.names_failure = None;
        Ok(())
    }

    pub fn loads(&self) -> usize {
        lock(&self.inner.up).loads
    }

    #[doc(hidden)]
    pub fn most_loaders(&self) -> usize {
        self.inner
            .most_alive
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn idle(&self) -> bool {
        !lock(&self.inner.up).running
    }

    pub fn with(&self, need: Need, wait: Duration) -> std::result::Result<Arc<State>, Unavailable> {
        let inner = &self.inner;
        let deadline = Instant::now() + wait;
        let mut g = lock(&inner.up);
        loop {
            let snap = inner.snapshot();
            let missing = match need {
                Need::Stages(s) => {
                    if snap.stages.contains(s) {
                        return Ok(snap);
                    }
                    s.minus(snap.stages)
                }
                Need::CheckNames => {
                    if names_known(&snap) {
                        return Ok(snap);
                    }
                    Stages::ALL.minus(snap.stages)
                }
            };
            if g.changed {
                return Err(Unavailable::Changed);
            }
            let now = Instant::now();
            if need == Need::CheckNames {
                if let Some(f) = &g.names_failure {
                    match f.retry_at {
                        None => {
                            return Err(Unavailable::Failed {
                                what: "names".into(),
                                error: f.error.clone(),
                                retry_in: None,
                            });
                        }
                        Some(at) if at > now => {
                            return Err(Unavailable::Failed {
                                what: "names".into(),
                                error: f.error.clone(),
                                retry_in: Some(at - now),
                            });
                        }
                        Some(_) => g.names_failure = None,
                    }
                }
                g.names_pending = true;
            }
            for (s, stage_name) in singles(missing) {
                let name = if need == Need::CheckNames {
                    "names"
                } else {
                    stage_name
                };
                let target = snap.stages.union(s);
                let Some(f) = g.failure(target) else {
                    continue;
                };
                match f.retry_at {
                    None => {
                        return Err(Unavailable::Failed {
                            what: name.into(),
                            error: f.error.clone(),
                            retry_in: None,
                        });
                    }
                    Some(at) if at > now => {
                        return Err(Unavailable::Failed {
                            what: name.into(),
                            error: f.error.clone(),
                            retry_in: Some(at - now),
                        });
                    }
                    Some(_) => g.failures.retain(|f| f.target != target),
                }
            }
            if need != Need::CheckNames {
                g.pending = g.pending.union(missing);
            }
            if !g.running {
                g.running = true;
                let worker = Arc::clone(inner);
                let spawned = std::thread::Builder::new()
                    .name("viewer-loader".into())
                    .spawn(move || Inner::run(worker));
                if let Err(e) = spawned {
                    g.running = false;
                    return Err(Unavailable::Failed {
                        what: if need == Need::CheckNames {
                            "names".into()
                        } else {
                            what(missing)
                        },
                        error: format!("cannot start the loader: {e}"),
                        retry_in: Some(Duration::from_secs(1)),
                    });
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Unavailable::Pending {
                    what: if need == Need::CheckNames {
                        "names".into()
                    } else {
                        what(missing)
                    },
                });
            }
            g = inner
                .done
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

fn names_known(st: &State) -> bool {
    st.names.is_some() || lock(&st.check.names).is_some()
}

impl Inner {
    fn snapshot(&self) -> Arc<State> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn run(self: Arc<Inner>) {
        use std::sync::atomic::Ordering;
        let now = self.alive.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_alive.fetch_max(now, Ordering::SeqCst);
        let mut guard = LoadingGuard {
            inner: Arc::clone(&self),
            armed: true,
        };
        loop {
            let control = self.snapshot().users_control.clone();
            let admission = control.admission.read().unwrap_or_else(|e| e.into_inner());
            if control.closed() {
                return;
            }
            let cur = self.snapshot();
            let (todo, names_job) = {
                let mut g = lock(&self.up);
                let names_job = std::mem::take(&mut g.names_pending);
                let todo = if g.split.is_empty() {
                    std::mem::take(&mut g.pending).minus(cur.stages)
                } else {
                    g.split.remove(0).minus(cur.stages)
                };
                if todo.is_empty() && !names_job && g.split.is_empty() {
                    g.running = false;
                    g.loading = None;
                    self.alive.fetch_sub(1, Ordering::SeqCst);
                    guard.armed = false;
                    drop(g);
                    self.done.notify_all();
                    return;
                }
                (todo, names_job)
            };
            let mut write_names = false;
            let mut todo = todo;
            if names_job && !names_known(&cur) {
                match cur.check.known(&cur.source, cur.names.as_deref()) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        write_names = true;
                        todo = todo.union(Stages::ALL.minus(cur.stages));
                    }
                    Err(e) => self.names_failed(&*e),
                }
                self.done.notify_all();
            }
            {
                let mut g = lock(&self.up);
                let now = Instant::now();
                for (s, _) in singles(todo) {
                    let Some(f) = g.failure(cur.stages.union(s)) else {
                        continue;
                    };
                    if f.retry_at.is_none_or(|at| at > now) {
                        todo = todo.minus(s);
                        if write_names {
                            write_names = false;
                            if f.retry_at.is_none() {
                                g.names_failure = Some(Failure {
                                    target: Stages::ALL,
                                    error: f.error.clone(),
                                    retry_at: None,
                                });
                            }
                        }
                    }
                }
            }
            if todo.is_empty() {
                if write_names {
                    self.write_names(&cur);
                }
                self.done.notify_all();
                continue;
            }
            let target = cur.stages.union(todo);
            lock(&self.up).loading = Some(target);
            let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.loader)(&cur.source, target, cur.fingerprint.as_ref())
            }));
            let result = match loaded {
                Ok(Ok((cfg, fp))) => self.recheck(&cur).map(|()| (cfg, fp)),
                Ok(Err(OpenError::Decode(e))) => self.recheck(&cur).and(Err(OpenError::Decode(e))),
                Ok(Err(e)) => Err(e),
                Err(p) => Err(OpenError::Decode(format!(
                    "internal error while loading: {}",
                    panic_text(&p)
                ))),
            };
            match result {
                Ok((cfg, fp)) => {
                    let next = Arc::new(State {
                        users_control: Arc::clone(&cur.users_control),
                        skins: cur.skins.clone(),
                        cfg,
                        source: cur.source.clone(),
                        names: cur.names.clone(),
                        lang: cur.lang,
                        nodes: Arc::clone(&cur.nodes),
                        stats: cur.stats.clone(),
                        check: Arc::clone(&cur.check),
                        stages: target,
                        fingerprint: Some(fp),
                        memory: cur.memory.clone(),
                        config_entries: cur.config_entries.clone(),
                        code_analysis_active: Arc::clone(&cur.code_analysis_active),
                    });
                    let old = std::mem::replace(
                        &mut *self.current.write().unwrap_or_else(|e| e.into_inner()),
                        Arc::clone(&next),
                    );
                    {
                        let mut g = lock(&self.up);
                        g.loads += 1;
                        g.loading = None;
                        for (s, _) in singles(todo) {
                            g.delays.retain(|(x, _)| *x != s);
                        }
                    }
                    self.done.notify_all();
                    if write_names {
                        self.write_names(&next);
                    }
                    drop(next);
                    drop(cur);
                    drop(admission);
                    let t = Instant::now();
                    while Arc::strong_count(&old) > 1 {
                        if t.elapsed() >= OLD_SNAPSHOT_WAIT {
                            eprintln!(
                                "v1b: old snapshot still held after {} s; starting the next load",
                                OLD_SNAPSHOT_WAIT.as_secs()
                            );
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    drop(old);
                    crate::heap::trim();
                }
                Err(OpenError::Changed) => {
                    let mut g = lock(&self.up);
                    g.changed = true;
                    g.pending = Stages::NONE;
                    g.split.clear();
                    g.loading = None;
                }
                Err(OpenError::Transient(e)) => {
                    let mut g = lock(&self.up);
                    let now = Instant::now();
                    for (s, _) in singles(todo) {
                        let delay = g.next_delay(s);
                        let t = cur.stages.union(s);
                        g.failures.retain(|f| f.target != t);
                        g.failures.push(Failure {
                            target: t,
                            error: e.clone(),
                            retry_at: Some(now + delay),
                        });
                    }
                    g.loading = None;
                }
                Err(OpenError::Decode(e)) => {
                    let mut g = lock(&self.up);
                    let several: Vec<Stages> = singles(todo).map(|(s, _)| s).collect();
                    if several.len() > 1 {
                        g.split.extend(several);
                    } else {
                        g.failures.retain(|f| f.target != target);
                        g.failures.push(Failure {
                            target,
                            error: e.clone(),
                            retry_at: None,
                        });
                        if write_names {
                            g.names_failure = Some(Failure {
                                target: Stages::ALL,
                                error: e.clone(),
                                retry_at: None,
                            });
                        }
                    }
                    g.loading = None;
                }
            }
            self.done.notify_all();
        }
    }

    fn recheck(&self, cur: &State) -> std::result::Result<(), OpenError> {
        let Some(fp) = &cur.fingerprint else {
            return Ok(());
        };
        match source::fingerprint(&cur.source) {
            Ok(now) if now == *fp => Ok(()),
            Err(e @ OpenError::Transient(_)) => Err(e),
            Ok(_) | Err(OpenError::Changed | OpenError::Decode(_)) => Err(OpenError::Changed),
        }
    }

    fn names_failed(&self, e: &(dyn std::error::Error + 'static)) {
        let mut g = lock(&self.up);
        let retry_at = source::is_io(e).then(|| Instant::now() + g.next_delay(Stages::NONE));
        g.names_failure = Some(Failure {
            target: Stages::ALL,
            error: e.to_string(),
            retry_at,
        });
    }

    fn write_names(&self, st: &State) {
        #[cfg(test)]
        if self
            .panic_in_write_names
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            panic!("write_names: test panic");
        }
        match st.check.names_dir(&st.cfg, &st.source, st.names.as_deref()) {
            Ok(_) => lock(&self.up).delays.retain(|(x, _)| *x != Stages::NONE),
            Err(e) => self.names_failed(&*e),
        }
        self.done.notify_all();
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".to_string())
}
