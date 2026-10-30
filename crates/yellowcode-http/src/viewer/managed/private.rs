use super::{
    ManagedHttpError, error, response,
    selected::{json, message},
};
use crate::transport::{Request, Response};
use memory_budget::MemoryBudget;
use serde::Serialize;
use yellowcode_core::viewer::body::ViewerBody;
const QUERY: &str = "DT открыт только для просмотра метаданных CONFIG; консоль запросов требует восстановленную базу 1Cv8.1CD";
const DATA: &str = "DT открыт только для просмотра метаданных CONFIG; данные открываются на восстановленной базе 1Cv8.1CD";
const CODE: &str = "DT открыт только для просмотра метаданных CONFIG; консоль кода требует восстановленную базу 1Cv8.1CD";
#[derive(Serialize)]
struct QueryInfo<'a> {
    enabled: bool,
    path: &'a str,
    reason: &'static str,
}
#[derive(Serialize)]
struct BaseInfo {
    base: bool,
    reason: &'static str,
}
fn missing() -> Response<ViewerBody> {
    error(404, b"{\"error\":\"no such endpoint\"}")
}
fn method() -> Response<ViewerBody> {
    error(405, b"{\"error\":\"method not allowed\"}")
}
pub(super) fn handle(
    budget: &MemoryBudget,
    source: &str,
    req: &Request,
    limit: u64,
) -> Option<Result<Response<ViewerBody>, ManagedHttpError>> {
    if req.path == "/api/users" || req.path.starts_with("/api/users/") {
        return Some(Ok(if !matches!(req.method.as_str(), "GET" | "HEAD") {
            error(
                405,
                b"{\"error\":\"users endpoints are read-only; use GET or HEAD\"}",
            )
        } else if !req.query.is_empty() {
            error(
                400,
                b"{\"error\":\"users endpoints take no query parameters\"}",
            )
        } else if req
            .path
            .strip_prefix("/api/users/")
            .is_some_and(|id| !yellowcode_core::users::valid_id(id))
        {
            error(400, b"{\"error\":\"user ID must be a UUID\"}")
        } else {
            error(
                409,
                b"{\"error\":\"users are available only for an infobase\"}",
            )
        }));
    }
    let rest = req.path.strip_prefix("/api/x/")?;
    let (kind, route) = rest.split_once('/')?;
    let (post, reason) = match kind {
        "query" => match route {
            "info" | "job" | "rows" | "nested" | "choices" => (false, QUERY),
            "parameters" | "run" | "explain" | "cancel" | "reopen" => (true, QUERY),
            _ => return Some(Ok(missing())),
        },
        "code" => match route {
            "info" | "job" => (false, CODE),
            "run" | "cancel" | "reopen" => (true, CODE),
            "complete" | "analyze" => return None,
            _ => return Some(Ok(missing())),
        },
        "data" => match route {
            "source" | "tables" | "indexes" | "page" | "card" | "value" | "movements"
            | "movements/page" => (false, DATA),
            _ => return Some(Ok(missing())),
        },
        _ => return None,
    };
    if if kind == "data" {
        req.method != "GET"
    } else {
        (req.method == "POST") != post
    } {
        return Some(Ok(method()));
    }
    if kind == "data"
        && route == "source"
        && let Some((key, _)) = req.query.first()
    {
        return Some(message(
            budget,
            limit,
            400,
            &format_args!("unknown parameter: {key}"),
        ));
    }
    if kind == "query" && route == "cancel" {
        return Some(
            match req
                .query
                .iter()
                .find(|(key, _)| key == "id")
                .map(|(_, id)| id)
            {
                None => message(budget, limit, 400, &"id is required"),
                Some(id) if id.parse::<u64>().is_err() => {
                    message(budget, limit, 400, &"id: not a number")
                }
                Some(_) => Ok(error(200, b"{\"ok\":false}")),
            },
        );
    }
    Some(
        if (kind == "data" && route == "source") || (kind == "code" && route == "info") {
            json(
                budget,
                limit,
                &BaseInfo {
                    base: false,
                    reason,
                },
            )
            .map(|body| response(200, body))
            .map_err(Into::into)
        } else if kind == "query" && route == "info" {
            json(
                budget,
                limit,
                &QueryInfo {
                    enabled: false,
                    path: source,
                    reason,
                },
            )
            .map(|body| response(200, body))
            .map_err(Into::into)
        } else {
            message(budget, limit, 409, &reason)
        },
    )
}
