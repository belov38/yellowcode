use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{Value, json};
use x_qbuilder::{ErrorOut, Op, ParseMode, QueryRef, QuerySchema, Session};

use super::super::http::{Request, Response};
use super::super::state::State;
use super::{error, lock, param, reply};
use yellowcode_core::viewer::{module, path};

pub const MAX_SESSIONS: usize = 8;

type SchemaResult = Result<Arc<QuerySchema>, ErrorOut>;

#[derive(Default)]
struct Sessions {
    next: u64,
    list: VecDeque<(u64, Arc<Mutex<Session>>)>,
}

struct Slot {
    schema: OnceLock<SchemaResult>,
    sessions: Mutex<Sessions>,
}

fn slot(source: &Path) -> Arc<Slot> {
    static TABLE: OnceLock<Mutex<HashMap<PathBuf, Arc<Slot>>>> = OnceLock::new();
    let t = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
    Arc::clone(lock(t).entry(source.to_path_buf()).or_insert_with(|| {
        Arc::new(Slot {
            schema: OnceLock::new(),
            sessions: Mutex::new(Sessions::default()),
        })
    }))
}

fn out(e: &ErrorOut) -> Response {
    reply(200, &json!({ "error": e }))
}

fn plain(text: String) -> ErrorOut {
    ErrorOut {
        chain: vec![text],
        at: None,
    }
}

impl Slot {
    fn schema(&self, state: &State) -> &SchemaResult {
        self.schema.get_or_init(|| {
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Session::new(&state.cfg).map(|s| Arc::clone(s.query_schema()))
            }));
            match built {
                Ok(Ok(qs)) => Ok(qs),
                Ok(Err(e)) => Err(plain(format!(
                    "Конфигурация недоступна для конструктора запросов: {}",
                    e.chain.join(" ")
                ))),
                Err(_) => Err(plain(
                    "Конфигурация недоступна для конструктора запросов: внутренняя ошибка".into(),
                )),
            }
        })
    }

    fn insert(&self, s: Session) -> u64 {
        let mut g = lock(&self.sessions);
        g.next += 1;
        let id = g.next;
        g.list.push_back((id, Arc::new(Mutex::new(s))));
        while g.list.len() > MAX_SESSIONS {
            g.list.pop_front();
        }
        id
    }

    fn get(&self, id: u64) -> Option<Arc<Mutex<Session>>> {
        lock(&self.sessions)
            .list
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, s)| Arc::clone(s))
    }

    fn remove(&self, id: u64) -> bool {
        let mut g = lock(&self.sessions);
        let before = g.list.len();
        g.list.retain(|(i, _)| *i != id);
        g.list.len() != before
    }
}

fn with_session(slot: &Slot, id: u64, f: impl FnOnce(&mut Session) -> Response) -> Response {
    let s = match session_of(slot, id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut g = lock(&s);
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut g)));
    drop(g);
    done.unwrap_or_else(|_| {
        slot.remove(id);
        error(500, "internal error: сеанс закрыт, откройте запрос заново")
    })
}

fn panic_hook(text: &str) {
    if cfg!(debug_assertions) && text == "\u{0}panic-hook" {
        std::panic::resume_unwind(Box::new("qb panic hook"));
    }
}

#[derive(Deserialize)]
struct OpenBody {
    #[serde(default)]
    text: String,
    mode: Mode,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Query,
    Dcs,
}

#[derive(Deserialize)]
struct IdBody {
    id: u64,
}

#[derive(Deserialize)]
struct ApplyBody {
    id: u64,
    op: Op,
}

#[derive(Deserialize)]
struct TextBody {
    id: u64,
    text: String,
}

pub fn handle(state: &State, req: &Request, route: &str) -> Response {
    let post = match route {
        "open" | "apply" | "set-text" | "undo" | "close" => true,
        "view" | "available" | "children" | "literals" => false,
        _ => return error(404, "no such endpoint"),
    };
    if (req.method == "POST") != post {
        return error(405, "method not allowed");
    }
    if route == "literals" {
        return literals(state, req);
    }
    let slot = slot(&state.source);
    match route {
        "open" => open(state, &slot, req),
        "apply" => apply(&slot, req),
        "set-text" => set_text(&slot, req),
        "undo" => undo(&slot, req),
        "close" => close(&slot, req),
        "view" => view(&slot, req),
        "available" => available(&slot, req),
        _ => children(&slot, req),
    }
}

fn body<T: for<'de> Deserialize<'de>>(req: &Request) -> Result<T, Response> {
    serde_json::from_slice(&req.body).map_err(|e| error(400, &format!("тело запроса: {e}")))
}

fn num(req: &Request, name: &str, default: usize) -> Result<usize, Response> {
    match param(req, name) {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| error(400, &format!("{name}: not a number"))),
    }
}

fn session_of(slot: &Slot, id: u64) -> Result<Arc<Mutex<Session>>, Response> {
    slot.get(id).ok_or_else(|| {
        error(
            404,
            "сеанс не найден (закрыт или вытеснен более новыми); откройте запрос заново",
        )
    })
}

fn query_id(req: &Request) -> Result<u64, Response> {
    param(req, "id")
        .ok_or_else(|| error(400, "id is required"))?
        .parse::<u64>()
        .map_err(|_| error(400, "id: not a number"))
}

fn state_json(s: &Session) -> Response {
    reply(200, &json!({ "text": s.text(), "view": s.view(0, 0).ok() }))
}

fn open(state: &State, slot: &Slot, req: &Request) -> Response {
    let b: OpenBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let qs = match slot.schema(state) {
        Ok(q) => Arc::clone(q),
        Err(e) => return out(e),
    };
    let mode = match b.mode {
        Mode::Query => ParseMode::Query,
        Mode::Dcs => ParseMode::Dcs,
    };
    let mut s = Session::empty(qs, mode);
    if !b.text.trim().is_empty()
        && let Err(e) = s.open(&b.text, mode)
    {
        return out(&e);
    }
    let (text, view) = (s.text(), s.view(0, 0).ok());
    let id = slot.insert(s);
    reply(200, &json!({ "id": id, "text": text, "view": view }))
}

fn apply(slot: &Slot, req: &Request) -> Response {
    let b: ApplyBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    with_session(slot, b.id, |g| match g.apply(&b.op) {
        Ok(()) => state_json(g),
        Err(e) => out(&e),
    })
}

fn set_text(slot: &Slot, req: &Request) -> Response {
    let b: TextBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    with_session(slot, b.id, |g| {
        panic_hook(&b.text);
        let mode = g.mode();
        match g.open(&b.text, mode) {
            Ok(()) => state_json(g),
            Err(e) => out(&e),
        }
    })
}

fn undo(slot: &Slot, req: &Request) -> Response {
    let b: IdBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    with_session(slot, b.id, |g| {
        let undone = g.undo();
        reply(
            200,
            &json!({ "text": g.text(), "view": g.view(0, 0).ok(), "undone": undone }),
        )
    })
}

fn close(slot: &Slot, req: &Request) -> Response {
    let b: IdBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    reply(200, &json!({ "ok": slot.remove(b.id) }))
}

fn position(req: &Request) -> Result<(QueryRef, usize), Response> {
    let stmt = num(req, "stmt", 0)?;
    let op = num(req, "op_index", 0)?;
    let nested = match param(req, "nested") {
        None | Some("") => Vec::new(),
        Some(v) => serde_json::from_str::<Vec<(usize, String)>>(v)
            .map_err(|e| error(400, &format!("nested: {e}")))?,
    };
    Ok((QueryRef { stmt, nested }, op))
}

fn view(slot: &Slot, req: &Request) -> Response {
    let id = match query_id(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let (q, op) = match position(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    with_session(slot, id, |g| match g.view_nested(&q, op) {
        Ok(v) => reply(200, &serde_json::to_value(&v).unwrap_or(Value::Null)),
        Err(e) => out(&e),
    })
}

fn available(slot: &Slot, req: &Request) -> Response {
    let id = match query_id(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let (q, op) = match position(req) {
        Ok(p) => p,
        Err(r) => return r,
    };
    with_session(slot, id, |g| match g.available(q.stmt, op) {
        Ok(a) => reply(200, &serde_json::to_value(&a).unwrap_or(Value::Null)),
        Err(e) => out(&e),
    })
}

fn children(slot: &Slot, req: &Request) -> Response {
    let id = match query_id(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let Some(table) = param(req, "table").filter(|t| !t.is_empty()) else {
        return error(400, "table is required");
    };
    let stmt = match num(req, "stmt", 0) {
        Ok(n) => n,
        Err(r) => return r,
    };
    with_session(slot, id, |g| match g.table(table, stmt) {
        Some(t) => reply(200, &serde_json::to_value(&t).unwrap_or(Value::Null)),
        None => error(404, "таблица не найдена"),
    })
}

fn literals(state: &State, req: &Request) -> Response {
    let Some(p) = param(req, "path") else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, p) {
        Ok(t) => t,
        Err(e) => return error(404, &e.to_string()),
    };
    let m = match module::module_of(&state.cfg, &target, param(req, "module")) {
        Ok(m) => m,
        Err(e) => return error(404, &e.to_string()),
    };
    reply(200, &yellowcode_core::viewer::literals::literals(&m.text()))
}
