use std::io::{BufRead, Write};

use onec_client::auth::printable;
use onec_client::dlist::{Cell, Filter, ListQuery, ListView, Order, Row};
use onec_client::{Date, EnumPresentation, Presentation, Session, Uuid, Value};
use serde_json::{Value as Json, json};

use super::{Login, open_session, steps_line, users_hint, values};
use crate::Result;
use crate::output::Out;

pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const DEFAULT_LIMIT: usize = 10_000;

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub login: Login,
    pub object: String,
    pub form: Option<String>,
    pub parent: Option<Uuid>,
    pub filters: Vec<Filter>,
    pub orders: Vec<Order>,
    pub page: usize,
    pub page_size: usize,
    pub all: bool,
    pub limit: usize,
    pub presentations: bool,
    pub json: bool,
}

impl ListRequest {
    fn query(&self) -> ListQuery {
        let (skip, take) = if self.all {
            (0, None)
        } else {
            (
                self.page.saturating_sub(1).saturating_mul(self.page_size),
                Some(self.page_size),
            )
        };
        ListQuery {
            object: self.object.clone(),
            form: self.form.clone(),
            parent: self.parent,
            filters: self.filters.clone(),
            orders: self.orders.clone(),
            skip,
            take,
            limit: self.limit,
            presentations: self.presentations,
        }
    }
}

pub(super) fn run(
    r: ListRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    if r.page == 0 || r.page_size == 0 {
        return Err("--page and --page-size start at 1".into());
    }
    let (mut session, login) = open_session(&r.login, input)?;
    let q = r.query();
    let outcome = login.and_then(|()| session.run_list(&q));
    let _ = session.logout();
    match outcome {
        Ok(view) => {
            for w in warnings(&view) {
                writeln!(err, "warning: {}", printable(&w))?;
            }
            let lines = if r.json {
                vec![serde_json::to_string_pretty(&list_json(
                    &view, &r, q.skip, &session,
                ))?]
            } else {
                list_lines(&view, &r, q.skip, &steps_line(&session))
            };
            for line in lines {
                writeln!(out, "{line}")?;
            }
            Ok(0)
        }
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            Err(e.into())
        }
    }
}

fn warnings(v: &ListView) -> Vec<String> {
    let mut w = Vec::new();
    w.extend(v.columns_warning.clone());
    if let Some(e) = &v.presentations_error {
        w.push(format!(
            "the presentations of the references are not known: {e}"
        ));
    }
    if let Some(e) = &v.close_error {
        w.push(format!("the list form was not closed: {e}"));
    }
    w
}

fn show_date(d: &Date) -> String {
    if d.is_empty() {
        return String::new();
    }
    let (y, mo, day, h, mi, s) = d.parts();
    format!("{day:02}.{mo:02}.{y:04} {h}:{mi:02}:{s:02}")
}

fn cell_text(v: &ListView, c: &Cell) -> String {
    match c {
        Cell::Value(Value::String(s)) => s.clone(),
        Cell::Value(Value::Number(n)) => n.to_string(),
        Cell::Value(Value::Bool(b)) => (if *b { "Да" } else { "Нет" }).to_string(),
        Cell::Value(Value::Date(d)) => show_date(d),
        Cell::Value(Value::Ref(r)) => match v.refs.get(r) {
            Some(Presentation::Valid(t) | Presentation::NotFound(t)) => t.clone(),
            _ if r.is_empty() => String::new(),
            _ => format!("{}({})", r.type_name(), r.uuid()),
        },
        Cell::Value(Value::EnumRef(e)) => match v.enums.get(e) {
            Some(EnumPresentation::Synonym(t)) => t.clone(),
            _ => e.name().to_string(),
        },
        Cell::Value(other) => other.lexical().unwrap_or_default(),
        Cell::RecordKey => "<ключ записи>".into(),
        Cell::UnknownRef { clsid, uuid } => format!("<ссылка {clsid}:{uuid}>"),
        Cell::Unsupported(raw) => format!("<? {raw}>"),
    }
}

fn flag(v: &ListView, row: &Row, role: Option<&str>) -> Option<bool> {
    match row.cells.get(v.role(role)?) {
        Some(Cell::Value(Value::Bool(b))) => Some(*b),
        _ => None,
    }
}

fn marks(v: &ListView, row: &Row) -> String {
    let roles = &v.form.roles;
    let mut m = String::new();
    if flag(v, row, roles.folder.as_deref()) == Some(true) {
        m.push_str("[группа] ");
    }
    if flag(v, row, roles.marked.as_deref()) == Some(true) {
        m.push_str("[удалён] ");
    }
    if flag(v, row, roles.posted.as_deref()) == Some(true) {
        m.push_str("[проведён] ");
    }
    m
}

fn heading(v: &ListView, r: &ListRequest, skip: usize) -> String {
    let name = v
        .form
        .object
        .last()
        .cloned()
        .unwrap_or_else(|| r.object.clone());
    let title = match &v.form.title {
        Some(t) => format!("{} ({})", printable(t), printable(&name)),
        None => printable(&name),
    };
    let range = if v.rows.is_empty() {
        "нет строк".to_string()
    } else {
        format!("строки {}–{}", skip + 1, skip + v.rows.len())
    };
    let more = if v.more {
        ", дальше есть"
    } else {
        ""
    };
    format!("{title}: {range}{more}")
}

fn list_lines(v: &ListView, r: &ListRequest, skip: usize, steps: &str) -> Vec<String> {
    let mut l = vec![heading(v, r, skip)];
    l.push(
        v.columns
            .iter()
            .map(|c| printable(&c.1))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    for row in &v.rows {
        let cells: Vec<String> = v
            .columns
            .iter()
            .map(|(i, _)| {
                i.and_then(|i| row.cells.get(i))
                    .map(|c| printable(&cell_text(v, c)))
                    .unwrap_or_default()
            })
            .collect();
        l.push(format!("{}{}", marks(v, row), cells.join(" | ")));
    }
    l.push(format!("RESULT OK {steps}"));
    l
}

fn cell_json(v: &ListView, c: &Cell) -> Json {
    match c {
        Cell::Value(x) => json!({"value": values::to_json(x), "text": cell_text(v, c)}),
        Cell::RecordKey => json!({"record_key": true}),
        Cell::UnknownRef { clsid, uuid } => {
            json!({"unknown_ref": [clsid.to_string(), uuid.to_string()]})
        }
        Cell::Unsupported(raw) => json!({ "unsupported": raw }),
    }
}

fn list_json(v: &ListView, r: &ListRequest, skip: usize, session: &Session) -> Json {
    let roles = &v.form.roles;
    let rows: Vec<Json> = v
        .rows
        .iter()
        .map(|row| {
            let mut o = serde_json::Map::new();
            let cells: Vec<Json> = v
                .columns
                .iter()
                .map(|(i, _)| {
                    i.and_then(|i| row.cells.get(i))
                        .map_or(Json::Null, |c| cell_json(v, c))
                })
                .collect();
            o.insert("cells".into(), Json::Array(cells));
            if let Some(c) = v.role(roles.key.as_deref()).and_then(|i| row.cells.get(i)) {
                o.insert("key".into(), cell_json(v, c));
            }
            for (name, role) in [
                ("folder", &roles.folder),
                ("deletion_mark", &roles.marked),
                ("posted", &roles.posted),
            ] {
                if let Some(b) = flag(v, row, role.as_deref()) {
                    o.insert(name.into(), json!(b));
                }
            }
            Json::Object(o)
        })
        .collect();
    let mut o = serde_json::Map::new();
    o.insert("object".into(), json!(v.form.object));
    o.insert("form".into(), json!(v.form.form_id));
    o.insert("title".into(), json!(v.form.title));
    if r.all {
        o.insert("all".into(), json!(true));
    } else {
        o.insert("page".into(), json!(r.page));
        o.insert("page_size".into(), json!(r.page_size));
    }
    o.insert("first_row".into(), json!(skip + 1));
    o.insert("more".into(), json!(v.more));
    o.insert(
        "columns".into(),
        Json::Array(
            v.columns
                .iter()
                .map(|(i, t)| {
                    let field = i
                        .and_then(|i| v.form.descr.fields.get(i))
                        .map(|f| f.name_ru.as_str());
                    json!({"field": field, "title": t})
                })
                .collect(),
        ),
    );
    o.insert("rows".into(), Json::Array(rows));
    for (k, w) in [
        ("columns_warning", &v.columns_warning),
        ("presentations_error", &v.presentations_error),
        ("close_error", &v.close_error),
    ] {
        if let Some(w) = w {
            o.insert(k.into(), json!(w));
        }
    }
    o.insert(
        "steps".into(),
        Json::Array(
            session
                .steps()
                .iter()
                .map(|s| json!({"name": s.name, "status": s.status}))
                .collect(),
        ),
    );
    Json::Object(o)
}
