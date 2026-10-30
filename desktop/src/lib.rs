pub mod bridge;
pub mod convert;
pub mod recent;
pub mod skins;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use yellowcode_core::viewer::Lang;
use yellowcode_core::viewer::memory::{Process, Session};
use yellowcode_core::viewer::state::{State, Viewer};

use recent::Recent;

const MAX_WORKERS: usize = 8;
const QUEUE_CAP: usize = 256;
const SMOKE_TIMEOUT: Duration = Duration::from_secs(120);

fn smoke_timeout() -> Duration {
    std::env::var("ONEC_SMOKE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(SMOKE_TIMEOUT, Duration::from_secs)
}

fn process_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Job = Box<dyn FnOnce(bool) + Send>;

pub struct Gate {
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
    cap: usize,
    workers: AtomicUsize,
}

impl Gate {
    pub fn new(workers: usize, cap: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            cap,
            workers: AtomicUsize::new(0),
        });
        for _ in 0..workers.max(1) {
            let g = gate.clone();
            let spawned = std::thread::Builder::new().spawn(move || g.work());
            if spawned.is_ok() {
                gate.workers.fetch_add(1, Ordering::SeqCst);
            }
        }
        gate
    }

    fn work(&self) {
        loop {
            let job = {
                let mut q = lock(&self.queue);
                loop {
                    if let Some(j) = q.pop_front() {
                        break j;
                    }
                    q = self.ready.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(true)));
        }
    }

    pub fn run(&self, job: impl FnOnce(bool) + Send + 'static) {
        if self.workers.load(Ordering::SeqCst) == 0 {
            return job(false);
        }
        let mut q = lock(&self.queue);
        if q.len() >= self.cap {
            drop(q);
            return job(false);
        }
        q.push_back(Box::new(job));
        drop(q);
        self.ready.notify_one();
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    Idle,
    Loading {
        path: String,
        elapsed_ms: u64,
        stage: String,
    },
    Ready {
        path: String,
        name: String,
    },
    Error {
        path: String,
        error: String,
    },
}

const MAX_LOADERS: usize = 2;

#[derive(Debug, PartialEq)]
pub enum Finish {
    Ready(String),
    Failed(String),
    Discarded,
}

pub struct AppState {
    pub skins: yellowcode_core::viewer::skin::Store,
    pub current: RwLock<Option<Arc<Viewer>>>,
    pub recent: Mutex<Recent>,
    pub recent_file: PathBuf,
    status: Mutex<Status>,
    since: Mutex<Option<Instant>>,
    load_id: AtomicU64,
    loaders: AtomicUsize,
    memory: Result<Process, memory_budget::BudgetError>,
    pending_memory: Mutex<Option<(u64, Arc<Session>)>>,
    lang: Lang,
    names: Option<PathBuf>,
    smoke: bool,
    cli_memory: Option<yellowcode_core::viewer::source::OpenMemory>,
    gate: Arc<Gate>,
}

impl AppState {
    pub fn new(
        recent: Recent,
        recent_file: PathBuf,
        lang: Lang,
        names: Option<PathBuf>,
        smoke: bool,
    ) -> AppState {
        Self::with_process(
            recent,
            recent_file,
            lang,
            names,
            smoke,
            Process::unlimited(),
        )
    }

    fn with_process(
        recent: Recent,
        recent_file: PathBuf,
        lang: Lang,
        names: Option<PathBuf>,
        smoke: bool,
        memory: Result<Process, memory_budget::BudgetError>,
    ) -> AppState {
        AppState {
            skins: yellowcode_core::viewer::skin::Skins::store(),
            current: RwLock::new(None),
            recent: Mutex::new(recent),
            recent_file,
            status: Mutex::new(Status::Idle),
            since: Mutex::new(None),
            load_id: AtomicU64::new(0),
            loaders: AtomicUsize::new(0),
            memory,
            pending_memory: Mutex::new(None),
            lang,
            names,
            smoke,
            cli_memory: None,
            gate: Gate::new(MAX_WORKERS, QUEUE_CAP),
        }
    }

    fn memory_session(
        &self,
        path: &Path,
        facts: memory_budget::desktop::Facts,
    ) -> Result<Arc<Session>, String> {
        let policy = match &self.cli_memory {
            Some(memory) => memory_budget::desktop::resolve(
                memory_budget::desktop::Selection::Bytes(memory.budget()),
                facts,
            )
            .map_err(|e| e.to_string())?,
            None => lock(&self.recent).clone().memory_policy(path, facts)?,
        };
        let process = self.memory.as_ref().map_err(ToString::to_string)?;
        process.refresh(facts);
        process.open(policy).map_err(|e| e.to_string())
    }

    fn bind_memory(&self, id: u64, session: Arc<Session>) -> Result<(), String> {
        let status = lock(&self.status);
        if self.load_id.load(Ordering::SeqCst) != id || !matches!(*status, Status::Loading { .. }) {
            session.cancel();
            return Err("loading cancelled".into());
        }
        *lock(&self.pending_memory) = Some((id, session));
        Ok(())
    }

    fn take_pending_memory(&self, id: u64) -> Option<Arc<Session>> {
        let mut pending = lock(&self.pending_memory);
        if pending.as_ref().is_some_and(|(owner, _)| *owner == id) {
            pending.take().map(|(_, session)| session)
        } else {
            None
        }
    }

    pub fn begin(&self, path: &str) -> Result<u64, String> {
        let mut status = lock(&self.status);
        if matches!(*status, Status::Loading { .. }) {
            return Err("another source is being loaded".into());
        }
        if self.loaders.load(Ordering::SeqCst) >= MAX_LOADERS {
            return Err("cancelled loads are still finishing, try again in a moment".into());
        }
        self.loaders.fetch_add(1, Ordering::SeqCst);
        *lock(&self.since) = Some(Instant::now());
        *status = Status::Loading {
            path: path.to_string(),
            elapsed_ms: 0,
            stage: "container".into(),
        };
        Ok(self.load_id.load(Ordering::SeqCst))
    }

    pub fn abort(&self, id: u64) {
        self.loaders.fetch_sub(1, Ordering::SeqCst);
        let mut status = lock(&self.status);
        if self.load_id.load(Ordering::SeqCst) == id {
            *status = Status::Idle;
        }
        let pending = self.take_pending_memory(id);
        drop(status);
        if let Some(session) = pending {
            session.cancel();
        }
    }

    fn report_progress(
        &self,
        id: u64,
        stage: &str,
        report: impl FnOnce(&Status),
    ) -> Result<(), String> {
        let mut status = lock(&self.status);
        if self.load_id.load(Ordering::SeqCst) != id {
            return Err("loading cancelled".into());
        }
        if let Status::Loading { stage: current, .. } = &mut *status
            && current.as_str() != stage
        {
            *current = stage.to_string();
            report(&status);
        }
        Ok(())
    }

    pub fn cancel(&self) -> bool {
        let mut status = lock(&self.status);
        if !matches!(*status, Status::Loading { .. }) {
            return false;
        }
        self.load_id.fetch_add(1, Ordering::SeqCst);
        *status = Status::Idle;
        let pending = lock(&self.pending_memory).take();
        drop(status);
        if let Some((_, session)) = pending {
            session.cancel();
        }
        true
    }

    pub fn finish(&self, id: u64, path: &str, loaded: Result<State, String>) -> Finish {
        let loaded = loaded.map(|mut state| {
            state.skins = self.skins.clone();
            let name = yellowcode_core::viewer::tree::config_name(&state.cfg);
            (Arc::new(Viewer::from_loaded(state)), name)
        });
        let mut previous = None;
        let pending;
        let out = {
            let mut status = lock(&self.status);
            pending = self.take_pending_memory(id);
            if self.load_id.load(Ordering::SeqCst) != id {
                Finish::Discarded
            } else {
                match loaded {
                    Ok((viewer, name)) => {
                        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
                        if let Some(old) = current.as_ref() {
                            old.shutdown();
                        }
                        previous = current.replace(viewer);
                        drop(current);
                        {
                            let mut r = lock(&self.recent);
                            r.touch(Path::new(path));
                            let _ = r.save(&self.recent_file);
                        }
                        *status = Status::Ready {
                            path: path.to_string(),
                            name: name.clone(),
                        };
                        Finish::Ready(name)
                    }
                    Err(error) => {
                        *status = Status::Error {
                            path: path.to_string(),
                            error: error.clone(),
                        };
                        Finish::Failed(error)
                    }
                }
            }
        };
        if !matches!(out, Finish::Ready(_))
            && let Some(session) = pending
        {
            session.cancel();
        }
        if let Some(old) = previous
            && let Some(session) = &old.snapshot().session
        {
            session.close();
        }
        self.loaders.fetch_sub(1, Ordering::SeqCst);
        out
    }

    fn since_start(&self) -> Duration {
        lock(&self.since).map_or(Duration::ZERO, |t| t.elapsed())
    }

    pub fn status(&self) -> Status {
        let mut s = lock(&self.status).clone();
        if let Status::Loading { elapsed_ms, .. } = &mut s {
            *elapsed_ms = lock(&self.since).map_or(0, |t| {
                u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
            });
        }
        s
    }
}

#[derive(Serialize, Clone)]
struct SourceEvent {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
pub struct RecentOut {
    name: String,
    path: String,
    label: String,
    exists: bool,
    memory: recent::MemorySetting,
    kind: recent::Kind,
    origin: recent::Origin,
    recent_rank: Option<usize>,
}

fn event(path: &str, name: Option<String>, error: Option<String>) -> SourceEvent {
    SourceEvent {
        path: path.to_string(),
        name,
        error,
    }
}

fn start_load(app: &AppHandle, path: String) -> Result<(), String> {
    let path = resolved(Path::new(&path)).unwrap_or(path);
    let st = app.state::<AppState>();
    let session = st.memory_session(Path::new(&path), memory_budget::desktop::facts())?;
    let id = st.begin(&path)?;
    if let Err(error) = st.bind_memory(id, session.clone()) {
        st.abort(id);
        return Err(error);
    }
    if let Err(e) = show_splash(app) {
        st.abort(id);
        return Err(e.to_string());
    }
    let _ = app.emit("source-loading", event(&path, None, None));
    let app2 = app.clone();
    let spawned = std::thread::Builder::new().spawn(move || {
        let st = app2.state::<AppState>();
        let (lang, names) = (st.lang, st.names.clone());
        let src = PathBuf::from(&path);
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            State::load_stages_with_session(
                &src,
                names.as_deref(),
                lang,
                Viewer::OPEN,
                session,
                &mut |stage| {
                    st.report_progress(id, stage, |status| {
                        let _ = app2.emit("source-progress", status);
                    })?;
                    Ok(())
                },
            )
            .map_err(|e| e.to_string())
        }))
        .unwrap_or_else(|_| Err("internal error while loading".to_string()));
        let smoke_error = loaded.as_ref().err().cloned();
        match st.finish(id, &path, loaded) {
            Finish::Ready(name) => {
                if st.smoke {
                    println!("smoke: loaded in {:.2} s", st.since_start().as_secs_f64());
                }
                close_splash(&app2);
                if let Some(window) = app2.get_webview_window("main") {
                    let _ = window.set_title("Yellowcode");
                    let _ = window.set_size(tauri::LogicalSize::new(1280.0, 800.0));
                }
                let _ = app2.emit("source-ready", event(&path, Some(name), None));
            }
            Finish::Failed(error) => {
                let _ = app2.emit("source-error", event(&path, None, Some(error)));
            }
            Finish::Discarded => {
                if st.smoke {
                    eprintln!(
                        "yellowcode: the load of {path} was discarded{}",
                        smoke_error.map_or(String::new(), |e| format!(" (it failed: {e})"))
                    );
                    std::process::exit(1);
                }
            }
        }
        if st.smoke
            && let Status::Error { error, .. } = st.status()
        {
            eprintln!("yellowcode: {error}");
            std::process::exit(1);
        }
    });
    if spawned.is_err() {
        st.abort(id);
        close_splash(app);
        return Err("cannot start the loading thread".into());
    }
    Ok(())
}

fn show_splash(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("splash") {
        let _ = window.destroy();
    }
    let url = if cfg!(windows) {
        "http://onec.localhost/splash.html"
    } else {
        "onec://localhost/splash.html"
    };
    let window = WebviewWindowBuilder::new(
        app,
        "splash",
        WebviewUrl::CustomProtocol(url.parse().map_err(tauri::Error::InvalidUrl)?),
    )
    .title("Yellowcode")
    .inner_size(600.0, 340.0)
    .resizable(false)
    .decorations(false)
    .center()
    .build()?;
    let handle = app.clone();
    window.on_window_event(move |window_event| {
        if matches!(window_event, tauri::WindowEvent::CloseRequested { .. }) {
            let st = handle.state::<AppState>();
            if st.cancel() {
                let _ = handle.emit("source-cancelled", event("", None, None));
            }
            if let Some(main) = handle.get_webview_window("main") {
                let _ = main.show();
                let _ = main.set_focus();
            }
        }
    });
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    Ok(())
}

fn close_splash(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("splash") {
        let _ = window.destroy();
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[derive(Serialize)]
struct MemoryPreview {
    effective_mib: Option<u64>,
    ceiling_mib: Option<u64>,
    fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_visibility: Option<&'static str>,
}

#[tauri::command(async)]
fn preview_source_memory(memory: recent::MemorySetting) -> Result<MemoryPreview, String> {
    let policy = memory.resolve(memory_budget::desktop::facts())?;
    Ok(MemoryPreview {
        effective_mib: (!policy.unlimited).then_some(policy.budget / (1 << 20)),
        ceiling_mib: policy.ceiling.map(|bytes| bytes / (1 << 20)),
        fallback: policy.fallback,
        external_visibility: match policy.external_visibility {
            memory_budget::discovery::Visibility::Visible => None,
            memory_budget::discovery::Visibility::Partial => Some("partial"),
            memory_budget::discovery::Visibility::Unknown => Some("unknown"),
        },
    })
}

#[tauri::command(async)]
fn save_source(
    state: tauri::State<'_, AppState>,
    old: Option<String>,
    path: String,
    name: String,
    memory: recent::MemorySetting,
    origin: Option<recent::Origin>,
) -> Result<String, String> {
    memory.resolve(memory_budget::desktop::facts())?;
    let path = resolved(Path::new(&path)).unwrap_or(path);
    let metadata_only = old.as_deref() == Some(path.as_str())
        && lock(&state.recent)
            .sources
            .iter()
            .any(|i| i.path == Path::new(&path));
    if !metadata_only {
        yellowcode_core::viewer::source::detect(Path::new(&path)).map_err(|e| e.to_string())?;
    }
    let mut recent = lock(&state.recent);
    let mut updated = recent.clone();
    let saved = updated.set_memory(
        old.as_deref().map(Path::new),
        Path::new(&path),
        &name,
        memory,
        memory_budget::desktop::facts(),
    )?;
    let saved_path = saved
        .to_str()
        .ok_or("source path must be valid Unicode")?
        .to_string();
    if let Some(origin) = origin {
        updated.set_origin(&saved, origin)?;
    }
    updated
        .save(&state.recent_file)
        .map_err(|e| e.to_string())?;
    *recent = updated;
    Ok(saved_path)
}

#[tauri::command]
fn startup_dialog_size(window: tauri::WebviewWindow, dialog: &str) -> Result<(), String> {
    let (width, height) = match dialog {
        "wizard" => (490.0, 650.0),
        "settings" => (548.0, 704.0),
        "" => (820.0, 500.0),
        _ => return Err("unknown startup dialog".into()),
    };
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn startup_version() -> &'static str {
    yellowcode_core::VERSION
}

#[tauri::command]
fn startup_language(state: tauri::State<'_, AppState>) -> &'static str {
    match state.lang {
        Lang::Ru => "ru",
        Lang::En => "en",
    }
}

#[tauri::command]
fn dismiss_load_error(app: AppHandle) {
    if matches!(app.state::<AppState>().status(), Status::Error { .. }) {
        close_splash(&app);
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn restart_application(app: AppHandle) {
    app.restart();
}

#[tauri::command]
async fn pick_source(app: AppHandle, folder: Option<bool>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let dialog = app.dialog().file();
    let picked = if folder.unwrap_or(false) {
        dialog.blocking_pick_folder()
    } else {
        dialog
            .add_filter(
                "1C configuration, DT metadata or file information base",
                &["cf", "cfe", "dt", "1CD"],
            )
            .blocking_pick_file()
    };
    resolved(&picked?.into_path().ok()?)
}

fn resolved(path: &Path) -> Option<String> {
    yellowcode_core::viewer::source::resolve(path)
        .to_str()
        .map(str::to_string)
}

#[tauri::command]
async fn open_source(app: AppHandle, path: String) -> Result<(), String> {
    start_load(&app, path)
}

#[tauri::command(async)]
fn recent(state: tauri::State<'_, AppState>) -> Vec<RecentOut> {
    let items = lock(&state.recent).items.clone();
    source_output(&items, &items)
}

#[tauri::command(async)]
fn sources(state: tauri::State<'_, AppState>) -> Vec<RecentOut> {
    let recent = lock(&state.recent).clone();
    source_output(&recent.sources, &recent.items)
}

fn source_output(items: &[recent::RecentItem], history: &[recent::RecentItem]) -> Vec<RecentOut> {
    Recent::exists_now(items)
        .into_iter()
        .filter_map(|i| {
            Some(RecentOut {
                path: i.path.to_str()?.to_string(),
                label: recent::menu_label(&i.path),
                exists: i.exists,
                name: i.name,
                memory: i.memory,
                kind: i.kind,
                origin: i.origin,
                recent_rank: history.iter().position(|h| h.path == i.path),
            })
        })
        .collect()
}

#[tauri::command(async)]
fn forget_recent(state: tauri::State<'_, AppState>, path: String) -> Result<(), String> {
    let mut recent = lock(&state.recent);
    let mut updated = recent.clone();
    updated.remove(Path::new(&path));
    updated
        .save(&state.recent_file)
        .map_err(|e| e.to_string())?;
    *recent = updated;
    Ok(())
}

#[tauri::command]
fn status(state: tauri::State<'_, AppState>) -> Status {
    state.status()
}

#[tauri::command]
fn cancel_load(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.cancel() {
        close_splash(&app);
        let _ = app.emit("source-cancelled", event("", None, None));
    }
}

fn reply_to_http(r: bridge::Reply, respond: impl FnOnce(tauri::http::Response<Vec<u8>>)) {
    let mut b = tauri::http::Response::builder()
        .status(r.status)
        .header("Content-Type", r.content_type)
        .header("Cache-Control", r.cache_control)
        .header("X-Content-Type-Options", "nosniff");
    if let Some(csp) = r.csp {
        b = b.header("Content-Security-Policy", csp);
    }
    r.body.deliver(|bytes| {
        let response = b.body(bytes).unwrap_or_else(|_| {
            let mut e = tauri::http::Response::new(b"error\n".to_vec());
            *e.status_mut() = tauri::http::StatusCode::INTERNAL_SERVER_ERROR;
            e
        });
        respond(response);
    });
}

fn serve_scheme(
    app: AppHandle,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let method = request.method().as_str().to_string();
    let target = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_string(), |p| p.as_str().to_string());
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let origin = header("origin");
    let fetch_site = header("sec-fetch-site");
    let permission = header("x-user-write-permission");
    let authority = request
        .uri()
        .authority()
        .map(|a| a.as_str().to_owned())
        .unwrap_or_default();
    let body = request.into_body();
    let st = app.state::<AppState>();
    let current = st.current.read().unwrap_or_else(|e| e.into_inner()).clone();
    let gate = st.gate.clone();
    let skins = st.skins.clone();
    gate.run(move |accepted| {
        if !accepted {
            reply_to_http(bridge::busy(), |response| responder.respond(response));
            return;
        }
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bridge::respond_local(
                &skins,
                current.as_deref(),
                &method,
                &target,
                content_type.as_deref(),
                &body,
                bridge::LocalRequestHeaders {
                    authority: &authority,
                    origin: origin.as_deref(),
                    fetch_site: fetch_site.as_deref(),
                    permission: permission.as_deref(),
                },
            )
        }))
        .unwrap_or_else(|_| bridge::internal_error());
        let smoke_done =
            app.state::<AppState>().smoke && target == "/api/info" && reply.status == 200;
        reply_to_http(reply, |response| responder.respond(response));
        if smoke_done {
            println!(
                "smoke: /api/info answered {:.2} s after the process start",
                process_start().elapsed().as_secs_f64()
            );
            app.exit(0);
        }
    });
}

#[derive(Default, Debug, PartialEq)]
pub struct Options {
    pub platform_build: Option<String>,
    pub source: Option<String>,
    pub lang: Option<String>,
    pub names: Option<String>,
    pub smoke: bool,
    pub memory_limit: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut positional = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--lang" || arg == "--names" || arg == "--memory-limit" {
            positional.push(arg);
            if let Some(value) = args.next() {
                positional.push(value);
            }
            continue;
        }
        let build = if arg == "--platform-build" {
            Some(args.next().ok_or("--platform-build needs a value")?.clone())
        } else {
            arg.strip_prefix("--platform-build=").map(str::to_string)
        };
        if let Some(build) = build {
            if o.platform_build.replace(build).is_some() {
                return Err("--platform-build cannot be used multiple times".into());
            }
        } else {
            positional.push(arg);
        }
    }
    let mut it = positional.iter().copied();
    if positional.first().is_some_and(|arg| arg.as_str() == "open") {
        it.next();
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--smoke" => o.smoke = true,
            "--lang" => o.lang = Some(it.next().ok_or("--lang needs a value")?.clone()),
            "--names" => o.names = Some(it.next().ok_or("--names needs a value")?.clone()),
            "--memory-limit" => {
                o.memory_limit = Some(it.next().ok_or("--memory-limit needs a value")?.clone())
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if o.source.is_none() => o.source = Some(s.to_string()),
            s => return Err(format!("unexpected argument {s}")),
        }
    }
    if o.smoke && o.source.is_none() {
        return Err("--smoke needs a source".into());
    }
    Ok(o)
}

pub fn run_from_args(args: Vec<String>) -> i32 {
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "yellowcode: {e}\nUsage: yellowcode [open] [<source>] [--lang ru|en] [--names <dump>] [--platform-build <BUILD>] [--memory-limit <size>] [--smoke]"
            );
            return 2;
        }
    };
    let lang = match opts.lang.as_deref() {
        None | Some("ru") => Lang::Ru,
        Some("en") => Lang::En,
        Some(other) => {
            eprintln!("yellowcode: --lang: ru or en expected, got {other}");
            return 2;
        }
    };
    let cli_memory = match &opts.memory_limit {
        Some(limit) => {
            match yellowcode_core::viewer::source::OpenMemory::resolve(Some(limit.clone())) {
                Ok(memory) => Some(memory),
                Err(error) => {
                    eprintln!("yellowcode: {error}");
                    return if yellowcode_core::service::memory::is_config_error(&*error) {
                        2
                    } else {
                        1
                    };
                }
            }
        }
        None => None,
    };
    let process = match Process::unlimited() {
        Ok(process) => process,
        Err(error) => {
            eprintln!("yellowcode: {error}");
            return 1;
        }
    };
    let build = opts
        .platform_build
        .as_deref()
        .unwrap_or(yellowcode_core::platform::DEFAULT_BUILD);
    let selected = {
        let root = process.budget().root_budget();
        yellowcode_core::platform::DiscoverySnapshot::capture(&root).and_then(|snapshot| {
            let selected = yellowcode_core::platform::select_from_roots_with_memory(
                build,
                snapshot.roots(),
                &root,
            );
            drop(snapshot);
            selected
        })
    };
    if let Err(error) = selected {
        eprintln!(
            "yellowcode: {}",
            yellowcode_core::platform::managed_message(build, &error)
        );
        return 2;
    }
    match run(opts, lang, process, cli_memory) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("yellowcode: {e}");
            1
        }
    }
}

fn skin_root(app: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()?
        .parent()
        .unwrap_or(Path::new("."))
        .join("Yellowcode")
        .join("skins"))
}

fn run(
    opts: Options,
    lang: Lang,
    process: Process,
    cli_memory: Option<yellowcode_core::viewer::source::OpenMemory>,
) -> tauri::Result<()> {
    process_start();
    let smoke = opts.smoke;
    let source = opts.source.clone();
    let names = opts.names.map(PathBuf::from);
    let url = if cfg!(windows) {
        "http://onec.localhost/"
    } else {
        "onec://localhost/"
    };
    let app = tauri::Builder::default()
        .manage(convert::Jobs::default())
        .on_window_event(|window, event| {
            if window.label() == "main"
                && window.state::<convert::Jobs>().running()
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("onec", |ctx, request, responder| {
            serve_scheme(ctx.app_handle().clone(), request, responder);
        })
        .invoke_handler(tauri::generate_handler![
            convert::convert_source_info,
            convert::conversion_status,
            convert::start_conversion,
            convert::pick_conversion_output,
            pick_source,
            save_source,
            preview_source_memory,
            startup_language,
            startup_version,
            startup_dialog_size,
            dismiss_load_error,
            quit,
            restart_application,
            open_source,
            recent,
            sources,
            forget_recent,
            status,
            cancel_load,
            skins::pick_skin,
            skins::install_skin,
            skins::download_classic_skin,
            skins::select_skin,
            skins::list_skins
        ])
        .setup(move |app| {
            let dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("onec-viewer"));
            let recent_file = dir.join("recent.json");
            let recent = Recent::load_unchecked(&recent_file);
            let mut state =
                AppState::with_process(recent, recent_file, lang, names, smoke, Ok(process));
            state.cli_memory = cli_memory;
            state.skins = Arc::new(Mutex::new(yellowcode_core::viewer::skin::Skins::installed(
                &skin_root(app.handle())?,
            )));
            app.manage(state);
            let parsed = url.parse().map_err(|e| format!("{e}"))?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::CustomProtocol(parsed))
                .title(match lang {
                    Lang::Ru => "Запуск Yellowcode",
                    Lang::En => "Start Yellowcode",
                })
                .inner_size(820.0, 500.0)
                .min_inner_size(600.0, 420.0)
                .build()?;
            if smoke {
                std::thread::spawn(|| {
                    std::thread::sleep(smoke_timeout());
                    eprintln!("yellowcode: smoke timeout");
                    std::process::exit(1);
                });
            }
            if let Some(path) = source
                && let Err(e) = start_load(app.handle(), path)
            {
                eprintln!("yellowcode: {e}");
            }
            Ok(())
        })
        .build(tauri::generate_context!())?;
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { ref api, .. } = event
            && handle.state::<convert::Jobs>().running()
        {
            api.prevent_exit();
        } else if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            let state = handle.state::<AppState>();
            if let Some(viewer) = state
                .current
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                viewer.shutdown();
            }
        }
    });
    Ok(())
}
