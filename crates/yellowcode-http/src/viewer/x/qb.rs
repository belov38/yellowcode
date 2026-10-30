use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
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
    list: VecDeque<(u64, Arc<Mutex<Kept>>)>,
}

struct Kept {
    session: Session,
    leases: Vec<memory_budget::Reservation>,
    shape_bytes: u64,
}

#[derive(Default)]
struct Slot {
    schema: OnceLock<SchemaResult>,
    sessions: Mutex<Sessions>,
    schema_lease: Mutex<Option<memory_budget::Reservation>>,
}

fn slot(state: &State) -> Result<Arc<Slot>, Response> {
    if let Some(memory) = &state.session {
        return memory
            .extension::<Slot>()
            .map_err(|_| super::super::memory::refusal());
    }
    static TABLE: OnceLock<Mutex<HashMap<PathBuf, Arc<Slot>>>> = OnceLock::new();
    let t = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
    Ok(Arc::clone(
        lock(t)
            .entry(state.source.clone())
            .or_insert_with(|| Arc::new(Slot::default())),
    ))
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
    fn schema(&self, state: &State) -> Result<&SchemaResult, Response> {
        if let Some(memory) = &state.session {
            super::super::memory::hold_workspace(state.response_working_bytes().saturating_mul(2))?;
            let mut lease = lock(&self.schema_lease);
            if lease.is_none() {
                *lease = Some(
                    memory
                        .budget()
                        .try_reserve(
                            memory_budget::Category::Metadata,
                            state.response_working_bytes().saturating_mul(2),
                        )
                        .map_err(|_| super::super::memory::refusal())?,
                );
            }
        }
        Ok(self.schema.get_or_init(|| {
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
        }))
    }

    fn insert(
        &self,
        s: Session,
        shape_bytes: u64,
        lease: Option<memory_budget::Reservation>,
    ) -> u64 {
        let mut g = lock(&self.sessions);
        g.next += 1;
        let id = g.next;
        g.list.push_back((
            id,
            Arc::new(Mutex::new(Kept {
                session: s,
                shape_bytes,
                leases: lease.into_iter().collect(),
            })),
        ));
        while g.list.len() > MAX_SESSIONS {
            g.list.pop_front();
        }
        id
    }

    fn get(&self, id: u64) -> Option<Arc<Mutex<Kept>>> {
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

fn admit_model(input: u64) -> Result<Option<memory_budget::Reservation>, Response> {
    let Some(memory) = super::super::memory::current() else {
        return Ok(None);
    };
    memory
        .budget()
        .try_reserve(
            memory_budget::Category::Metadata,
            (256_u64 << 10).saturating_add(input.saturating_mul(128)),
        )
        .map(Some)
        .map_err(|_| super::super::memory::refusal())
}

fn with_session(
    slot: &Slot,
    id: u64,
    growth: Option<u64>,
    f: impl FnOnce(&mut Session) -> Response,
) -> Response {
    let s = match session_of(slot, id) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mut g = lock(&s);
    let bytes = if let Some(growth) = growth {
        g.shape_bytes.saturating_add(growth).saturating_mul(2)
    } else {
        g.shape_bytes
    };
    if let Err(response) = super::super::memory::hold_workspace(
        (256_u64 << 10).saturating_add(bytes.saturating_mul(256)),
    ) {
        return response;
    }
    if growth.is_some() {
        match admit_model(bytes) {
            Ok(Some(lease)) => g.leases.push(lease),
            Ok(None) => {}
            Err(response) => return response,
        }
    }
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let response = f(&mut g.session);
        g.shape_bytes = g.shape_bytes.max(g.session.text().len() as u64);
        let remove = g
            .leases
            .len()
            .saturating_sub(g.session.undo_depth().saturating_add(1));
        drop(g.leases.drain(..remove));
        response
    }));
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
    #[cfg(test)]
    cancellation_hook("before-slot");
    let slot = match slot(state) {
        Ok(slot) => slot,
        Err(response) => return response,
    };
    if state.session.as_ref().is_some_and(|m| m.is_cancelled()) {
        return super::super::memory::refusal();
    }
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

fn session_of(slot: &Slot, id: u64) -> Result<Arc<Mutex<Kept>>, Response> {
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
    let schema = match slot.schema(state) {
        Ok(schema) => schema,
        Err(response) => return response,
    };
    let qs = match schema {
        Ok(q) => Arc::clone(q),
        Err(e) => return out(e),
    };
    let lease = match admit_model(b.text.len() as u64) {
        Ok(lease) => lease,
        Err(response) => return response,
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
    #[cfg(test)]
    cancellation_hook("before-publish");
    if state.session.as_ref().is_some_and(|m| m.is_cancelled()) {
        return super::super::memory::refusal();
    }
    let id = slot.insert(s, text.len() as u64, lease);
    reply(200, &json!({ "id": id, "text": text, "view": view }))
}

fn apply(slot: &Slot, req: &Request) -> Response {
    let b: ApplyBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    with_session(slot, b.id, Some(req.body.len() as u64), |g| {
        match g.apply(&b.op) {
            Ok(()) => state_json(g),
            Err(e) => out(&e),
        }
    })
}

fn set_text(slot: &Slot, req: &Request) -> Response {
    let b: TextBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    with_session(slot, b.id, Some(req.body.len() as u64), |g| {
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
    with_session(slot, b.id, None, |g| {
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
    with_session(slot, id, None, |g| match g.view_nested(&q, op) {
        Ok(v) => reply(200, &v),
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
    with_session(slot, id, None, |g| match g.available(q.stmt, op) {
        Ok(a) => reply(200, &a),
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
    with_session(slot, id, None, |g| match g.table(table, stmt) {
        Some(t) => reply(200, &t),
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
    let source = m.source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&m.source);
    let scanned = source.len().min(x_qbuilder::MAX_MODULE);
    if let Err(response) = super::super::memory::hold_workspace(
        (2_u64 << 20).saturating_add((scanned as u64).saturating_mul(160)),
    ) {
        return response;
    }
    if state.session.as_ref().is_some_and(|m| m.is_cancelled()) {
        return super::super::memory::refusal();
    }
    #[cfg(test)]
    cancellation_hook("before-literal-scan");
    if state.session.as_ref().is_some_and(|m| m.is_cancelled()) {
        return super::super::memory::refusal();
    }
    let Some(mut bounded) = source.get(..scanned) else {
        return error(500, "internal error: invalid literal scan boundary");
    };
    if let Err(invalid_utf8) = std::str::from_utf8(bounded)
        && invalid_utf8.error_len().is_none()
    {
        let Some(prefix) = bounded.get(..invalid_utf8.valid_up_to()) else {
            return error(500, "internal error: invalid UTF-8 scan boundary");
        };
        bounded = prefix;
    }
    let text = String::from_utf8_lossy(bounded);
    let mut answer = yellowcode_core::viewer::literals::literals(&text);
    if (source.len() > scanned || text.len() > x_qbuilder::MAX_MODULE)
        && let Some(truncated) = answer.get_mut("truncated")
    {
        *truncated = Value::Bool(true);
    }
    reply(200, &answer)
}

#[cfg(test)]
thread_local! {
    static CANCELLATION_HOOK: std::cell::RefCell<CancellationHook> = const { std::cell::RefCell::new(None) };
}
