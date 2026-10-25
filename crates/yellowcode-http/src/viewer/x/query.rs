use std::sync::Arc;

use query_console::{Console, EXPLAIN_BUSY, ErrorOut, ExplainFail, MAX_CHOICES, RunRequest};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::http::{Request, Response};
use super::super::state::State;
use super::{Availability, ConsoleSlot, NotBase, error, infobase_source, param, reply, slot};

const DEFAULT_ROWS: usize = 200;
const MAX_PAGE: usize = 1000;

#[derive(Deserialize)]
struct TextBody {
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
}

pub fn handle(state: &State, req: &Request, route: &str) -> Response {
    let (verb, known) = match route {
        "info" | "job" | "rows" | "nested" | "choices" => (Verb::Get, true),
        "parameters" | "run" | "explain" | "cancel" | "reopen" => (Verb::Post, true),
        _ => (Verb::Get, false),
    };
    if !known {
        return error(404, "no such endpoint");
    }
    let is_post = req.method == "POST";
    if is_post != (verb == Verb::Post) {
        return error(405, "method not allowed");
    }
    if route == "info" {
        return info(state);
    }
    if let Err(reason) = source_state(state) {
        return error(409, &reason);
    }
    let slot = slot(&state.source);
    if route == "reopen" {
        return match slot.reopen(&state.source) {
            Availability::Ready(c) => reply(200, &info_json(&c)),
            Availability::Failed(e) => reply(200, &failed_json(state, &e)),
        };
    }
    let console = match slot.get(&state.source) {
        Availability::Ready(c) => c,
        Availability::Failed(e) => return error(409, &format!("база не открыта: {e}")),
    };
    match route {
        "parameters" => parameters(&console, req),
        "run" => run(&slot, console, req),
        "job" => job(&slot, req),
        "rows" => rows(&slot, &console, req),
        "nested" => nested(&slot, &console, req),
        "explain" => explain(&console, req),
        "cancel" => cancel(&slot, req),
        "choices" => choices(&console, req),
        _ => error(404, "no such endpoint"),
    }
}

fn source_state(state: &State) -> Result<(), String> {
    infobase_source(state).map_err(|kind| match kind {
        NotBase::Cf => "Консоль запросов работает на базе: откройте файл 1Cv8.1CD (сейчас открыта конфигурация из файла .cf)"
            .to_string(),
        NotBase::Dt => "DT открыт только для просмотра метаданных CONFIG; консоль запросов требует восстановленную базу 1Cv8.1CD".to_string(),
        NotBase::XmlDir => "Консоль запросов работает на базе: откройте файл 1Cv8.1CD (сейчас открыт каталог выгрузки)"
            .to_string(),
        NotBase::Unknown(e) => format!("Источник не определён: {e}"),
    })
}

fn info_json(c: &Console) -> Value {
    let mut v = serde_json::to_value(c.info()).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("enabled".into(), Value::Bool(true));
    }
    v
}

fn failed_json(state: &State, e: &str) -> Value {
    json!({
        "enabled": true,
        "path": state.source.display().to_string(),
        "error": e,
    })
}

fn info(state: &State) -> Response {
    if let Err(reason) = source_state(state) {
        return reply(
            200,
            &json!({
                "enabled": false,
                "path": state.source.display().to_string(),
                "reason": reason,
            }),
        );
    }
    match slot(&state.source).get(&state.source) {
        Availability::Ready(c) => reply(200, &info_json(&c)),
        Availability::Failed(e) => reply(200, &failed_json(state, &e)),
    }
}

fn query_error(e: &ErrorOut) -> Response {
    reply(200, &serde_json::to_value(e).unwrap_or(Value::Null))
}

fn body<T: for<'de> Deserialize<'de>>(req: &Request) -> Result<T, Response> {
    serde_json::from_slice(&req.body).map_err(|e| error(400, &format!("тело запроса: {e}")))
}

fn id_of(req: &Request) -> Result<u64, Response> {
    param(req, "id")
        .ok_or_else(|| error(400, "id is required"))?
        .parse::<u64>()
        .map_err(|_| error(400, "id: not a number"))
}

fn count(req: &Request, name: &str, default: usize) -> Result<usize, Response> {
    match param(req, name) {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| error(400, &format!("{name}: not a number"))),
    }
}

fn parameters(console: &Console, req: &Request) -> Response {
    let b: TextBody = match body(req) {
        Ok(b) => b,
        Err(r) => return r,
    };
    match console.parameters(&b.text) {
        Ok(p) => reply(200, &serde_json::to_value(p).unwrap_or(Value::Null)),
        Err(e) => query_error(&e),
    }
}

fn run(slot: &ConsoleSlot, console: Arc<Console>, req: &Request) -> Response {
    let r: RunRequest = match body(req) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match slot.jobs.start(console, r) {
        Ok(id) => reply(200, &json!({ "id": id })),
        Err(e) => error(503, &e),
    }
}

fn job(slot: &ConsoleSlot, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match slot.jobs.status(id) {
        Some(s) => reply(200, &serde_json::to_value(s).unwrap_or(Value::Null)),
        None => error(404, "no such job"),
    }
}

fn job_failure(e: &str) -> Response {
    let code = match e {
        "no such job" | "no such statement" => 404,
        _ => 409,
    };
    error(code, e)
}

fn rows(slot: &ConsoleSlot, console: &Console, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let (stmt, from, limit) = match (
        count(req, "stmt", 0),
        count(req, "from", 0),
        count(req, "limit", DEFAULT_ROWS),
    ) {
        (Ok(s), Ok(f), Ok(l)) => (s, f, l),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    if limit > MAX_PAGE {
        return error(400, &format!("limit: at most {MAX_PAGE}"));
    }
    match slot.jobs.page(console, id, stmt, from, limit) {
        Ok(p) => reply(200, &serde_json::to_value(p).unwrap_or(Value::Null)),
        Err(e) => job_failure(&e),
    }
}

fn nested(slot: &ConsoleSlot, console: &Console, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let (stmt, row, col) = match (
        count(req, "stmt", 0),
        count(req, "row", 0),
        count(req, "col", 0),
    ) {
        (Ok(s), Ok(r), Ok(c)) => (s, r, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    match slot.jobs.nested(console, id, stmt, row, col) {
        Ok(n) => reply(200, &serde_json::to_value(n).unwrap_or(Value::Null)),
        Err(e) => query_error(&e),
    }
}

fn explain(console: &Console, req: &Request) -> Response {
    let r: RunRequest = match body(req) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match console.explain_guarded(&r) {
        Ok(x) => reply(200, &serde_json::to_value(x).unwrap_or(Value::Null)),
        Err(ExplainFail::Busy) => error(429, EXPLAIN_BUSY),
        Err(ExplainFail::Query(e)) => query_error(&e),
    }
}

fn cancel(slot: &ConsoleSlot, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    reply(200, &json!({ "ok": slot.jobs.cancel(id) }))
}

fn choices(console: &Console, req: &Request) -> Response {
    let Some(kind) = param(req, "kind").filter(|k| !k.is_empty()) else {
        return error(400, "kind is required");
    };
    let limit = match count(req, "limit", MAX_CHOICES) {
        Ok(l) => l.min(MAX_CHOICES),
        Err(e) => return e,
    };
    reply(
        200,
        &serde_json::to_value(console.ref_choices(kind, limit)).unwrap_or(Value::Null),
    )
}
