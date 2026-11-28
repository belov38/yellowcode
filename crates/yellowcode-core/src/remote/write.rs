use std::io::{BufRead, Write};
use std::path::PathBuf;

use onec_client::auth::printable;
use onec_client::{
    Applied, Assign, Cell, Input, ObjectChange, Outcome, Ref, RowEdit, TableEdit, Target,
    UserMessage, Value, WriteMode, WriteRequest, WriteResult,
};
use onec_client::{ObjectError, SessionError};
use serde_json::{Value as Json, json};

use super::{open_session, show, steps_line, users_hint, values};
use crate::Result;
use crate::output::Out;

pub const UNKNOWN_EXIT: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    New(String),
    Existing(Ref),
}

impl WriteTarget {
    pub fn parse_new(arg: &str) -> std::result::Result<WriteTarget, String> {
        onec_client::write::new_object_ref(arg)?;
        Ok(WriteTarget::New(arg.to_string()))
    }

    pub fn parse_existing(arg: &str) -> std::result::Result<WriteTarget, String> {
        onec_client::object::parse_ref(arg).map(WriteTarget::Existing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Set {
        row: Option<(String, usize, bool)>,
        name: String,
        value: String,
    },
    DeleteRow {
        table: String,
        line: usize,
    },
    ClearTable(String),
}

impl Edit {
    pub fn parse_set(arg: &str) -> std::result::Result<Edit, String> {
        let bad = || format!("not a change: {arg:?} (Поле=значение or ТЧ[N].Поле=значение)");
        let (left, value) = arg.split_once('=').ok_or_else(bad)?;
        let (row, name) = match left.split_once('[') {
            None => (None, left.to_string()),
            Some((table, rest)) => {
                let (index, col) = rest
                    .split_once("].")
                    .ok_or_else(|| format!("not a cell: {left:?} (ТЧ[N].Поле or ТЧ[+N].Поле)"))?;
                let (line, added) = line_of(index, left)?;
                (Some((table.to_string(), line, added)), col.to_string())
            }
        };
        if name.is_empty() || row.as_ref().is_some_and(|(t, _, _)| t.is_empty()) {
            return Err(bad());
        }
        Ok(Edit::Set {
            row,
            name,
            value: value.to_string(),
        })
    }

    pub fn parse_delete(arg: &str) -> std::result::Result<Edit, String> {
        let bad = || format!("not a row: {arg:?} (ТЧ[N], N the line number)");
        let (table, rest) = arg.split_once('[').ok_or_else(bad)?;
        let index = rest.strip_suffix(']').ok_or_else(bad)?;
        let (line, added) = line_of(index, arg)?;
        if added || table.is_empty() {
            return Err(bad());
        }
        Ok(Edit::DeleteRow {
            table: table.to_string(),
            line,
        })
    }

    pub fn parse_clear_path(arg: &str) -> std::result::Result<Edit, String> {
        if arg.is_empty()
            || arg.contains(['[', ']', '='])
            || arg.starts_with('.')
            || arg.ends_with('.')
        {
            return Err(format!("not a table: {arg:?}"));
        }
        Ok(Edit::ClearTable(arg.to_string()))
    }

    pub fn parse_clear(arg: &str) -> std::result::Result<Edit, String> {
        if arg.is_empty() || arg.contains(['[', ']', '.', '=']) {
            return Err(format!("not a tabular section: {arg:?}"));
        }
        Ok(Edit::ClearTable(arg.to_string()))
    }
}

fn line_of(index: &str, arg: &str) -> std::result::Result<(usize, bool), String> {
    let (digits, added) = match index.strip_prefix('+') {
        Some(d) => (d, true),
        None => (index, false),
    };
    match digits.parse::<usize>() {
        Ok(n) if n > 0 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok((n, added)),
        _ => Err(format!(
            "not a row number: {index:?} in {arg:?} (1, 2, … or +1, +2, … for added rows)"
        )),
    }
}

#[derive(Debug, Clone)]
pub struct WriteCommand {
    pub login: super::Login,
    pub target: WriteTarget,
    pub edits: Vec<Edit>,
    pub data: Option<PathBuf>,
    pub mode: WriteMode,
    pub deletion_mark: Option<bool>,
    pub dry_run: bool,
    pub json: bool,
}

fn change_of_json(text: &str) -> std::result::Result<ObjectChange, String> {
    let j: Json = serde_json::from_str(text).map_err(|e| format!("--data: {e}"))?;
    let obj = j.as_object().ok_or("--data: not a JSON object")?;
    let mut change = ObjectChange::default();
    for (k, v) in obj {
        match k.as_str() {
            "set" => change.set = assigns(v, "set")?,
            "tables" => {
                let tables = v.as_object().ok_or("--data: tables is not an object")?;
                for (name, t) in tables {
                    change.tables.push(table_of_json(name, t)?);
                }
            }
            other => return Err(format!("--data: unknown key {other:?} (set, tables)")),
        }
    }
    Ok(change)
}

fn assigns(v: &Json, at: &str) -> std::result::Result<Vec<Assign>, String> {
    let o = v
        .as_object()
        .ok_or_else(|| format!("--data: {at} is not an object"))?;
    o.iter()
        .map(|(name, v)| {
            Ok(Assign {
                name: name.clone(),
                input: input_of_json(v).map_err(|e| format!("--data: {at}.{name}: {e}"))?,
            })
        })
        .collect()
}

fn input_of_json(v: &Json) -> std::result::Result<Input, String> {
    match v {
        Json::String(s) => Ok(Input::Text(s.clone())),
        other => super::parse_param_json(&other.to_string()).map(Input::Value),
    }
}

fn table_of_json(name: &str, t: &Json) -> std::result::Result<TableEdit, String> {
    let o = t
        .as_object()
        .ok_or_else(|| format!("--data: tables.{name} is not an object"))?;
    let mut edit = TableEdit {
        name: name.to_string(),
        clear: false,
        rows: Vec::new(),
    };
    for (k, v) in o {
        match k.as_str() {
            "clear" => {
                edit.clear = v
                    .as_bool()
                    .ok_or_else(|| format!("--data: tables.{name}.clear is not a boolean"))?
            }
            "rows" => {
                let rows = v
                    .as_array()
                    .ok_or_else(|| format!("--data: tables.{name}.rows is not an array"))?;
                for (i, r) in rows.iter().enumerate() {
                    edit.rows.push(
                        row_of_json(r)
                            .map_err(|e| format!("--data: tables.{name}.rows[{i}]: {e}"))?,
                    );
                }
            }
            other => {
                return Err(format!(
                    "--data: tables.{name}: unknown key {other:?} (clear, rows)"
                ));
            }
        }
    }
    Ok(edit)
}

fn row_of_json(r: &Json) -> std::result::Result<RowEdit, String> {
    let o = r.as_object().ok_or("not an object")?;
    let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let line = || -> std::result::Result<usize, String> {
        o.get("line")
            .and_then(Json::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| "line is not a row number (1, 2, …)".to_string())
    };
    let cells = |key: &str| assigns(&o[key], key).map_err(|e| e.replace("--data: ", ""));
    match keys.as_slice() {
        ["add"] => Ok(RowEdit::Add { cells: cells("add")? }),
        ["line", "set"] => Ok(RowEdit::Set {
            line: line()?,
            cells: cells("set")?,
        }),
        ["delete", "line"] if o["delete"] == Json::Bool(true) => {
            Ok(RowEdit::Delete { line: line()? })
        }
        _ => Err(
            "a row is {\"add\": {…}}, {\"line\": N, \"set\": {…}} or {\"line\": N, \"delete\": true}"
                .into(),
        ),
    }
}

fn table_mut<'a>(tables: &'a mut Vec<TableEdit>, name: &str) -> Option<&'a mut TableEdit> {
    let same = |t: &TableEdit| t.name.to_lowercase() == name.to_lowercase();
    if !tables.iter().any(same) {
        tables.push(TableEdit {
            name: name.to_string(),
            clear: false,
            rows: Vec::new(),
        });
    }
    tables.iter_mut().find(|t| same(t))
}

pub(super) fn add_edits(
    change: &mut ObjectChange,
    edits: &[Edit],
    deletion_mark: Option<bool>,
) -> std::result::Result<(), String> {
    let mut added: Vec<(String, usize, Vec<Assign>)> = Vec::new();
    for e in edits {
        match e {
            Edit::Set {
                row: None,
                name,
                value,
            } => change.set.push(Assign {
                name: name.clone(),
                input: Input::Text(value.clone()),
            }),
            Edit::Set {
                row: Some((table, line, false)),
                name,
                value,
            } => {
                let Some(t) = table_mut(&mut change.tables, table) else {
                    continue;
                };
                let cell = Assign {
                    name: name.clone(),
                    input: Input::Text(value.clone()),
                };
                match t
                    .rows
                    .iter_mut()
                    .find(|r| matches!(r, RowEdit::Set { line: l, .. } if l == line))
                {
                    Some(RowEdit::Set { cells, .. }) => cells.push(cell),
                    _ => t.rows.push(RowEdit::Set {
                        line: *line,
                        cells: vec![cell],
                    }),
                }
            }
            Edit::Set {
                row: Some((table, n, true)),
                name,
                value,
            } => {
                let cell = Assign {
                    name: name.clone(),
                    input: Input::Text(value.clone()),
                };
                match added
                    .iter_mut()
                    .find(|(t, k, _)| t.to_lowercase() == table.to_lowercase() && k == n)
                {
                    Some((_, _, cells)) => cells.push(cell),
                    None => added.push((table.clone(), *n, vec![cell])),
                }
            }
            Edit::DeleteRow { table, line } => {
                if let Some(t) = table_mut(&mut change.tables, table) {
                    t.rows.push(RowEdit::Delete { line: *line });
                }
            }
            Edit::ClearTable(table) => {
                if let Some(t) = table_mut(&mut change.tables, table) {
                    t.clear = true;
                }
            }
        }
    }
    added.sort_by_key(|(_, n, _)| *n);
    let mut tables: Vec<String> = added.iter().map(|(t, _, _)| t.to_lowercase()).collect();
    tables.dedup();
    tables.sort();
    tables.dedup();
    for t in &tables {
        let numbers: Vec<usize> = added
            .iter()
            .filter(|(x, _, _)| x.to_lowercase() == *t)
            .map(|(_, n, _)| *n)
            .collect();
        if numbers.iter().enumerate().any(|(i, n)| *n != i + 1) {
            return Err(format!(
                "the added rows of {t} are numbered {numbers:?}: number them 1, 2, … without a gap"
            ));
        }
    }
    for (table, _, cells) in added {
        if let Some(t) = table_mut(&mut change.tables, &table) {
            t.rows.push(RowEdit::Add { cells });
        }
    }
    if let Some(mark) = deletion_mark {
        change.set.push(Assign {
            name: "DeletionMark".to_string(),
            input: Input::Value(Value::Bool(mark)),
        });
    }
    Ok(())
}

fn change_of(r: &WriteCommand, input: &mut dyn BufRead) -> Result<ObjectChange> {
    let mut change = match &r.data {
        None => ObjectChange::default(),
        Some(p) if p.as_os_str() == "-" => {
            let mut text = String::new();
            input.read_to_string(&mut text)?;
            change_of_json(&text)?
        }
        Some(p) => {
            let text =
                std::fs::read_to_string(p).map_err(|e| format!("--data {}: {e}", p.display()))?;
            change_of_json(&text)?
        }
    };
    add_edits(&mut change, &r.edits, r.deletion_mark)?;
    Ok(change)
}

pub(super) fn run(
    r: WriteCommand,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    if r.data.as_ref().is_some_and(|p| p.as_os_str() == "-")
        && r.login.user.is_some()
        && matches!(r.login.password, super::Password::Stdin)
    {
        return Err(
            "--data - reads the changes from stdin: give the password by --password-env".into(),
        );
    }
    let change = change_of(&r, input)?;
    let target = match &r.target {
        WriteTarget::New(name) => Target::New(name.clone()),
        WriteTarget::Existing(r) => Target::Existing(r.clone()),
    };
    let req = WriteRequest {
        target,
        change,
        mode: r.mode,
        dry_run: r.dry_run,
    };
    let (mut session, login) = open_session(&r.login, input)?;
    let outcome = login.and_then(|()| session.write_object(&req));
    let _ = session.logout();
    let result = match outcome {
        Ok(w) => w,
        Err(SessionError::Object(ObjectError::WriteSent(m))) => {
            if r.json {
                let j = json!({ "object": target_text(&r.target), "outcome": { "unknown": { "text": m } } });
                writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
            } else {
                writeln!(out, "unknown: {}", printable(&m))?;
                writeln!(out, "RESULT UNKNOWN {}", steps_line(&session))?;
            }
            writeln!(err, "yellowcode: {}", ObjectError::WriteSent(m))?;
            return Ok(UNKNOWN_EXIT);
        }
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            return Err(e.into());
        }
    };
    let refused = matches!(result.outcome, Outcome::Refused { .. });
    if r.json {
        let j = result_json(&r.target, &result, &session);
        writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
    } else {
        for line in result_lines(&r.target, &result) {
            writeln!(out, "{line}")?;
        }
        let status = if refused { "FAIL" } else { "OK" };
        writeln!(out, "RESULT {status} {}", steps_line(&session))?;
    }
    for w in &result.warnings {
        writeln!(err, "warning: {}", printable(w))?;
    }
    match result.outcome {
        Outcome::Refused {
            reason: Some(reason),
            text,
        } => Err(format!(
            "the server did not write the object: {reason}: {}",
            printable(&text)
        )
        .into()),
        Outcome::Refused { reason: None, text } => Err(printable(&text).into()),
        _ => Ok(0),
    }
}

fn target_text(t: &WriteTarget) -> String {
    match t {
        WriteTarget::New(name) => format!("new {}", printable(name)),
        WriteTarget::Existing(r) => format!("{}({})", r.type_name(), r.uuid()),
    }
}

fn old_text(c: &Option<Cell>) -> String {
    match c {
        Some(Cell::Value(v)) => show(v, true, None),
        Some(Cell::Raw(t)) => format!("raw:{t:?}"),
        None => String::new(),
    }
}

fn applied_line(a: &Applied) -> String {
    let path = printable(&a.path);
    match (&a.old, &a.new) {
        (_, None) if a.path.contains("[+") => format!("change: {path}: added"),
        (_, None) if a.path.contains('[') => format!("change: {path}: removed"),
        (_, None) => format!("change: {path}: cleared"),
        (None, Some(v)) => format!("change: {path}: {}", show(v, true, None)),
        (old, Some(v)) => format!(
            "change: {path}: {} -> {}",
            old_text(old),
            show(v, true, None)
        ),
    }
}

fn message_line(m: &UserMessage) -> String {
    match &m.field {
        Some(f) => format!("message: [{}] {}", printable(f), printable(&m.text)),
        None => format!("message: {}", printable(&m.text)),
    }
}

fn result_lines(t: &WriteTarget, r: &WriteResult) -> Vec<String> {
    let mut l = vec![format!("object: {}", target_text(t))];
    if let Some(title) = &r.title {
        l.push(format!("title: {}", printable(title)));
    }
    l.push(format!("form: {}", printable(&r.form_id)));
    if let Some(m) = r.document_mode {
        l.push(format!("mode: {m}"));
    }
    l.extend(r.changes.iter().map(applied_line));
    match &r.outcome {
        Outcome::DryRun { body } => {
            l.push("dry run: the write was not sent; its body:".to_string());
            l.push(
                body.trim_start_matches('\u{feff}')
                    .replace(['\r', '\n'], " "),
            );
        }
        Outcome::Written {
            reference,
            code,
            data_version,
            presentation,
        } => {
            l.push(format!(
                "written: {}({})",
                reference.type_name(),
                reference.uuid()
            ));
            if let Some(c) = code {
                l.push(format!("code: {}", printable(c)));
            }
            if let Some(v) = data_version {
                l.push(format!("version: {}", printable(v)));
            }
            l.push(format!("presentation: {}", printable(presentation)));
        }
        Outcome::Refused { reason, text } => l.push(format!(
            "refused: {}{}",
            reason.as_ref().map_or(String::new(), |r| format!("{r}: ")),
            printable(text)
        )),
    }
    l.extend(r.messages.iter().map(message_line));
    l
}

fn cell_json(c: &Option<Cell>) -> Json {
    match c {
        Some(Cell::Value(v)) => values::to_json(v),
        Some(Cell::Raw(t)) => json!({ "raw": t }),
        None => Json::Null,
    }
}

fn result_json(t: &WriteTarget, r: &WriteResult, session: &onec_client::Session) -> Json {
    let changes: Vec<Json> = r
        .changes
        .iter()
        .map(|a| {
            json!({
                "path": a.path,
                "old": cell_json(&a.old),
                "new": a.new.as_ref().map_or(Json::Null, values::to_json),
            })
        })
        .collect();
    let outcome = match &r.outcome {
        Outcome::DryRun { body } => {
            json!({ "dry_run": { "body": body.trim_start_matches('\u{feff}') } })
        }
        Outcome::Written {
            reference,
            code,
            data_version,
            presentation,
        } => json!({ "written": {
            "ref": [reference.type_name().to_string(), reference.uuid().to_string()],
            "code": code,
            "version": data_version,
            "presentation": presentation,
        } }),
        Outcome::Refused { reason, text } => {
            json!({ "refused": { "reason": reason, "text": text } })
        }
    };
    let messages: Vec<Json> = r
        .messages
        .iter()
        .map(|m| json!({ "text": m.text, "field": m.field }))
        .collect();
    let steps: Vec<Json> = session
        .steps()
        .iter()
        .map(|s| json!({ "name": s.name, "status": s.status }))
        .collect();
    json!({
        "object": target_text(t),
        "form_id": r.form_id,
        "title": r.title,
        "mode": r.document_mode,
        "changes": changes,
        "outcome": outcome,
        "messages": messages,
        "warnings": r.warnings,
        "steps": steps,
    })
}
