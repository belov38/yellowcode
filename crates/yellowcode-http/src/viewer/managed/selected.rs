use super::{ManagedHttpError, RequestControl, error, response};
use crate::transport::{Request, Response};
use mdconf::ManagedEntrySource;
use memory_budget::{BudgetError, BudgetedWriter, MemoryBudget};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::fmt;
use yellowcode_core::viewer::{
    body::ViewerBody,
    exchange_view,
    find::{FindQuery, ManagedFindError},
    object_window,
    path::ManagedPathError,
    predefined_view,
    rights_view::{self, RightsQueryRef},
    show::{self, ManagedShowError},
    snapshot::ManagedSnapshot,
};

pub(super) fn param<'a>(req: &'a Request, key: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}
fn flag(req: &Request, key: &str) -> bool {
    matches!(param(req, key), Some("1" | "true"))
}
pub(super) fn json<T: Serialize + ?Sized>(
    budget: &MemoryBudget,
    limit: u64,
    value: &T,
) -> Result<ViewerBody, BudgetError> {
    let mut out = BudgetedWriter::new(budget, limit)?;
    let result = serde_json::to_writer(out.serialization_sink(), value);
    if let Some(error) = out.error() {
        return Err(*error);
    }
    result.map_err(|_| BudgetError::Overflow)?;
    Ok(ViewerBody::Owned(out.into_buffer()))
}
struct Display<'a, T: ?Sized>(&'a T);
impl<T: fmt::Display + ?Sized> Serialize for Display<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self.0)
    }
}
#[derive(Serialize)]
struct ErrorBody<T> {
    error: T,
}
struct Suggestions<'a>(&'a ManagedPathError);
impl Serialize for Suggestions<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.suggestions.len()))?;
        for row in self.0.suggestions.iter() {
            seq.serialize_element(row.as_str())?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
struct PathError<'a> {
    error: Display<'a, ManagedPathError>,
    suggestions: Suggestions<'a>,
}
pub(super) fn path_error(
    budget: &MemoryBudget,
    limit: u64,
    error: &ManagedPathError,
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    Ok(response(
        404,
        json(
            budget,
            limit,
            &PathError {
                error: Display(error),
                suggestions: Suggestions(error),
            },
        )?,
    ))
}
pub(super) fn message(
    budget: &MemoryBudget,
    limit: u64,
    status: u16,
    message: &impl fmt::Display,
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    Ok(response(
        status,
        json(
            budget,
            limit,
            &ErrorBody {
                error: Display(message),
            },
        )?,
    ))
}
fn selected<E: fmt::Display>(
    budget: &MemoryBudget,
    limit: u64,
    result: Result<ViewerBody, ManagedShowError<E>>,
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    match result {
        Ok(body) => Ok(response(200, body)),
        Err(ManagedShowError::Path(error)) => path_error(budget, limit, &error),
        Err(ManagedShowError::Memory(error)) => Err(error.into()),
        Err(ManagedShowError::Read(error)) => message(
            budget,
            limit,
            503,
            &format_args!("loading selection failed: {error}"),
        ),
    }
}
pub(super) fn handle<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: fmt::Display,
{
    let budget = snapshot.catalog().memory_budget();
    let open = snapshot.open();
    let names = snapshot.names();
    if req.path == "/api/find" {
        let q = param(req, "q").unwrap_or_default();
        if q.is_empty() {
            return Ok(error(400, b"{\"error\":\"q is required\"}"));
        }
        return match snapshot.find_body_with_control(
            &FindQuery {
                pattern: q,
                regex: flag(req, "regex"),
                names_only: flag(req, "names"),
                limit: if flag(req, "names") && flag(req, "tree") {
                    usize::MAX
                } else {
                    500
                },
                wire_limit: limit,
                canceled: control.canceled,
                deadline: control.deadline,
            },
            &|| control.is_canceled(),
        ) {
            Ok(body) => Ok(response(200, body)),
            Err(ManagedFindError::Memory(error)) => Err(error.into()),
            Err(ManagedFindError::Pattern(error)) => message(budget, limit, 400, &error),
            Err(ManagedFindError::Canceled) => {
                control.check()?;
                Err(ManagedHttpError::Canceled)
            }
            Err(error) => message(
                budget,
                limit,
                503,
                &format_args!("loading selection failed: {error}"),
            ),
        };
    }
    if !matches!(
        req.path.as_str(),
        "/api/show"
            | "/api/rights"
            | "/api/object"
            | "/api/predefined"
            | "/api/exchange-membership"
    ) {
        return Ok(error(404, b"{\"error\":\"no such endpoint\"}"));
    }
    let Some(path) = param(req, "path") else {
        return Ok(error(400, b"{\"error\":\"path is required\"}"));
    };
    if req.path == "/api/rights" {
        let mut query = RightsQueryRef {
            q: param(req, "q").unwrap_or("").trim(),
            only: flag(req, "only"),
            templates: flag(req, "templates"),
            from: 0,
            limit: rights_view::DEFAULT_LIMIT,
        };
        for (name, slot) in [("from", &mut query.from), ("limit", &mut query.limit)] {
            if let Some(value) = param(req, name) {
                match value.parse::<usize>() {
                    Ok(n) => *slot = n,
                    Err(_) => {
                        return message(budget, limit, 400, &format_args!("{name}: not a number"));
                    }
                }
            }
        }
        if query.limit == 0 {
            return Ok(error(400, b"{\"error\":\"limit: must be positive\"}"));
        }
        return match rights_view::rights_table_borrowed(
            open,
            &names,
            path,
            snapshot.lang(),
            &query,
            limit,
        ) {
            Ok(Some(body)) => Ok(response(200, body)),
            Ok(None) | Err(ManagedShowError::Path(_)) => {
                Ok(error(404, b"{\"error\":\"no rights for this path\"}"))
            }
            Err(error) => selected(budget, limit, Err(error)),
        };
    }
    match req.path.as_str() {
        "/api/show" => selected(
            budget,
            limit,
            show::show_http_managed(open, &names, path, snapshot.lang(), limit),
        ),
        "/api/object" => selected(
            budget,
            limit,
            object_window::object_window_managed(open, &names, path, snapshot.lang(), limit),
        ),
        "/api/predefined" => {
            match predefined_view::predefined_managed(open, &names, path, snapshot.lang(), limit) {
                Ok(None) => Ok(error(
                    404,
                    b"{\"error\":\"the object has no predefined data\"}",
                )),
                Ok(Some(body)) => Ok(response(200, body)),
                Err(error) => selected(budget, limit, Err(error)),
            }
        }
        _ => {
            if path.trim().is_empty() {
                return Ok(error(400, b"{\"error\":\"path is required\"}"));
            }
            match exchange_view::membership_managed(open, &names, path, limit) {
                Ok(None) => Ok(error(
                    400,
                    b"{\"error\":\"path must name a top-level metadata object\"}",
                )),
                Ok(Some(body)) => Ok(response(200, body)),
                Err(error) => selected(budget, limit, Err(error)),
            }
        }
    }
}
