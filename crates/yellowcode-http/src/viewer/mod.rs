pub mod api;
pub mod api_ext;
pub mod form;
pub mod front;
pub mod skin;
mod users;
pub mod x;

pub(crate) use crate::transport as http;
use crate::{DeadlineReader, close_gracefully};
pub(crate) use yellowcode_core::viewer::state;

use std::io::BufReader;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::{DRAIN, guard, text};
use http::{HttpError, MAX_BODY, Request, Response, read_request_with, write_response};
use state::{State, Viewer};
use yellowcode_core::Result;

pub fn accepts_post(path: &str) -> bool {
    path.starts_with("/api/x/") || path == "/api/skins/select"
}

pub fn is_api(path: &str) -> bool {
    path.starts_with("/api/")
}

pub fn read_request(r: &mut dyn std::io::BufRead) -> std::result::Result<Request, HttpError> {
    read_request_with(r, &accepts_post, MAX_BODY)
}

const MAX_CONNECTIONS: usize = 16;
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server {
    pub addr: SocketAddr,
    listener: TcpListener,
    viewer: Arc<Viewer>,
    stop: AtomicBool,
    active: Arc<AtomicUsize>,
    read_timeout: Duration,
}

pub fn bind_listener(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("127.0.0.1:{port}: {e}"))?)
}

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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
        let addr = listener.local_addr()?;
        Ok(Server {
            addr,
            listener,
            viewer,
            stop: AtomicBool::new(false),
            active: Arc::new(AtomicUsize::new(0)),
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
                if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    self.active.fetch_sub(1, Ordering::SeqCst);
                    let _ = std::thread::Builder::new().spawn(move || {
                        let _ = stream.set_write_timeout(Some(DRAIN));
                        let _ = respond_status(&stream, 503);
                        close_gracefully(&stream);
                    });
                    continue;
                }
                let slot = Slot(self.active.clone());
                let spawned = std::thread::Builder::new().spawn_scoped(scope, move || {
                    let _slot = slot;
                    self.serve_connection(&stream);
                });
                if spawned.is_err() {
                    continue;
                }
            }
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }

    fn serve_connection(&self, stream: &TcpStream) {
        let _ = stream.set_write_timeout(Some(self.read_timeout));
        let mut reader = BufReader::new(DeadlineReader {
            stream,
            deadline: Instant::now() + self.read_timeout,
        });
        match read_request(&mut reader) {
            Ok(req) => {
                let resp = guard(&req, self.addr.port(), &is_api)
                    .unwrap_or_else(|| dispatch(&self.viewer, &req));
                let mut w = stream;
                let _ = write_response(&mut w, &resp, req.method == "HEAD");
                close_gracefully(stream);
            }
            Err(e) => {
                if let Some(code) = e.status() {
                    let _ = respond_status(stream, code);
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
    guarded(|| route(state, req))
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
