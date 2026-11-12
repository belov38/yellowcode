use std::io::Write;
use std::path::PathBuf;

use mdconf::{Configuration, LoadOptions};
use serde_json::json;

use super::metadata::Format;
use super::modules::{ModuleRef, modules, resolve_module};
use super::scope::{Scope, ScopeArgs};
use crate::viewer::names::{fold_path_name, module_name};
use crate::viewer::out::Lang;
use crate::viewer::outline::{OutlineItem, outline, render_text};
use crate::viewer::source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Whole,
    Lines { first: usize, last: Option<usize> },
    Procedure(String),
}

pub enum Command {
    List {
        source: PathBuf,
        scope: ScopeArgs,
        format: Format,
    },
    Show {
        source: PathBuf,
        module: String,
        selection: Selection,
        numbers: bool,
        format: Format,
    },
    Outline {
        source: PathBuf,
        module: String,
        format: Format,
    },
}

pub fn parse_lines(s: &str) -> Result<Selection, String> {
    let (a, b) = s
        .split_once(':')
        .ok_or_else(|| format!("--lines: expected FIRST:LAST or FIRST:, got {s:?}"))?;
    let number = |part: &str| {
        part.trim()
            .parse::<usize>()
            .map_err(|_| format!("--lines: {part:?} is not a line number"))
    };
    let first = number(a)?;
    if first < 1 {
        return Err("--lines: line numbers start at 1".into());
    }
    let last = if b.trim().is_empty() {
        None
    } else {
        Some(number(b)?)
    };
    if let Some(last) = last
        && first > last
    {
        return Err(format!(
            "--lines: the first line {first} is after the last {last}"
        ));
    }
    Ok(Selection::Lines { first, last })
}

pub fn line_spans(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c == '\n' || c == '\r' {
            let mut end = i + 1;
            if c == '\r'
                && let Some(&(_, '\n')) = it.peek()
            {
                it.next();
                end += 1;
            }
            out.push(text.get(start..end).unwrap_or_default());
            start = end;
        }
    }
    if start < text.len() {
        out.push(text.get(start..).unwrap_or_default());
    }
    out
}

pub(super) fn write_json(out: &mut dyn Write, value: &serde_json::Value) -> crate::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

fn procedure_start(spans: &[&str], line: usize) -> usize {
    let mut first = line.max(1);
    while first > 1 {
        let above = spans.get(first - 2).map_or("", |s| s.trim_start());
        if above.starts_with("//") || above.starts_with('&') {
            first -= 1;
        } else {
            break;
        }
    }
    first
}

fn procedure_range(
    m: &ModuleRef<'_>,
    spans: &[&str],
    name: &str,
    items: &[OutlineItem],
    syntax_error: Option<&str>,
) -> crate::Result<(usize, usize)> {
    let wanted = fold_path_name(name);
    let found: Vec<&OutlineItem> = items
        .iter()
        .filter(|i| fold_path_name(&i.name) == wanted)
        .collect();
    match found.as_slice() {
        [] => Err(format!("no procedure or function {name} in {}", m.path).into()),
        [item] => {
            let Some(end) = item.end_line else {
                return Err(format!(
                    "{name} in {} is cut by a syntax error: {}",
                    m.path,
                    syntax_error.unwrap_or("no end of the method")
                )
                .into());
            };
            let end = end as usize;
            if end > spans.len() {
                return Err(format!(
                    "procedure {name} ends at line {end}, past the end of the module ({} lines)",
                    spans.len()
                )
                .into());
            }
            Ok((procedure_start(spans, item.line as usize), end))
        }
        several => {
            let lines: Vec<String> = several.iter().map(|i| i.line.to_string()).collect();
            Err(format!(
                "several procedures {name} at lines {}; choose with --lines",
                lines.join(", ")
            )
            .into())
        }
    }
}

fn show_module(
    cfg: &Configuration,
    module: &str,
    selection: &Selection,
    numbers: bool,
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let m = resolve_module(cfg, module)?;
    let text = m.module.text();
    let spans = line_spans(&text);
    let count = spans.len();
    let (first, last) = match selection {
        Selection::Whole => (usize::from(count > 0), count),
        Selection::Lines { first, last } => {
            if *first == 0 {
                return Err("--lines: lines start at 1".into());
            }
            if let Some(last) = last
                && first > last
            {
                return Err(
                    format!("--lines: the first line {first} is after the last {last}").into(),
                );
            }
            if *first > count {
                return Err(
                    format!("line {first} is past the end of the module ({count} lines)").into(),
                );
            }
            (*first, last.map_or(count, |l| l.min(count)))
        }
        Selection::Procedure(name) => {
            let (items, error) = outline(&text);
            procedure_range(&m, &spans, name, &items, error.as_deref())?
        }
    };
    let selected = if first == 0 {
        &[][..]
    } else {
        spans.get(first - 1..last).unwrap_or_default()
    };
    match format {
        Format::Json => {
            write_json(
                out,
                &json!({
                    "schemaVersion": 1,
                    "path": m.path,
                    "module": module_name(m.kind, Lang::En),
                    "firstLine": first,
                    "lastLine": last,
                    "lineCount": count,
                    "text": selected.concat(),
                }),
            )?;
        }
        Format::Text if numbers => {
            for (n, line) in (first..).zip(selected) {
                write!(out, "{n:>6}\t{line}")?;
            }
        }
        Format::Text => out.write_all(selected.concat().as_bytes())?,
    }
    Ok(())
}

fn list_modules(
    cfg: &Configuration,
    scope: &ScopeArgs,
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let scope = Scope::resolve(cfg, scope)?;
    let refs = modules(cfg, &scope);
    match format {
        Format::Text => {
            for r in &refs {
                writeln!(out, "{}", r.path)?;
            }
        }
        Format::Json => {
            let rows: Vec<_> = refs
                .iter()
                .map(|r| {
                    json!({
                        "path": r.path,
                        "owner": r.owner,
                        "module": module_name(r.kind, Lang::En),
                        "lines": line_spans(&r.module.text()).len(),
                    })
                })
                .collect();
            write_json(out, &json!({"schemaVersion": 1, "modules": rows}))?;
        }
    }
    Ok(())
}

fn outline_module(
    cfg: &Configuration,
    module: &str,
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let m = resolve_module(cfg, module)?;
    let (items, error) = outline(&m.module.text());
    match format {
        Format::Text => render_text(&items, error.as_deref(), out, Lang::Ru)?,
        Format::Json => write_json(
            out,
            &json!({
                "schemaVersion": 1,
                "path": m.path,
                "items": items,
                "syntaxError": error,
            }),
        )?,
    }
    Ok(())
}

pub fn run_with_memory_limit(
    command: Command,
    limit: Option<String>,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let memory = source::OpenMemory::resolve(limit)?;
    let open = |path: &PathBuf| {
        source::open_with_memory(path, &LoadOptions::default(), &memory, &mut |_| Ok(()))
    };
    match command {
        Command::List {
            source,
            scope,
            format,
        } => list_modules(&open(&source)?, &scope, format, out),
        Command::Show {
            source,
            module,
            selection,
            numbers,
            format,
        } => show_module(&open(&source)?, &module, &selection, numbers, format, out),
        Command::Outline {
            source,
            module,
            format,
        } => outline_module(&open(&source)?, &module, format, out),
    }
}
