pub mod code;
pub mod data;
pub mod qb;
pub mod query;

pub use yellowcode_core::viewer::consoles::{
    Availability, ConsoleSlot, NotBase, infobase_source, lock, slot,
};

use serde_json::{Value, json};

use super::http::{Request, Response};
use super::state::State;

const JS: &str = "text/javascript; charset=utf-8";

const CSS: &str = "text/css; charset=utf-8";

pub const FILES: &[(&str, &str, &str)] = &[
    ("x.css", CSS, include_str!("../front/x.css")),
    ("js/x/code.js", JS, include_str!("../front/js/x/code.js")),
    (
        "js/x/code_analysis.js",
        JS,
        include_str!("../front/js/x/code_analysis.js"),
    ),
    (
        "js/x/code_catalog.js",
        JS,
        include_str!("../front/js/x/code_catalog.js"),
    ),
    (
        "js/x/code_context.js",
        JS,
        include_str!("../front/js/x/code_context.js"),
    ),
    (
        "js/x/code_completion.js",
        JS,
        include_str!("../front/js/x/code_completion.js"),
    ),
    ("js/x/data.js", JS, include_str!("../front/js/x/data.js")),
    (
        "js/x/data_card.js",
        JS,
        include_str!("../front/js/x/data_card.js"),
    ),
    (
        "js/x/data_grid.js",
        JS,
        include_str!("../front/js/x/data_grid.js"),
    ),
    (
        "js/x/data_moves.js",
        JS,
        include_str!("../front/js/x/data_moves.js"),
    ),
    ("js/x/index.js", JS, include_str!("../front/js/x/index.js")),
    ("js/x/net.js", JS, include_str!("../front/js/x/net.js")),
    ("js/x/qb.js", JS, include_str!("../front/js/x/qb.js")),
    (
        "js/x/qb_splitters.js",
        JS,
        include_str!("../front/js/x/qb_splitters.js"),
    ),
    (
        "js/x/qb_fields.js",
        JS,
        include_str!("../front/js/x/qb_fields.js"),
    ),
    (
        "js/x/qb_literals.js",
        JS,
        include_str!("../front/js/x/qb_literals.js"),
    ),
    (
        "js/x/qb_tabs.js",
        JS,
        include_str!("../front/js/x/qb_tabs.js"),
    ),
    ("js/x/query.js", JS, include_str!("../front/js/x/query.js")),
    (
        "js/x/query_params.js",
        JS,
        include_str!("../front/js/x/query_params.js"),
    ),
    ("js/x/table.js", JS, include_str!("../front/js/x/table.js")),
    (
        "js/x/waiting.js",
        JS,
        include_str!("../front/js/x/waiting.js"),
    ),
];

pub(crate) fn reply(status: u16, body: &Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(body).unwrap_or_else(|_| b"null".to_vec()),
        ..Default::default()
    }
}

pub(crate) fn error(status: u16, msg: &str) -> Response {
    reply(status, &json!({ "error": msg }))
}

pub(crate) fn param<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

pub fn handle(state: &State, req: &Request) -> Option<Response> {
    let run = || {
        let rest = req.path.strip_prefix("/api/x/").unwrap_or_default();
        match rest.split_once('/') {
            Some(("query", route)) => query::handle(state, req, route),
            Some(("code", route)) => code::handle(state, req, route),
            Some(("data", route)) => data::handle(state, req, route),
            Some(("qb", route)) => qb::handle(state, req, route),
            _ => error(404, "no such endpoint"),
        }
    };
    Some(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .unwrap_or_else(|_| error(500, "internal error")),
    )
}
