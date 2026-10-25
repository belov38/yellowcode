use std::io::Write;

use bsl_syntax::{ExecContext, NoBuiltins, ParseMode, ParseOptions, SourceText, parse};
use platform_catalog::messages::{Lang as MsgLang, render};
use serde::Serialize;

use super::out::Lang;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    pub kind: &'static str,
    pub name: String,
    pub params: Vec<String>,
    pub directives: Vec<String>,
    pub export: bool,
    pub is_async: bool,
    pub line: u32,
    pub end_line: Option<u32>,
    pub region: Option<String>,
}

const CONTEXTS: [ExecContext; 3] = [
    ExecContext::Server,
    ExecContext::ThinClient,
    ExecContext::WebClient,
];

pub fn outline(text: &str) -> (Vec<OutlineItem>, Option<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let src = SourceText::from_str("", text);
    let mut items: Vec<(u32, OutlineItem)> = Vec::new();
    let mut error = None;
    for ctx in CONTEXTS {
        let out = parse(
            &src,
            &ParseOptions {
                mode: ParseMode::Module,
                context: ctx,
                methods_only: false,
                extension: true,
                builtins: &NoBuiltins,
            },
        );
        if error.is_none()
            && let Some(e) = &out.error
        {
            let t =
                render(e.msg.0, e.arg.as_deref(), MsgLang::Ru).unwrap_or_else(|| match &e.arg {
                    Some(a) => format!("{} ({a})", e.msg.0),
                    None => e.msg.0.to_string(),
                });
            error = Some(format!("({},{}): {t}", e.pos.line, e.pos.col));
        }
        for m in &out.module.methods {
            if items.iter().any(|(s, _)| *s == m.span.start) {
                continue;
            }
            let region = out
                .module
                .regions
                .iter()
                .filter(|r| r.span.start <= m.span.start && m.span.start < r.span.end)
                .max_by_key(|r| r.span.start)
                .map(|r| r.name.name.clone());
            let directives = m
                .annotations
                .iter()
                .map(|a| match &a.arg {
                    Some(arg) => {
                        format!("&{}(\"{}\")", a.name.name, String::from_utf16_lossy(arg))
                    }
                    None => format!("&{}", a.name.name),
                })
                .collect();
            items.push((
                m.span.start,
                OutlineItem {
                    kind: match m.kind {
                        bsl_syntax::ast::MethodKind::Procedure => "Процедура",
                        bsl_syntax::ast::MethodKind::Function => "Функция",
                    },
                    name: m.name.name.clone(),
                    params: m
                        .params
                        .iter()
                        .map(|p| src.text(p.span).trim().to_string())
                        .collect(),
                    directives,
                    export: m.export,
                    is_async: m.async_kw.is_some(),
                    line: src.pos(m.span.start).line,
                    end_line: m.end_kw.map(|s| src.pos(s.start).line),
                    region,
                },
            ));
        }
    }
    items.sort_by_key(|(s, _)| *s);
    (items.into_iter().map(|(_, i)| i).collect(), error)
}

pub fn render_text(
    items: &[OutlineItem],
    error: Option<&str>,
    out: &mut dyn Write,
    lang: Lang,
) -> crate::Result<()> {
    let ru = lang == Lang::Ru;
    for i in items {
        write!(out, "{:>5}  ", i.line)?;
        for d in &i.directives {
            write!(out, "{d} ")?;
        }
        if i.is_async {
            write!(out, "{} ", if ru { "Асинх" } else { "Async" })?;
        }
        let kind = match (i.kind, ru) {
            ("Процедура", false) => "Procedure",
            ("Функция", false) => "Function",
            (k, _) => k,
        };
        write!(out, "{kind} {}({})", i.name, i.params.join(", "))?;
        if i.export {
            write!(out, " {}", if ru { "Экспорт" } else { "Export" })?;
        }
        if let Some(r) = &i.region {
            write!(out, "  — {r}")?;
        }
        writeln!(out)?;
    }
    if let Some(e) = error {
        writeln!(out, "{}: {e}", if ru { "Ошибка" } else { "Error" })?;
    }
    Ok(())
}
