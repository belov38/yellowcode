use std::io::{BufRead, Write};

use onec_client::auth::printable;
use onec_client::{
    Assign, AttrValue, CallOutcome, Called, Cell, Changed, CommandRun, FormAction, FormAttr,
    FormCommand, FormError, FormRequest, FormTarget, FormView, Input, MethodPlace, Ref,
    ServerMethod, SessionError, SetValue, UserMessage,
};
use serde_json::{Value as Json, json};

use super::{Param, open_session, show, steps_line, users_hint, values};
use crate::Result;
use crate::output::Out;

pub const UNKNOWN_EXIT: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSet {
    pub name: String,
    pub value: String,
}

impl FormSet {
    pub fn parse(arg: &str) -> std::result::Result<FormSet, String> {
        match arg.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() => Ok(FormSet {
                name: name.trim().to_string(),
                value: value.to_string(),
            }),
            _ => Err(format!(
                "not a change: {arg:?} (Реквизит=значение or Объект.Поле=значение)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FormRun {
    Show,
    Command(String),
    Call { name: String, params: Vec<Param> },
}

#[derive(Debug, Clone)]
pub struct FormCommandRequest {
    pub login: super::Login,
    pub form: Option<String>,
    pub reference: Option<Ref>,
    pub set: Vec<FormSet>,
    pub run: FormRun,
    pub json: bool,
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
    let action = match &r.run {
        FormRun::Show => FormAction::Show,
        FormRun::Command(c) => FormAction::Command(c.clone()),
        FormRun::Call { name, params } => FormAction::Call {
            name: name.clone(),
            params: params.iter().map(|Param(v)| v.clone()).collect(),
        },
    };
    let req = FormRequest {
        target,
        set: r
            .set
            .iter()
            .map(|s| Assign {
                name: s.name.clone(),
                input: Input::Text(s.value.clone()),
            })
            .collect(),
        action,
    };
    let (session, login) = open_session(&r.login, input)?;
    let (session, outcome) = super::on_value_stack(move || {
        let mut session = session;
        let outcome = login.and_then(|()| session.run_form(&req));
        let _ = session.logout();
        (session, outcome)
    })?;
    let view = match outcome {
        Ok(v) => v,
        Err(SessionError::Form(FormError::Sent(m))) => {
            if r.json {
                let j = json!({ "outcome": { "unknown": { "text": m } }, "steps": steps_json(&session) });
                writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
            } else {
                writeln!(out, "unknown: {}", printable(&m))?;
                writeln!(out, "RESULT UNKNOWN {}", steps_line(&session))?;
            }
            writeln!(err, "yellowcode: {}", FormError::Sent(m))?;
            return Ok(UNKNOWN_EXIT);
        }
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            return Err(e.into());
        }
    };
    let failed = match &view.called {
        Some(Called {
            outcome: CallOutcome::Failed { reason, text },
            ..
        }) => Some(format!(
            "the call failed on the server: {}{}",
            reason.as_ref().map_or(String::new(), |r| format!("{r}: ")),
            printable(text)
        )),
        _ => None,
    };
    if r.json {
        let mut j = view_json(&view);
        put(&mut j, "steps", steps_json(&session));
        writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
    } else {
        for line in view_lines(&view) {
            writeln!(out, "{line}")?;
        }
        let status = if failed.is_some() { "FAIL" } else { "OK" };
        writeln!(out, "RESULT {status} {}", steps_line(&session))?;
    }
    for w in &view.warnings {
        writeln!(err, "warning: {}", printable(w))?;
    }
    match failed {
        Some(f) => Err(f.into()),
        None => Ok(0),
    }
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

fn cell_text(c: &Cell, inner: bool) -> String {
    match c {
        Cell::Value(onec_client::Value::String(t)) if !inner => printable(t),
        Cell::Value(v) => show(v, inner, None),
        Cell::Raw(t) => format!("raw:{t:?}"),
    }
}

fn row_text(cells: &[Cell]) -> String {
    cells
        .iter()
        .map(|c| cell_text(c, true))
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

fn attribute_lines(a: &FormAttr, l: &mut Vec<String>) {
    let name = printable(a.attribute.field.display_name());
    match &a.value {
        AttrValue::Cell(c) => l.push(format!("attribute: {name}: {}", cell_text(c, false))),
        AttrValue::Sheet => l.push(format!("attribute: {name}: (spreadsheet document)")),
        AttrValue::Other(_) => l.push(format!("attribute: {name}: (a value not shown)")),
        AttrValue::Object { fields, tables } => {
            l.push(format!(
                "attribute: {name}{}",
                if a.main { " (main)" } else { "" }
            ));
            for (f, c) in fields {
                l.push(format!(
                    "  {name}.{}: {}",
                    printable(f.display_name()),
                    cell_text(c, false)
                ));
            }
            for (t, columns, rows) in tables {
                l.push(format!(
                    "  {name}.{}: {} rows",
                    printable(t.display_name()),
                    rows.len()
                ));
                if !rows.is_empty() {
                    l.push(format!("    {}", columns_text(columns)));
                    l.extend(rows.iter().map(|r| format!("    {}", row_text(r))));
                }
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

fn set_line(s: &SetValue) -> String {
    let old = s
        .old
        .as_ref()
        .map_or(String::new(), |c| format!("{} -> ", cell_text(c, true)));
    format!(
        "set: {}: {old}{}",
        printable(&s.path),
        show(&s.new, true, None)
    )
}

fn changed_lines(c: &Changed, l: &mut Vec<String>) {
    match c {
        Changed::Value { path, old, new } => {
            let old = old
                .as_ref()
                .map_or(String::new(), |c| format!("{} -> ", cell_text(c, true)));
            l.push(format!(
                "changed: {}: {old}{}",
                printable(path),
                cell_text(new, true)
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
                        .map(|(i, r)| format!("  {} | {}", i + 1, row_text(r))),
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

pub(super) fn view_lines(v: &FormView) -> Vec<String> {
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
        attribute_lines(a, &mut l);
    }
    l.extend(v.methods.iter().map(method_line));
    l.extend(v.commands.iter().map(command_line));
    l.extend(
        v.opening_messages
            .iter()
            .map(|m| message_line(m).replacen("message:", "message (opening):", 1)),
    );
    l.extend(v.set.iter().map(set_line));
    if let Some(c) = &v.called {
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
                    l.push(format!("return: {}", cell_text(r, true)));
                }
                for x in changed {
                    changed_lines(x, &mut l);
                }
            }
            CallOutcome::Failed { reason, text } => l.push(format!(
                "failed: {}{}",
                reason.as_ref().map_or(String::new(), |r| format!("{r}: ")),
                printable(text)
            )),
        }
        l.extend(c.messages.iter().map(message_line));
    }
    l
}

fn cell_json(c: &Cell) -> Json {
    match c {
        Cell::Value(v) => values::to_json(v),
        Cell::Raw(t) => json!({ "raw": t }),
    }
}

fn attribute_json(a: &FormAttr) -> Json {
    let mut j = json!({
        "id": a.attribute.field.id,
        "name": a.attribute.field.display_name(),
        "main": a.main,
    });
    match &a.value {
        AttrValue::Cell(c) => put(&mut j, "value", cell_json(c)),
        AttrValue::Sheet => put(&mut j, "kind", json!("spreadsheet")),
        AttrValue::Other(_) => put(&mut j, "kind", json!("not shown")),
        AttrValue::Object { fields, tables } => {
            put(&mut j, "fields", json!(fields
                .iter()
                .map(|(f, c)| json!({ "id": f.id, "name": f.display_name(), "value": cell_json(c) }))
                .collect::<Vec<_>>()));
            put(&mut j, "tables", json!(tables
                .iter()
                .map(|(t, cols, rows)| json!({
                    "id": t.id,
                    "name": t.display_name(),
                    "columns": cols.iter().map(|c| c.display_name()).collect::<Vec<_>>(),
                    "rows": rows.iter().map(|r| r.iter().map(cell_json).collect::<Vec<_>>()).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()));
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
            "old": old.as_ref().map_or(Json::Null, cell_json),
            "new": cell_json(new),
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

pub(super) fn view_json(v: &FormView) -> Json {
    let call = v.called.as_ref().map(|c| {
        let mut j = json!({
            "method": c.method,
            "place": place_json(c.place),
            "command": c.command,
            "messages": c.messages.iter().map(|m| json!({ "text": m.text, "field": m.field })).collect::<Vec<_>>(),
        });
        match &c.outcome {
            CallOutcome::Done { ret, changed } => {
                put(&mut j, "return", ret.as_ref().map_or(Json::Null, cell_json));
                put(&mut j, "changed", json!(changed.iter().map(changed_json).collect::<Vec<_>>()));
            }
            CallOutcome::Failed { reason, text } => {
                put(&mut j, "failed", json!({ "reason": reason, "text": text }));
            }
        }
        j
    });
    json!({
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
        "set": v.set.iter().map(|s| json!({
            "path": s.path,
            "old": s.old.as_ref().map_or(Json::Null, cell_json),
            "new": values::to_json(&s.new),
        })).collect::<Vec<_>>(),
        "opening_messages": v.opening_messages.iter().map(|m| json!({ "text": m.text, "field": m.field })).collect::<Vec<_>>(),
        "call": call,
        "warnings": v.warnings,
    })
}
