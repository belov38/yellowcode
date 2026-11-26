use std::io::{BufRead, Write};

use onec_client::auth::printable;
use onec_client::{Cell, ObjectData, Ref, Session, SessionError, Value};
use serde_json::{Value as Json, json};

use super::{
    Shown, enum_presentation_json, open_session, presentation_json, show, steps_line, users_hint,
    values,
};
use crate::Result;
use crate::output::Out;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef(Ref);

impl ObjectRef {
    pub fn parse(arg: &str) -> std::result::Result<ObjectRef, String> {
        onec_client::object::parse_ref(arg).map(ObjectRef)
    }
}

#[derive(Debug, Clone)]
pub struct ObjectRequest {
    pub login: super::Login,
    pub reference: ObjectRef,
    pub json: bool,
    pub presentations: bool,
}

pub(super) fn run(
    r: ObjectRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    let (mut session, login) = open_session(&r.login, input)?;
    let outcome = login.and_then(|()| session.read_object(&r.reference.0));
    let shown = match &outcome {
        Ok(o) if r.presentations => Some(ask(&mut session, o).map_err(|e| e.to_string())),
        _ => None,
    };
    let _ = session.logout();
    let object = match outcome {
        Ok(o) => o,
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            return Err(e.into());
        }
    };
    if r.json {
        let j = object_json(&object, shown.as_ref(), &session);
        writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
        return Ok(0);
    }
    let (pres, warning) = match shown {
        None => (None, None),
        Some(Ok(p)) => (Some(p), None),
        Some(Err(e)) => (None, Some(format!("warning: presentations not shown: {e}"))),
    };
    for line in object_lines(&object, pres.as_ref()) {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "RESULT OK {}", steps_line(&session))?;
    if let Some(w) = warning {
        writeln!(err, "{w}")?;
    }
    Ok(0)
}

fn values_of(o: &ObjectData) -> Vec<&Value> {
    let fields = o.fields.iter().map(|f| &f.value);
    let cells = o.tables.iter().flat_map(|t| t.rows.iter().flatten());
    fields
        .chain(cells)
        .filter_map(|c| match c {
            Cell::Value(v) => Some(v),
            Cell::Raw(_) => None,
        })
        .collect()
}

fn ask(session: &mut Session, o: &ObjectData) -> std::result::Result<Shown, SessionError> {
    let values = values_of(o);
    let refs = session.presentations(&onec_client::refs::collect(values.iter().copied()))?;
    let enums = session.enum_presentations(&onec_client::enums::collect(values.iter().copied()))?;
    Ok(Shown { refs, enums })
}

fn cell_text(c: &Cell, inner: bool, pres: Option<&Shown>) -> String {
    match c {
        Cell::Value(Value::String(t)) if !inner => printable(t),
        Cell::Value(v) => show(v, inner, pres),
        Cell::Raw(t) => format!("raw:{t:?}"),
    }
}

pub(super) fn object_lines(o: &ObjectData, pres: Option<&Shown>) -> Vec<String> {
    let mut l = vec![format!(
        "object: {}({})",
        o.reference.type_name(),
        o.reference.uuid()
    )];
    if let Some(t) = &o.title {
        l.push(format!("title: {}", printable(t)));
    }
    l.push(format!(
        "form: {}{}",
        printable(&o.form_id),
        if o.read_only { " (read only)" } else { "" }
    ));
    for f in &o.fields {
        l.push(format!(
            "{}: {}",
            printable(f.field.display_name()),
            cell_text(&f.value, false, pres)
        ));
    }
    for t in &o.tables {
        l.push(format!(
            "{}: {} rows",
            printable(t.field.display_name()),
            t.rows.len()
        ));
        l.push(format!(
            "  {}",
            t.columns
                .iter()
                .map(|c| printable(c.display_name()))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        for row in &t.rows {
            l.push(format!(
                "  {}",
                row.iter()
                    .map(|c| cell_text(c, true, pres))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
    }
    l
}

fn cell_json(c: &Cell) -> Json {
    match c {
        Cell::Value(v) => values::to_json(v),
        Cell::Raw(t) => json!({ "raw": t }),
    }
}

fn field_json(f: &onec_client::FormField) -> serde_json::Map<String, Json> {
    let mut o = serde_json::Map::new();
    o.insert("id".into(), json!(f.id));
    o.insert("name".into(), json!(f.display_name()));
    o
}

pub(super) fn object_json(
    o: &ObjectData,
    pres: Option<&std::result::Result<Shown, String>>,
    session: &Session,
) -> Json {
    let mut j = serde_json::Map::new();
    j.insert(
        "ref".into(),
        json!([
            o.reference.type_name().to_string(),
            o.reference.uuid().to_string()
        ]),
    );
    j.insert("form_id".into(), json!(o.form_id));
    j.insert("title".into(), json!(o.title));
    j.insert("read_only".into(), json!(o.read_only));
    let fields: Vec<Json> = o
        .fields
        .iter()
        .map(|f| {
            let mut x = field_json(&f.field);
            x.insert("value".into(), cell_json(&f.value));
            Json::Object(x)
        })
        .collect();
    j.insert("fields".into(), Json::Array(fields));
    let tables: Vec<Json> = o
        .tables
        .iter()
        .map(|t| {
            let mut x = field_json(&t.field);
            x.insert(
                "columns".into(),
                Json::Array(
                    t.columns
                        .iter()
                        .map(|c| Json::Object(field_json(c)))
                        .collect(),
                ),
            );
            x.insert(
                "rows".into(),
                Json::Array(
                    t.rows
                        .iter()
                        .map(|r| Json::Array(r.iter().map(cell_json).collect()))
                        .collect(),
                ),
            );
            Json::Object(x)
        })
        .collect();
    j.insert("tables".into(), Json::Array(tables));
    match pres {
        None => {}
        Some(Ok(p)) => {
            let items = p
                .refs
                .items()
                .iter()
                .map(|(r, p)| presentation_json(r, p))
                .chain(
                    p.enums
                        .items()
                        .iter()
                        .map(|(e, p)| enum_presentation_json(e, p)),
                )
                .collect();
            j.insert("presentations".into(), Json::Array(items));
        }
        Some(Err(e)) => {
            j.insert("presentations_error".into(), json!(e));
        }
    }
    j.insert(
        "unrecognized".into(),
        o.unrecognized
            .iter()
            .map(|x| json!({"answer": x.answer, "name": x.name}))
            .collect(),
    );
    j.insert(
        "steps".into(),
        session
            .steps()
            .iter()
            .map(|s| json!({"name": s.name, "status": s.status}))
            .collect(),
    );
    Json::Object(j)
}
