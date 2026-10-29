pub mod api;
pub mod api_ext;
pub mod form;
pub mod front;
pub mod managed;
pub mod skin;
mod users;
mod users_control;
pub mod x;

pub(crate) use crate::transport as http;
use crate::{DeadlineReader, close_gracefully};
pub(crate) use yellowcode_core::viewer::state;

use std::io::BufReader;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{DRAIN, guard, text};
use http::{HttpError, MAX_BODY, Request, Response, read_request_with_body_limit, write_response};
use state::{State, Viewer};
use yellowcode_core::Result;
use yellowcode_core::viewer::body::ViewerBody;

pub fn accepts_post(path: &str) -> bool {
    path.starts_with("/api/x/") || path == "/api/skins/select" || path == "/api/users/apply"
}

pub fn is_api(path: &str) -> bool {
    path.starts_with("/api/")
}

pub fn read_request(r: &mut dyn std::io::BufRead) -> std::result::Result<Request, HttpError> {
    read_request_with_body_limit(r, &accepts_post, &|path| {
        if path == "/api/users/apply" {
            64 * 1024
        } else {
            MAX_BODY
        }
    })
}

const MAX_CONNECTIONS: usize = 16;
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server<V = Arc<Viewer>> {
    pub addr: SocketAddr,
    listener: TcpListener,
    viewer: V,
    stop: AtomicBool,
    connections: Arc<Connections>,
    read_timeout: Duration,
}

pub fn bind_listener(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("127.0.0.1:{port}: {e}"))?)
}

#[derive(Default)]
struct Connections {
    counts: Mutex<ConnectionCounts>,
    changed: Condvar,
}

#[derive(Default)]
struct ConnectionCounts {
    active: usize,
    draining: usize,
    sockets: [Option<TcpStream>; MAX_CONNECTIONS],
}

struct Slot {
    connections: Arc<Connections>,
    draining: bool,
    index: Option<usize>,
}

impl Connections {
    fn acquire(self: &Arc<Self>) -> Option<Slot> {
        self.acquire_locked(
            self.counts
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    fn acquire_locked(
        self: &Arc<Self>,
        mut counts: MutexGuard<'_, ConnectionCounts>,
    ) -> Option<Slot> {
        while counts.active >= MAX_CONNECTIONS {
            if counts.draining == 0 {
                return None;
            }
            counts = self
                .changed
                .wait(counts)
                .unwrap_or_else(|error| error.into_inner());
        }
        counts.active += 1;
        Some(Slot {
            connections: self.clone(),
            draining: false,
            index: None,
        })
    }
}

impl Slot {
    fn begin_drain(&mut self) {
        let mut counts = self
            .connections
            .counts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        counts.draining += 1;
        self.draining = true;
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut counts = self
            .connections
            .counts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = self.index
            && let Some(socket) = counts.sockets.get_mut(index)
        {
            *socket = None;
        }
        counts.active -= 1;
        if self.draining {
            counts.draining -= 1;
        }
        self.connections.changed.notify_one();
    }
}

impl Server {
    pub fn bind(state: Arc<State>, port: u16) -> Result<Server> {
        Server::from_listener(state, bind_listener(port)?)
    }

    pub fn from_listener(state: Arc<State>, listener: TcpListener) -> Result<Server> {
        Server::from_listener_viewer(Arc::new(Viewer::from_arc(state)), listener)
    }

    pub fn bind_viewer(viewer: Arc<Viewer>, port: u16) -> Result<Server> {
        Server::from_listener_viewer(viewer, bind_listener(port)?)
    }

    pub fn from_listener_viewer(viewer: Arc<Viewer>, listener: TcpListener) -> Result<Server> {
        Server::with_backend(viewer, listener)
    }

    pub fn bind_managed<S: mdconf::ManagedEntrySource + Send + Sync>(
        snapshot: yellowcode_core::viewer::snapshot::ManagedSnapshot<S>,
        source: &str,
        wire_limit: u64,
        port: u16,
    ) -> Result<Server<memory_budget::Shared<managed::Endpoint<S>>>>
    where
        S::Error: std::fmt::Display,
    {
        let endpoint = managed::Endpoint::new(snapshot, source, wire_limit)?;
        Server::with_backend(endpoint, bind_listener(port)?)
    }
    pub fn bind_managed_with_analysis<S: mdconf::ManagedEntrySource + Send + Sync>(
        snapshot: yellowcode_core::viewer::snapshot::ManagedSnapshot<S>,
        source: &str,
        wire_limit: u64,
        port: u16,
        paths: managed::AnalysisPaths<'_>,
    ) -> Result<Server<memory_budget::Shared<managed::Endpoint<S>>>>
    where
        S::Error: std::fmt::Display,
    {
        let endpoint = managed::Endpoint::with_native(snapshot, source, wire_limit, Some(paths))?;
        Server::with_backend(endpoint, bind_listener(port)?)
    }
}

mod sealed {
    pub trait Backend {}
}

pub trait HttpDispatch: sealed::Backend + Sync {
    fn shutdown(&self) {}
    fn response(
        &self,
        req: &Request,
        control: &managed::RequestControl<'_>,
    ) -> std::result::Result<Response<ViewerBody>, managed::ManagedHttpError>;
}
impl sealed::Backend for Arc<Viewer> {}
impl HttpDispatch for Arc<Viewer> {
    fn shutdown(&self) {
        Viewer::shutdown(self);
    }
    fn response(
        &self,
        req: &Request,
        _control: &managed::RequestControl<'_>,
    ) -> std::result::Result<Response<ViewerBody>, managed::ManagedHttpError> {
        Ok(managed::eager(dispatch(self, req)))
    }
}

impl<V: HttpDispatch> Server<V> {
    fn with_backend(viewer: V, listener: TcpListener) -> Result<Self> {
        let addr = listener.local_addr()?;
        Ok(Server {
            addr,
            listener,
            viewer,
            stop: AtomicBool::new(false),
            connections: Arc::default(),
            read_timeout: DEFAULT_READ_TIMEOUT,
        })
    }

    pub fn set_read_timeout(&mut self, d: Duration) {
        self.read_timeout = d;
    }

    pub fn run(&self) -> Result<()> {
        std::thread::scope(|scope| {
            loop {
                let (stream, _) = match self.listener.accept() {
                    Ok(c) => c,
                    Err(e) => {
                        if self.stop.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        if e.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                };
                if self.stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let Some(mut slot) = self.connections.acquire() else {
                    let _ = stream.set_nonblocking(true);
                    let mut writer = crate::conn::DeadlineWriter {
                        stream: &stream,
                        deadline: Instant::now() + DRAIN,
                        wire_bytes: 0,
                    };
                    let _ = write_response(&mut writer, &text(503, "error\n"), false);
                    close_gracefully(&stream);
                    continue;
                };
                {
                    let mut counts = self
                        .connections
                        .counts
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if self.stop.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    let Some((index, entry)) = counts
                        .sockets
                        .iter_mut()
                        .enumerate()
                        .find(|(_, socket)| socket.is_none())
                    else {
                        continue;
                    };
                    match stream.try_clone() {
                        Ok(socket) => {
                            *entry = Some(socket);
                            slot.index = Some(index);
                        }
                        Err(_) => continue,
                    }
                }
                let spawned = std::thread::Builder::new().spawn_scoped(scope, move || {
                    self.serve_connection(&stream, &mut slot);
                });
                if spawned.is_err() {
                    continue;
                }
            }
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.viewer.shutdown();
        let connections = self
            .connections
            .counts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for stream in connections.sockets.iter().flatten() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        drop(connections);
        let _ = TcpStream::connect(self.addr);
    }

    fn serve_connection(&self, stream: &TcpStream, slot: &mut Slot) {
        let deadline = Instant::now() + self.read_timeout;
        let mut reader = BufReader::new(DeadlineReader { stream, deadline });
        match read_request(&mut reader) {
            Ok(req) => {
                if stream.set_nonblocking(true).is_err() {
                    return;
                }
                let canceled = AtomicBool::new(false);
                let control = managed::RequestControl::with_shutdown(
                    &canceled,
                    &self.stop,
                    Some(deadline),
                    stream,
                );
                let resp = match guard(&req, self.addr.port(), &is_api) {
                    Some(response) => Ok(managed::eager(response)),
                    None => self.viewer.response(&req, &control),
                };
                let mut w = crate::conn::DeadlineWriter {
                    stream,
                    deadline,
                    wire_bytes: 0,
                };
                match resp {
                    Ok(response) => {
                        let _ = managed::send(&mut w, response, req.method == "HEAD", &control);
                    }
                    Err(managed::ManagedHttpError::Memory(_)) => {
                        let _ = managed::send(
                            &mut w,
                            managed::memory_refusal(),
                            req.method == "HEAD",
                            &control,
                        );
                    }
                    Err(_) => {}
                }
                slot.begin_drain();
                close_gracefully(stream);
            }
            Err(e) => {
                if let Some(code) = e.status() {
                    let _ = respond_status(stream, code);
                    slot.begin_drain();
                    close_gracefully(stream);
                }
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn respond_status(stream: &TcpStream, status: u16) -> std::io::Result<()> {
    let mut w = stream;
    write_response(&mut w, &text(status, "error\n"), false)
}

pub fn handle(state: &State, req: &Request) -> Response {
    guarded(|| {
        let Ok(_read) = state.users_control.admission.read() else {
            return users_control::busy();
        };
        if state.users_control.closed() {
            return users_control::source_closed();
        }
        if users_control::is_write_route(req) {
            return users_control::without_viewer(state, req);
        }
        route(state, req)
    })
}

fn guarded(f: impl FnOnce() -> Response) -> Response {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".to_string());
            Response {
                status: 500,
                content_type: "application/json; charset=utf-8",
                body: serde_json::to_vec(&serde_json::json!({
                    "error": format!("internal error: {msg}")
                }))
                .unwrap_or_default(),
                ..Default::default()
            }
        }
    }
}

const WAIT: Duration = Duration::from_secs(1);

pub fn dispatch(viewer: &Viewer, req: &Request) -> Response {
    guarded(|| {
        if users_control::is_write_route(req) {
            return users_control::handle(viewer, req);
        }
        let control = viewer.snapshot().users_control.clone();
        let Ok(_read) = control.admission.read() else {
            return users_control::busy();
        };
        if control.closed() {
            return users_control::source_closed();
        }
        let snap = viewer.snapshot();
        let Some(need) = need(req, &snap) else {
            return route(&snap, req);
        };
        drop(snap);
        match viewer.with(need, WAIT) {
            Ok(st) => route(&st, req),
            Err(u) => unavailable(req, u),
        }
    })
}

#[cfg(test)]
thread_local! {
    static PANIC_IN_NEED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn param<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

const FORM_WINDOW: mdconf::Stages = mdconf::Stages::FORMS.union(mdconf::Stages::PICTURES);

fn need(req: &Request, st: &State) -> Option<state::Need> {
    use mdconf::{MdKind, Stages, SubKind};
    use state::Need;
    use yellowcode_core::viewer::path::{self, Target};
    #[cfg(test)]
    if PANIC_IN_NEED.with(std::cell::Cell::get) {
        panic!("need: test panic");
    }
    if st.stages == Stages::ALL {
        return None;
    }
    let missing = |s: Stages| (!st.stages.contains(s)).then_some(Need::Stages(s));
    let target = || {
        param(req, "path")
            .filter(|p| !p.is_empty())
            .and_then(|p| path::resolve(&st.cfg, p).ok())
    };
    let route = match req.path.as_str() {
        p if p.starts_with("/res/fpic/") => "/res/fpic",
        p if p.starts_with("/res/cpic/") => "/res/cpic",
        p => p,
    };
    match route {
        "/api/form-structure" | "/api/form" | "/form" | "/res/fpic" => {
            if route != "/api/form-structure" && param(req, "generated").is_some() {
                return None;
            }
            match target()? {
                Target::Sub { sub, .. } if sub.kind == SubKind::Form => missing(FORM_WINDOW),
                Target::Object(o) if o.kind == MdKind::CommonForm => missing(FORM_WINDOW),
                _ => None,
            }
        }
        "/res/cpic" => missing(Stages::PICTURES),
        "/api/template" => match target()? {
            Target::Sub { sub, .. } if sub.kind == SubKind::Template => missing(Stages::TEMPLATES),
            Target::Object(o) if o.kind == MdKind::CommonTemplate => missing(Stages::TEMPLATES),
            _ => None,
        },
        "/api/check" => {
            let t = target()?;
            let which = param(req, "module")
                .map(str::to_string)
                .or_else(|| api::module_names(&t).into_iter().next());
            let m =
                yellowcode_core::viewer::module::module_of(&st.cfg, &t, which.as_deref()).ok()?;
            yellowcode_core::viewer::check::items(&st.cfg)
                .into_iter()
                .find(|i| std::ptr::eq(i.module, m))?;
            let known = st.names.is_some()
                || st
                    .check
                    .names
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some();
            (!known).then_some(Need::CheckNames)
        }
        _ => None,
    }
}

fn unavailable(req: &Request, u: state::Unavailable) -> Response {
    use state::Unavailable;
    let (status, body, refresh) = match u {
        Unavailable::Pending { what } => (
            503,
            serde_json::json!({
                "loading": what,
                "retry_ms": WAIT.as_millis() as u64,
                "error": format!("loading {what}…"),
            }),
            true,
        ),
        Unavailable::Changed => (
            409,
            serde_json::json!({ "error": "the source changed since it was opened; reopen it" }),
            false,
        ),
        Unavailable::Failed {
            what,
            error,
            retry_in,
        } => {
            let mut e = format!("loading {what} failed: {error}");
            if let Some(d) = retry_in {
                e.push_str(&format!("; retrying in {} s", d.as_secs().max(1)));
            }
            (503, serde_json::json!({ "error": e }), false)
        }
    };
    if req.path == "/form" {
        let text = body
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        return form::message_page(status, text, refresh);
    }
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(&body).unwrap_or_default(),
        ..Default::default()
    }
}

fn route(state: &State, req: &Request) -> Response {
    if let Some(r) = skin::handle(&state.skins, req) {
        return r;
    }
    if let Some(r) = form::handle(state, req) {
        return r;
    }
    if let Some(r) = api::handle(state, req) {
        return r;
    }
    if let Some(r) = front::handle(req) {
        return r;
    }
    match req.path.as_str() {
        "/favicon.ico" => Response {
            status: 204,
            content_type: "image/x-icon",
            body: Vec::new(),
            ..Default::default()
        },
        _ => text(404, "not found\n"),
    }
}
