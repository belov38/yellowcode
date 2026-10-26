use std::path::Path;

use code_console::{BUSY, CodeConsole, CodeJobState, SessionMode};
use serde::Deserialize;
use serde_json::{Value, json};
use yellowcode_core::viewer::code_consoles::{CodeAvailability, CodeSlot, StartError, code_slot};

use super::super::http::{Request, Response};
use super::super::state::State;
use super::{NotBase, error, infobase_source, param, reply};

#[derive(Deserialize)]
struct RunBody {
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
}

pub fn handle(state: &State, req: &Request, route: &str) -> Response {
    let (verb, known) = match route {
        "info" | "job" => (Verb::Get, true),
        "run" | "cancel" | "reopen" => (Verb::Post, true),
        _ => (Verb::Get, false),
    };
    if !known {
        return error(404, "no such endpoint");
    }
    if (req.method == "POST") != (verb == Verb::Post) {
        return error(405, "method not allowed");
    }
    if route == "info" {
        return info(state);
    }
    if let Err(reason) = source_state(state) {
        return error(409, &reason);
    }
    let slot = code_slot(&state.source);
    if route == "reopen" {
        return match slot.reopen(&state.source) {
            Ok(CodeAvailability::Ready(c)) => reply(200, &ready_json(&c)),
            Ok(CodeAvailability::Failed(e)) => reply(200, &failed_json(&e)),
            Err(busy) => error(409, &busy),
        };
    }
    match route {
        "run" => run(&slot, state, req),
        "job" => job(&slot, req),
        "cancel" => cancel(&slot, req),
        _ => error(404, "no such endpoint"),
    }
}

fn source_state(state: &State) -> Result<(), String> {
    infobase_source(state).map_err(|kind| match kind {
        NotBase::Cf => "Консоль кода работает на базе: откройте файл 1Cv8.1CD (сейчас открыта конфигурация из файла .cf)"
            .to_string(),
        NotBase::Dt => "DT открыт только для просмотра метаданных CONFIG; консоль кода требует восстановленную базу 1Cv8.1CD".to_string(),
        NotBase::XmlDir => "Консоль кода работает на базе: откройте файл 1Cv8.1CD (сейчас открыт каталог выгрузки)"
            .to_string(),
        NotBase::Unknown(e) => format!("Источник не определён: {e}"),
    })
}

fn ready_json(c: &CodeConsole) -> Value {
    let info = c.info();
    let mut v = json!({
        "base": true,
        "ready": true,
        "rightsChecked": info.rights_checked,
    });
    if let Value::Object(m) = &mut v {
        match info.session {
            SessionMode::Full => {
                m.insert("session".into(), json!("full"));
            }
            SessionMode::Bare { reason } => {
                m.insert("session".into(), json!("bare"));
                m.insert("sessionReason".into(), json!(reason));
            }
        }
    }
    v
}

fn failed_json(e: &str) -> Value {
    json!({ "base": true, "ready": false, "error": e })
}

fn info(state: &State) -> Response {
    if let Err(reason) = source_state(state) {
        return reply(200, &json!({ "base": false, "reason": reason }));
    }
    reply(200, &open_json(&code_slot(&state.source), &state.source))
}

fn open_json(slot: &CodeSlot, source: &Path) -> Value {
    match slot.get(source) {
        CodeAvailability::Ready(c) => ready_json(&c),
        CodeAvailability::Failed(e) => failed_json(&e),
    }
}

fn id_of(req: &Request) -> Result<u64, Response> {
    param(req, "id")
        .ok_or_else(|| error(400, "id is required"))?
        .parse::<u64>()
        .map_err(|_| error(400, "id: not a number"))
}

fn run(slot: &CodeSlot, state: &State, req: &Request) -> Response {
    let b: RunBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(e) => return error(400, &format!("тело запроса: {e}")),
    };
    match slot.start(&state.source, b.text) {
        Ok(id) => reply(200, &json!({ "id": id })),
        Err(StartError::NotOpen(e)) => error(409, &format!("база не открыта: {e}")),
        Err(StartError::Refused(e)) if e == BUSY => error(409, &e),
        Err(StartError::Refused(e)) => error(503, &e),
    }
}

fn job(slot: &CodeSlot, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match slot.jobs.status(id) {
        Some(s) => reply(200, &serde_json::to_value(s).unwrap_or(Value::Null)),
        None => error(404, "no such job"),
    }
}

fn cancel(slot: &CodeSlot, req: &Request) -> Response {
    let id = match id_of(req) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let running = slot
        .jobs
        .status(id)
        .is_some_and(|s| s.state == CodeJobState::Running);
    reply(
        200,
        &json!({ "cancelled": running && slot.jobs.cancel(id) }),
    )
}
