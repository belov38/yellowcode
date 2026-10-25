use super::http::{Request, Response};
use serde_json::json;
use yellowcode_core::{
    users,
    viewer::{
        source::{self, SourceKind},
        state::State,
    },
};

fn reply(status: u16, body: serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(&body).unwrap_or_default(),
        ..Default::default()
    }
}
fn error(status: u16, msg: &str) -> Response {
    reply(status, json!({"error":msg}))
}

pub(super) fn handle(state: &State, req: &Request) -> Response {
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        return error(405, "users endpoints are read-only; use GET or HEAD");
    }
    if !req.query.is_empty() {
        return error(400, "users endpoints take no query parameters");
    }
    let id = if req.path == "/api/users" {
        None
    } else {
        let Some(id) = req.path.strip_prefix("/api/users/") else {
            return error(404, "no such endpoint");
        };
        if brace_id(id).is_none() {
            return error(400, "user ID must be a UUID");
        }
        Some(id)
    };
    match source::detect(&state.source) {
        Ok(SourceKind::Infobase) => {}
        Ok(_) => return error(409, "users are available only for an infobase"),
        Err(_) => return error(503, "the infobase source is unavailable"),
    }
    let cards = match users::list_with_configuration(&state.source, &state.cfg) {
        Ok(cards) => cards,
        Err(e) => return error(500, &e.to_string()),
    };
    match id {
        None => reply(200, json!(cards)),
        Some(id) => match users::find(&cards, id) {
            Ok(card) => reply(200, json!(card)),
            Err(users::Error::NotFound) => error(404, "no such infobase user"),
            Err(e) => error(500, &e.to_string()),
        },
    }
}

fn brace_id(id: &str) -> Option<()> {
    users::valid_id(id).then_some(())
}
