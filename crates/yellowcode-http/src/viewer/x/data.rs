use query_console::Cell;
use query_console::data::{Card, DataError, DataPageRequest, DocumentRegisters};
use query_console::{Console, IndexOrder, IndexRange, RowFilter, TableReadError};
use serde_json::{Value, json};

use super::super::http::{Request, Response};
use super::super::state::State;
use super::{Availability, NotBase, error, infobase_source, reply, slot_for};

pub fn handle(state: &State, req: &Request, route: &str) -> Response {
    if !matches!(
        route,
        "source"
            | "tables"
            | "indexes"
            | "page"
            | "card"
            | "value"
            | "movements"
            | "movements/page"
    ) {
        return error(404, "no such endpoint");
    }
    if req.method != "GET" {
        return error(405, "method not allowed");
    }
    if route == "source" {
        if let Err(r) = params(req, []) {
            return r;
        }
        return match infobase_source(state) {
            Ok(()) => reply(200, &json!({ "base": true })),
            Err(kind) => reply(200, &json!({ "base": false, "reason": not_base(kind) })),
        };
    }
    if let Err(kind) = infobase_source(state) {
        return error(409, &not_base(kind));
    }
    let console = match slot_for(state).get(&state.source) {
        Availability::Ready(c) => c,
        Availability::Failed(e) => return error(409, &format!("база не открыта: {e}")),
    };
    let answer = match route {
        "tables" => tables(&console, req),
        "indexes" => indexes(&console, req),
        "card" => card(&console, req),
        "value" => card_value(&console, req),
        "movements" => movements(&console, req),
        "movements/page" => movements_page(&console, req),
        _ => page(&console, req),
    };
    answer.unwrap_or_else(|r| r)
}

fn not_base(kind: NotBase) -> String {
    match kind {
        NotBase::Cf => "Данные открываются на базе: откройте файл 1Cv8.1CD (сейчас открыта конфигурация из файла .cf)".into(),
        NotBase::Dt => "DT открыт только для просмотра метаданных CONFIG; данные открываются на восстановленной базе 1Cv8.1CD".into(),
        NotBase::XmlDir => "Данные открываются на базе: откройте файл 1Cv8.1CD (сейчас открыт каталог выгрузки)".into(),
        NotBase::Unknown(e) => format!("Источник не определён: {e}"),
    }
}

fn params<'a, const N: usize>(
    req: &'a Request,
    names: [&str; N],
) -> Result<[Option<&'a str>; N], Response> {
    let mut out = [None; N];
    for (k, v) in &req.query {
        let Some(slot) = names
            .iter()
            .position(|n| n == k)
            .and_then(|i| out.get_mut(i))
        else {
            return Err(error(400, &format!("unknown parameter: {k}")));
        };
        if slot.replace(v.as_str()).is_some() {
            return Err(error(400, &format!("repeated parameter: {k}")));
        }
    }
    Ok(out)
}

fn table_path(path: Option<&str>) -> Result<(&str, &str), Response> {
    let path = path.ok_or_else(|| error(400, "path is required"))?;
    match path.split_once('.') {
        Some((kind, name)) if !kind.is_empty() && !name.is_empty() => {
            if name.contains('.') {
                Err(error(404, &format!("no such table: {path}")))
            } else {
                Ok((kind, name))
            }
        }
        _ => Err(error(400, "path: expected <kind>.<name>")),
    }
}

fn failure(e: TableReadError) -> Response {
    let code = match &e {
        TableReadError::IndexRejected {
            error,
            available_indexes,
        } => {
            return reply(
                422,
                &json!({
                    "error": error,
                    "availableIndexes": serde_json::to_value(available_indexes).unwrap_or(Value::Null),
                }),
            );
        }
        TableReadError::Invalid(_) => 400,
        TableReadError::NotFound(_) => 404,
        TableReadError::Unsupported(_) => 422,
        TableReadError::Memory(_) => 503,
        TableReadError::Storage(_) => 500,
    };
    error(code, &e.to_string())
}

fn value<T: serde::Serialize + ?Sized>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn tables(console: &Console, req: &Request) -> Result<Response, Response> {
    params(req, [])?;
    Ok(reply(200, console.data_tables()))
}

fn indexes(console: &Console, req: &Request) -> Result<Response, Response> {
    let [path] = params(req, ["path"])?;
    let (kind, name) = table_path(path)?;
    let listed = console.data_indexes(kind, name).map_err(failure)?;
    let (period, reason) = match console.period_access(kind, name).map_err(failure)? {
        Ok(p) => (value(&p), Value::Null),
        Err(reason) => (Value::Null, Value::String(reason)),
    };
    Ok(reply(
        200,
        &json!({
            "default": listed.default,
            "limit": listed.limit,
            "period": period,
            "periodReason": reason,
            "indexes": value(&listed.indexes),
            "filters": value(&listed.filters),
        }),
    ))
}

struct Cuts<'a>(&'a [(usize, usize, usize)]);
impl serde::Serialize for Cuts<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(serde::Serialize)]
        struct Cut {
            row: usize,
            col: usize,
            length: usize,
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for &(row, col, length) in self.0 {
            seq.serialize_element(&Cut { row, col, length })?;
        }
        seq.end()
    }
}

fn page(console: &Console, req: &Request) -> Result<Response, Response> {
    let [
        path,
        index,
        eq,
        range,
        order,
        posted,
        deletion_mark,
        cursor,
        limit,
    ] = params(
        req,
        [
            "path",
            "index",
            "eq",
            "range",
            "order",
            "posted",
            "deletionMark",
            "cursor",
            "limit",
        ],
    )?;
    let (kind, name) = table_path(path)?;
    let index = match index {
        Some("") => return Err(error(400, "index: empty")),
        other => other.map(str::to_owned),
    };
    let eq = match eq {
        None => Vec::new(),
        Some(text) => serde_json::from_str::<Vec<String>>(text)
            .map_err(|e| error(400, &format!("eq: expected a JSON array of strings: {e}")))?,
    };
    let range = match range {
        None => IndexRange::default(),
        Some(text) => serde_json::from_str::<IndexRange>(text).map_err(|e| {
            error(
                400,
                &format!("range: expected a JSON object {{gt|gte, lt|lte}}: {e}"),
            )
        })?,
    };
    let order = match order {
        None | Some("asc") => IndexOrder::Asc,
        Some("desc") => IndexOrder::Desc,
        Some(_) => return Err(error(400, "order: expected asc or desc")),
    };
    let limit = parse_limit(limit)?;
    let filter = RowFilter {
        posted: flag("posted", posted)?,
        deletion_mark: flag("deletionMark", deletion_mark)?,
    };
    let request = DataPageRequest {
        index,
        eq,
        range,
        order,
        cursor: cursor.map(str::to_owned),
        limit,
        filter,
    };
    let page = console.data_page(kind, name, &request).map_err(failure)?;
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer<'a> {
        columns: &'a [query_console::Column],
        rows: &'a [Vec<Cell>],
        next_cursor: Option<&'a str>,
        index: Option<&'a str>,
        order: &'a IndexOrder,
        limit: usize,
        cut: Cuts<'a>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<&'a RowFilter>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scanned: Option<u64>,
    }
    let filtered = page.page.filter.is_some() && page.page.scanned.is_some();
    Ok(reply(
        200,
        &Answer {
            columns: &page.page.columns,
            rows: &page.page.rows,
            next_cursor: page.page.next_cursor.as_deref(),
            index: page.index.as_deref(),
            order: &page.order,
            limit: page.limit,
            cut: Cuts(&page.cut),
            filter: filtered.then_some(page.page.filter.as_ref()).flatten(),
            scanned: filtered.then_some(page.page.scanned).flatten(),
        },
    ))
}

fn flag(name: &str, text: Option<&str>) -> Result<Option<bool>, Response> {
    match text {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(error(400, &format!("{name}: expected true or false"))),
    }
}

fn parse_limit(limit: Option<&str>) -> Result<Option<usize>, Response> {
    match limit {
        None => Ok(None),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|l| (1..=10_000).contains(l) && text.bytes().all(|b| b.is_ascii_digit()))
            .map(Some)
            .ok_or_else(|| error(400, "limit: expected a number from 1 to 10000")),
    }
}

fn data_failure(e: DataError) -> Response {
    let code = match e {
        DataError::Table(e) => return failure(e),
        DataError::Invalid(_) => 400,
        DataError::NotFound(_) => 404,
        DataError::Query(_) | DataError::TooLarge(_) => 422,
    };
    error(code, &e.to_string())
}

fn card(console: &Console, req: &Request) -> Result<Response, Response> {
    let [reference] = params(req, ["ref"])?;
    let reference = reference.ok_or_else(|| error(400, "ref is required"))?;
    let Some(card) = console.object_card(reference).map_err(data_failure)? else {
        return Ok(reply(200, &json!({ "found": false })));
    };
    #[derive(serde::Serialize)]
    struct Found<'a> {
        found: bool,
        #[serde(flatten)]
        card: &'a Card,
    }
    Ok(body(&Found {
        found: true,
        card: &card,
    }))
}

fn body<T: serde::Serialize>(v: &T) -> Response {
    reply(200, v)
}

fn card_value(console: &Console, req: &Request) -> Result<Response, Response> {
    let [reference, field, tabular, line] = params(req, ["ref", "field", "tabular", "line"])?;
    let reference = reference.ok_or_else(|| error(400, "ref is required"))?;
    let field = field.ok_or_else(|| error(400, "field is required"))?;
    let line = match (tabular, line) {
        (None, None) => None,
        (Some(part), Some(n)) => Some((
            part,
            n.parse::<u32>()
                .ok()
                .filter(|_| n.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| error(400, "line: expected a line number (НомерСтроки)"))?,
        )),
        _ => return Err(error(400, "tabular and line go together")),
    };
    match console
        .card_value(reference, field, line)
        .map_err(data_failure)?
    {
        Some(cell) => {
            #[derive(serde::Serialize)]
            struct Answer<'a> {
                value: &'a Cell,
            }
            Ok(body(&Answer { value: &cell }))
        }
        None => Err(error(
            404,
            if line.is_some() {
                "no such object or line"
            } else {
                "no such object"
            },
        )),
    }
}

fn movements(console: &Console, req: &Request) -> Result<Response, Response> {
    let [reference] = params(req, ["ref"])?;
    let reference = reference.ok_or_else(|| error(400, "ref is required"))?;
    let Some(list) = console
        .document_registers(reference)
        .map_err(data_failure)?
    else {
        return Ok(reply(200, &json!({ "found": false })));
    };
    #[derive(serde::Serialize)]
    struct Found<'a> {
        found: bool,
        #[serde(flatten)]
        list: &'a DocumentRegisters,
    }
    Ok(body(&Found {
        found: true,
        list: &list,
    }))
}

fn movements_page(console: &Console, req: &Request) -> Result<Response, Response> {
    let [reference, register, cursor, limit] = params(req, ["ref", "register", "cursor", "limit"])?;
    let reference = reference.ok_or_else(|| error(400, "ref is required"))?;
    let register = register.ok_or_else(|| error(400, "register is required"))?;
    let limit = parse_limit(limit)?;
    let Some(page) = console
        .document_movements_page(reference, register, cursor, limit)
        .map_err(data_failure)?
    else {
        return Ok(reply(200, &json!({ "found": false })));
    };
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer<'a> {
        found: bool,
        register: &'a str,
        columns: &'a [query_console::Column],
        rows: &'a [Vec<Cell>],
        next_cursor: Option<&'a str>,
        index: Option<&'a str>,
        limit: usize,
        cut: Cuts<'a>,
    }
    Ok(body(&Answer {
        found: true,
        register: &page.register,
        columns: &page.page.columns,
        rows: &page.page.rows,
        next_cursor: page.page.next_cursor.as_deref(),
        index: page.index.as_deref(),
        limit: page.limit,
        cut: Cuts(&page.cut),
    }))
}
