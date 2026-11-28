use std::io::{BufRead, Write};

use onec_client::auth::printable;
use onec_client::{
    Assign, AttrValue, CallOutcome, Called, Cell, Changed, CommandRun, FormAction, FormAttr,
    FormCommand, FormRequest, FormStep, FormTarget, FormValue, FormView, Input, MethodPlace,
    ObjectChange, Ref, ServerMethod, Session, SessionError, SetValue, UserMessage, Value,
};
use serde_json::{Value as Json, json};

use super::write::{Edit, add_edits};
use super::{
    Param, Shown, enum_presentation_json, open_session, presentation_json, show, steps_line, values,
};
use crate::Result;
use crate::output::Out;

pub const UNKNOWN_EXIT: i32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum FormEdit {
    Edit(Edit),
    Json { name: String, value: Value },
}

impl FormEdit {
    pub fn parse_json(arg: &str) -> std::result::Result<FormEdit, String> {
        match arg.split_once('=') {
            Some((name, j)) if !name.trim().is_empty() => Ok(FormEdit::Json {
                name: name.trim().to_string(),
                value: super::parse_param_json(j)?,
            }),
            _ => Err(format!("not a change: {arg:?} (Реквизит=<typed JSON>)")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FormRun {
    Command(String),
    Call { name: String, params: Vec<Param> },
}

#[derive(Debug, Clone)]
pub struct FormCall {
    pub edits: Vec<FormEdit>,
    pub run: FormRun,
}

#[derive(Debug, Clone)]
pub struct FormCommandRequest {
    pub login: super::Login,
    pub form: Option<String>,
    pub reference: Option<Ref>,
    pub calls: Vec<FormCall>,
    pub json: bool,
    pub presentations: bool,
    pub init: Option<super::InitCall>,
}

fn change_of(edits: &[FormEdit]) -> std::result::Result<ObjectChange, String> {
    let mut change = ObjectChange::default();
    let plain: Vec<Edit> = edits
        .iter()
        .filter_map(|e| match e {
            FormEdit::Edit(e) => Some(e.clone()),
            FormEdit::Json { .. } => None,
        })
        .collect();
    add_edits(&mut change, &plain, None)?;
    for e in edits {
        if let FormEdit::Json { name, value } = e {
            change.set.push(Assign {
                name: name.clone(),
                input: Input::Value(value.clone()),
            });
        }
    }
    Ok(change)
}

pub(super) fn run(
    r: FormCommandRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    let target = match (&r.reference, &r.form) {
        (Some(reference), key) => FormTarget::Object {
            reference: reference.clone(),
            key: key.clone(),
        },
        (None, Some(key)) => FormTarget::Form(key.clone()),
        (None, None) => return Err("--form or --ref is needed".into()),
    };
    let steps = r
        .calls
        .iter()
        .map(|c| {
            Ok(FormStep {
                change: change_of(&c.edits)?,
                action: match &c.run {
                    FormRun::Command(name) => FormAction::Command(name.clone()),
                    FormRun::Call { name, params } => FormAction::Call {
                        name: name.clone(),
                        params: params.iter().map(|Param(v)| v.clone()).collect(),
                    },
                },
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let req = FormRequest { target, steps };
    let (session, login) = open_session(&r.login, input)?;
    let presentations = r.presentations;
    let init = r.init.clone();
    let (session, outcome, shown) = super::on_value_stack(move || {
        let mut session = session;
        let outcome = super::start(&mut session, login, init.as_ref())
            .and_then(|init| Ok((init, session.run_form(&req)?)));
        let shown = match &outcome {
            Ok((_, v)) if presentations => Some(ask(&mut session, v).map_err(|e| e.to_string())),
            _ => None,
        };
        let _ = session.logout();
        (session, outcome, shown)
    })?;
    let (init, view) = match outcome {
        Ok(v) => v,
        Err(e) => return super::session_failed(&r.login.user, e, &session, r.json, out, err),
    };
    let (pres, pres_error) = match shown {
        None => (None, None),
        Some(Ok(p)) => (Some(p), None),
        Some(Err(e)) => (None, Some(e)),
    };
    let unknown = view.calls.iter().find_map(|c| match &c.outcome {
        CallOutcome::Unknown(t) => Some(t.clone()),
        _ => None,
    });
    let failed = view.calls.iter().find_map(|c| match &c.outcome {
        CallOutcome::Failed { reason, text, .. } => Some(format!(
            "the call {} failed on the server: {}{}",
            c.method,
            reason.as_ref().map_or(String::new(), |r| format!("{r}: ")),
            printable(text)
        )),
        _ => None,
    });
    let refused = view
        .refused
        .as_ref()
        .map(|t| format!("refused before the call: {}", printable(t)));
    if r.json {
        let mut j = view_json(&view);
        match (&pres, &pres_error) {
            (Some(p), _) => put(&mut j, "presentations", presentations_json(p)),
            (None, Some(e)) => put(&mut j, "presentations_error", json!(e)),
            (None, None) => {}
        }
        put(&mut j, "steps", steps_json(&session));
        if let Some(c) = &init {
            put(&mut j, "init", super::init_json(c));
        }
        if let Some(u) = &unknown {
            put(&mut j, "outcome", json!({ "unknown": { "text": u } }));
        }
        writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
    } else {
        for line in init.iter().map(super::init_line) {
            writeln!(out, "{line}")?;
        }
        for line in view_lines(&view, pres.as_ref()) {
            writeln!(out, "{line}")?;
        }
        let status = match (&unknown, failed.is_some() || refused.is_some()) {
            (Some(_), _) => "UNKNOWN",
            (None, true) => "FAIL",
            (None, false) => "OK",
        };
        writeln!(out, "RESULT {status} {}", steps_line(&session))?;
    }
    for w in &view.warnings {
        writeln!(err, "warning: {}", printable(w))?;
    }
    if let Some(e) = &pres_error {
        writeln!(err, "warning: presentations not shown: {e}")?;
    }
    if let Some(u) = unknown {
        writeln!(
            err,
            "yellowcode: the call was sent and its outcome is not known ({}); the method may have run on the server",
            printable(&u)
        )?;
        return Ok(UNKNOWN_EXIT);
    }
    match failed.or(refused) {
        Some(f) => Err(f.into()),
        None => Ok(0),
    }
}

fn values_of(v: &FormView) -> Vec<&Value> {
    fn form_value(x: &FormValue) -> Option<&Cell> {
        match x {
            FormValue::Cell(c) => Some(c),
            FormValue::Period(_) => None,
        }
    }
    let mut cells: Vec<&Cell> = Vec::new();
    for a in &v.attributes {
        match &a.value {
            AttrValue::Cell(c) => cells.push(c),
            AttrValue::Object { fields, tables } => {
                cells.extend(fields.iter().map(|(_, c)| c));
                cells.extend(tables.iter().flat_map(|(_, _, r)| r.iter().flatten()));
            }
            AttrValue::Rows { rows, .. } => cells.extend(rows.iter().flatten()),
            _ => {}
        }
    }
    for c in &v.calls {
        for s in &c.set {
            cells.extend(s.old.iter().chain(&s.new).filter_map(form_value));
        }
        let changed = match &c.outcome {
            CallOutcome::Done { ret, changed } => {
                cells.extend(ret);
                changed.as_slice()
            }
            CallOutcome::Failed { changed, .. } => changed.as_slice(),
            CallOutcome::Unknown(_) => &[],
        };
        for x in changed {
            match x {
                Changed::Value { old, new, .. } => cells.extend(
                    old.iter()
                        .chain(std::iter::once(new))
                        .filter_map(form_value),
                ),
                Changed::Rows { rows, .. } => cells.extend(rows.iter().flat_map(|(_, r)| r)),
                _ => {}
            }
        }
    }
    cells
        .into_iter()
        .filter_map(|c| match c {
            Cell::Value(v) => Some(v),
            Cell::Raw(_) => None,
        })
        .collect()
}

fn ask(session: &mut Session, v: &FormView) -> std::result::Result<Shown, SessionError> {
    let values = values_of(v);
    let refs = session.presentations(&onec_client::refs::collect(values.iter().copied()))?;
    let enums = session.enum_presentations(&onec_client::enums::collect(values.iter().copied()))?;
    Ok(Shown { refs, enums })
}

fn presentations_json(p: &Shown) -> Json {
    Json::Array(
        p.refs
            .items()
            .iter()
            .map(|(r, x)| presentation_json(r, x))
            .chain(
                p.enums
                    .items()
                    .iter()
                    .map(|(e, x)| enum_presentation_json(e, x)),
            )
            .collect(),
    )
}

fn put(j: &mut Json, key: &str, value: Json) {
    if let Some(o) = j.as_object_mut() {
        o.insert(key.to_string(), value);
    }
}

fn steps_json(session: &onec_client::Session) -> Json {
    json!(
        session
            .steps()
            .iter()
            .map(|s| json!({ "name": s.name, "status": s.status }))
            .collect::<Vec<_>>()
    )
}

fn cell_text(c: &Cell, inner: bool, pres: Option<&Shown>) -> String {
    match c {
        Cell::Value(onec_client::Value::String(t)) if !inner => printable(t),
        Cell::Value(v) => show(v, inner, pres),
        Cell::Raw(t) => format!("raw:{t:?}"),
    }
}

fn value_text(v: &FormValue, inner: bool, pres: Option<&Shown>) -> String {
    match v {
        FormValue::Cell(c) => cell_text(c, inner, pres),
        FormValue::Period(p) => format!("period:{}", printable(&p.to_string())),
    }
}

fn row_text(cells: &[Cell], pres: Option<&Shown>) -> String {
    cells
        .iter()
        .map(|c| cell_text(c, true, pres))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn columns_text(columns: &[onec_client::FormField]) -> String {
    columns
        .iter()
        .map(|c| printable(c.display_name()))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn rows_lines(
    head: String,
    columns: &[onec_client::FormField],
    rows: &[Vec<Cell>],
    pres: Option<&Shown>,
    l: &mut Vec<String>,
) {
    l.push(format!("{head}: {} rows", rows.len()));
    if !rows.is_empty() {
        l.push(format!("    {}", columns_text(columns)));
        l.extend(rows.iter().map(|r| format!("    {}", row_text(r, pres))));
    }
}

fn attribute_lines(a: &FormAttr, pres: Option<&Shown>, l: &mut Vec<String>) {
    let name = printable(a.attribute.field.display_name());
    match &a.value {
        AttrValue::Cell(c) => l.push(format!("attribute: {name}: {}", cell_text(c, false, pres))),
        AttrValue::Period(p) => l.push(format!(
            "attribute: {name}: period:{}",
            printable(&p.to_string())
        )),
        AttrValue::Sheet => l.push(format!("attribute: {name}: (spreadsheet document)")),
        AttrValue::Other(_) => l.push(format!("attribute: {name}: (a value not shown)")),
        AttrValue::Rows { columns, rows } => {
            rows_lines(format!("attribute: {name}"), columns, rows, pres, l);
        }
        AttrValue::Object { fields, tables } => {
            l.push(format!(
                "attribute: {name}{}",
                if a.main { " (main)" } else { "" }
            ));
            for (f, c) in fields {
                l.push(format!(
                    "  {name}.{}: {}",
                    printable(f.display_name()),
                    cell_text(c, false, pres)
                ));
            }
            for (t, columns, rows) in tables {
                rows_lines(
                    format!("  {name}.{}", printable(t.display_name())),
                    columns,
                    rows,
                    pres,
                    l,
                );
            }
        }
    }
}

fn place_text(p: MethodPlace) -> &'static str {
    match p {
        MethodPlace::Server => "server",
        MethodPlace::ServerWithoutContext => "server without context",
    }
}

fn method_line(m: &ServerMethod) -> String {
    let optional = m.params.saturating_sub(m.required);
    format!(
        "method: {} {}({}{}) {}",
        if m.function { "function" } else { "procedure" },
        printable(&m.name),
        m.params,
        if optional > 0 {
            format!(", {optional} optional")
        } else {
            String::new()
        },
        place_text(m.place)
    )
}

fn command_line(c: &FormCommand) -> String {
    let name = printable(&c.name);
    match &c.run {
        CommandRun::Server(m) => format!("command: {name}: server (handler {})", printable(m)),
        CommandRun::ClientCallsServer { handler, method } => format!(
            "command: {name}: server (client handler {} calls {})",
            printable(handler),
            printable(method)
        ),
        CommandRun::Client {
            handler,
            calls,
            reason,
        } => {
            let handler = handler
                .as_ref()
                .map_or(String::new(), |h| format!(" (handler {})", printable(h)));
            let calls = if calls.is_empty() {
                String::new()
            } else {
                format!(
                    "; calls {}",
                    calls
                        .iter()
                        .map(|c| printable(c))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!(
                "command: {name}: client{handler}: {}{calls}",
                printable(reason)
            )
        }
    }
}

fn set_line(s: &SetValue, pres: Option<&Shown>) -> String {
    let old = s.old.as_ref().map_or(String::new(), |c| {
        format!("{} -> ", value_text(c, true, pres))
    });
    let new = match &s.new {
        Some(v) => value_text(v, true, pres),
        None if s.path.ends_with(']') && !s.path.contains("[+") => "(removed)".to_string(),
        None if s.path.ends_with(']') => "(added)".to_string(),
        None => "(cleared)".to_string(),
    };
    format!("set: {}: {old}{new}", printable(&s.path))
}

fn changed_lines(c: &Changed, pres: Option<&Shown>, l: &mut Vec<String>) {
    match c {
        Changed::Value { path, old, new } => {
            let old = old.as_ref().map_or(String::new(), |c| {
                format!("{} -> ", value_text(c, true, pres))
            });
            l.push(format!(
                "changed: {}: {old}{}",
                printable(path),
                value_text(new, true, pres)
            ));
        }
        Changed::Rows {
            path,
            count,
            columns,
            rows,
        } => {
            l.push(format!(
                "changed: {}: {count} rows, {} given",
                printable(path),
                rows.len()
            ));
            if !rows.is_empty() {
                l.push(format!("  N | {}", columns_text(columns)));
                l.extend(
                    rows.iter()
                        .map(|(i, r)| format!("  {} | {}", i + 1, row_text(r, pres))),
                );
            }
        }
        Changed::Sheet { path } => l.push(format!(
            "changed: {}: (spreadsheet document; remote report reads it)",
            printable(path)
        )),
        Changed::Other { path } => {
            l.push(format!("changed: {}: (a value not shown)", printable(path)))
        }
    }
}

fn message_line(m: &UserMessage) -> String {
    match &m.field {
        Some(f) => format!("message: [{}] {}", printable(f), printable(&m.text)),
        None => format!("message: {}", printable(&m.text)),
    }
}

pub(super) fn view_lines(v: &FormView, pres: Option<&Shown>) -> Vec<String> {
    let mut l = vec![format!("form: {}", printable(&v.form_id))];
    if let Some(n) = v.names.last() {
        l.push(format!("name: {}", printable(n)));
    }
    if let Some(t) = &v.title {
        l.push(format!("title: {}", printable(t)));
    }
    if v.read_only {
        l.push("read only: the user may not change the data of the form".to_string());
    }
    for a in &v.attributes {
        attribute_lines(a, pres, &mut l);
    }
    l.extend(v.methods.iter().map(method_line));
    l.extend(v.commands.iter().map(command_line));
    l.extend(
        v.opening_messages
            .iter()
            .map(|m| message_line(m).replacen("message:", "message (opening):", 1)),
    );
    for c in &v.calls {
        l.extend(c.set.iter().map(|s| set_line(s, pres)));
        l.push(format!(
            "call: {} ({}{})",
            printable(&c.method),
            place_text(c.place),
            c.command
                .as_ref()
                .map_or(String::new(), |n| format!(", command {}", printable(n)))
        ));
        match &c.outcome {
            CallOutcome::Done { ret, changed } => {
                if let Some(r) = ret {
                    l.push(format!("return: {}", cell_text(r, true, pres)));
                }
                for x in changed {
                    changed_lines(x, pres, &mut l);
                }
            }
            CallOutcome::Failed {
                reason,
                text,
                changed,
            } => {
                l.push(format!(
                    "failed: {}{}",
                    reason.as_ref().map_or(String::new(), |r| format!("{r}: ")),
                    printable(text)
                ));
                for x in changed {
                    let mut one = Vec::new();
                    changed_lines(x, pres, &mut one);
                    if let Some(first) = one.first_mut() {
                        *first = first.replacen("changed:", "changed (after the exception):", 1);
                    }
                    l.extend(one);
                }
            }
            CallOutcome::Unknown(t) => l.push(format!("unknown: {}", printable(t))),
        }
        l.extend(c.messages.iter().map(message_line));
    }
    if let Some(r) = &v.refused {
        l.push(format!("refused: {}", printable(r)));
    }
    l.extend(
        v.not_run
            .iter()
            .map(|s| format!("not run: {}", printable(s))),
    );
    l
}

fn cell_json(c: &Cell) -> Json {
    match c {
        Cell::Value(v) => values::to_json(v),
        Cell::Raw(t) => json!({ "raw": t }),
    }
}

fn value_json(v: &FormValue) -> Json {
    match v {
        FormValue::Cell(c) => cell_json(c),
        FormValue::Period(p) => period_json(p),
    }
}

fn period_json(p: &onec_client::Period) -> Json {
    json!({ "period": { "variant": p.variant, "start": p.start, "end": p.end } })
}

fn rows_json(columns: &[onec_client::FormField], rows: &[Vec<Cell>]) -> (Json, Json) {
    (
        json!(columns.iter().map(|c| c.display_name()).collect::<Vec<_>>()),
        json!(
            rows.iter()
                .map(|r| r.iter().map(cell_json).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        ),
    )
}

fn attribute_json(a: &FormAttr) -> Json {
    let mut j = json!({
        "id": a.attribute.field.id,
        "name": a.attribute.field.display_name(),
        "main": a.main,
    });
    match &a.value {
        AttrValue::Cell(c) => put(&mut j, "value", cell_json(c)),
        AttrValue::Period(p) => put(&mut j, "value", period_json(p)),
        AttrValue::Sheet => put(&mut j, "kind", json!("spreadsheet")),
        AttrValue::Other(_) => put(&mut j, "kind", json!("not shown")),
        AttrValue::Rows { columns, rows } => {
            let (c, r) = rows_json(columns, rows);
            put(&mut j, "columns", c);
            put(&mut j, "rows", r);
        }
        AttrValue::Object { fields, tables } => {
            put(&mut j, "fields", json!(fields
                .iter()
                .map(|(f, c)| json!({ "id": f.id, "name": f.display_name(), "value": cell_json(c) }))
                .collect::<Vec<_>>()));
            put(
                &mut j,
                "tables",
                json!(
                    tables
                        .iter()
                        .map(|(t, cols, rows)| {
                            let (c, r) = rows_json(cols, rows);
                            json!({ "id": t.id, "name": t.display_name(), "columns": c, "rows": r })
                        })
                        .collect::<Vec<_>>()
                ),
            );
        }
    }
    j
}

fn place_json(p: MethodPlace) -> &'static str {
    match p {
        MethodPlace::Server => "server",
        MethodPlace::ServerWithoutContext => "server_without_context",
    }
}

fn command_json(c: &FormCommand) -> Json {
    let run = match &c.run {
        CommandRun::Server(m) => json!({ "server": { "method": m } }),
        CommandRun::ClientCallsServer { handler, method } => {
            json!({ "client_calls_server": { "handler": handler, "method": method } })
        }
        CommandRun::Client {
            handler,
            calls,
            reason,
        } => json!({ "client": { "handler": handler, "calls": calls, "reason": reason } }),
    };
    json!({ "name": c.name, "handlers": c.handlers, "runs": c.run.runs(), "run": run })
}

fn changed_json(c: &Changed) -> Json {
    match c {
        Changed::Value { path, old, new } => json!({
            "path": path,
            "old": old.as_ref().map_or(Json::Null, value_json),
            "new": value_json(new),
        }),
        Changed::Rows {
            path,
            count,
            columns,
            rows,
        } => json!({
            "path": path,
            "count": count,
            "columns": columns.iter().map(|c| c.display_name()).collect::<Vec<_>>(),
            "rows": rows.iter().map(|(i, r)| json!({
                "line": i + 1,
                "values": r.iter().map(cell_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
        Changed::Sheet { path } => json!({ "path": path, "kind": "spreadsheet" }),
        Changed::Other { path } => json!({ "path": path, "kind": "not shown" }),
    }
}

fn call_json(c: &Called) -> Json {
    let mut j = json!({
        "method": c.method,
        "place": place_json(c.place),
        "command": c.command,
        "set": c.set.iter().map(|s| json!({
            "path": s.path,
            "old": s.old.as_ref().map_or(Json::Null, value_json),
            "new": s.new.as_ref().map_or(Json::Null, value_json),
        })).collect::<Vec<_>>(),
        "messages": c.messages.iter().map(|m| json!({ "text": m.text, "field": m.field })).collect::<Vec<_>>(),
    });
    match &c.outcome {
        CallOutcome::Done { ret, changed } => {
            put(&mut j, "return", ret.as_ref().map_or(Json::Null, cell_json));
            put(
                &mut j,
                "changed",
                json!(changed.iter().map(changed_json).collect::<Vec<_>>()),
            );
        }
        CallOutcome::Failed {
            reason,
            text,
            changed,
        } => {
            put(&mut j, "failed", json!({ "reason": reason, "text": text }));
            put(
                &mut j,
                "changed",
                json!(changed.iter().map(changed_json).collect::<Vec<_>>()),
            );
        }
        CallOutcome::Unknown(t) => put(&mut j, "unknown", json!({ "text": t })),
    }
    j
}

pub(super) fn view_json(v: &FormView) -> Json {
    let mut j = json!({
        "form_id": v.form_id,
        "names": v.names,
        "title": v.title,
        "read_only": v.read_only,
        "attributes": v.attributes.iter().map(attribute_json).collect::<Vec<_>>(),
        "methods": v.methods.iter().map(|m| json!({
            "name": m.name,
            "function": m.function,
            "params": m.params,
            "required": m.required,
            "place": place_json(m.place),
        })).collect::<Vec<_>>(),
        "commands": v.commands.iter().map(command_json).collect::<Vec<_>>(),
        "opening_messages": v.opening_messages.iter().map(|m| json!({ "text": m.text, "field": m.field })).collect::<Vec<_>>(),
        "calls": v.calls.iter().map(call_json).collect::<Vec<_>>(),
        "refused": v.refused,
        "not_run": v.not_run,
        "warnings": v.warnings,
    });
    if let [c] = v.calls.as_slice() {
        let call = call_json(c);
        put(
            &mut j,
            "set",
            call.get("set").cloned().unwrap_or(Json::Null),
        );
        put(&mut j, "call", call);
    }
    j
}
