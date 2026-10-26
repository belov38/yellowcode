mod resolve;
use super::{Builder, CompletionItem, CompletionList, complete};
use crate::viewer::Lang;
use bsl_syntax::{
    ExecContext, NoBuiltins, ParseMode, ParseOptions, SourceText, TokenKind as T,
    ast::{ExprKind, StmtKind},
    parse, tokenize,
};
use mdconf::Configuration;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug)]
pub enum AnalysisKind {
    Completion,
    Signature,
}
#[derive(Debug, Serialize)]
pub struct AnalysisResult {
    pub items: Vec<CompletionItem>,
    pub total: usize,
    pub from: usize,
    pub to: usize,
    #[serde(rename = "analysisStatus")]
    pub analysis_status: &'static str,
    pub argument: Option<usize>,
    pub anchor: Option<usize>,
}
pub fn valid_position(text: &str, p: usize) -> bool {
    let mut n = 0;
    for c in text.chars() {
        if n == p {
            return true;
        }
        n += c.len_utf16();
        if n > p {
            return false;
        }
    }
    n == p
}
fn parsed(units: &[u16], mode: ParseMode) -> bsl_syntax::ParseOutput {
    parse(
        &SourceText::from_utf16("editor", units.to_vec()),
        &ParseOptions {
            mode,
            context: ExecContext::Server,
            methods_only: false,
            extension: false,
            builtins: &NoBuiltins,
        },
    )
}
fn expression(units: &[u16]) -> Option<bsl_syntax::ast::Expr> {
    let mut p = parsed(units, ParseMode::Expression);
    if p.error.is_some() {
        return None;
    }
    match p.module.body.pop()?.kind {
        StmtKind::Value(e) => Some(e),
        _ => None,
    }
}
fn list(prefix: &str, lang: Lang) -> Builder {
    Builder {
        prefix: bsl_prim::fold_name(prefix),
        lang,
        result: CompletionList::default(),
        seen: Default::default(),
        limit: super::MAX_ITEMS,
    }
}
pub fn analyze(
    cfg: &Configuration,
    text: &str,
    position_utf16: usize,
    kind: AnalysisKind,
    lang: Lang,
) -> AnalysisResult {
    let p = position_utf16;
    let mut r = AnalysisResult {
        items: vec![],
        total: 0,
        from: p,
        to: p,
        analysis_status: "unknown",
        argument: None,
        anchor: None,
    };
    if text.len() > 256 * 1024 || !valid_position(text, p) {
        r.analysis_status = "limited";
        return r;
    }
    let mut u: Vec<u16> = text.encode_utf16().take(p).collect();
    if let Some(first) = u.first_mut()
        && *first == 0xfeff
    {
        *first = 32;
    }
    let src = SourceText::from_utf16("editor", u.clone());
    let lex = tokenize(&src);
    if lex.tokens.len() > 32768 {
        r.analysis_status = "limited";
        return r;
    }
    let mut ts = lex.tokens;
    ts.retain(|t| t.kind != T::Eof);
    if ts.is_empty()
        && u.iter()
            .all(|c| char::from_u32(u32::from(*c)).is_some_and(char::is_whitespace))
        && matches!(kind, AnalysisKind::Completion)
    {
        let list = complete(cfg, &[], "", false, lang);
        r.items = list.items;
        r.total = list.total;
        r.analysis_status = "complete";
        return r;
    }
    if ts.last().is_none_or(|t| {
        let end = t.span.end as usize;
        end < p
            && !u
                .get(end..p)
                .unwrap_or_default()
                .iter()
                .all(|c| char::from_u32(u32::from(*c)).is_some_and(char::is_whitespace))
    }) {
        return r;
    }
    if ts.last().is_some_and(|t| {
        matches!(t.kind, T::Directive | T::Bad(_) | T::Unknown)
            || matches!(t.kind, T::Str(_)) && matches!(kind, AnalysisKind::Completion)
    }) {
        return r;
    }
    let mut end = ts.len();
    let mut prefix = String::new();
    match kind {
        AnalysisKind::Completion => {
            if ts.last().is_some_and(|t| t.kind == T::Ident) {
                let Some(t) = end.checked_sub(1).and_then(|i| ts.get(i)).copied() else {
                    return r;
                };
                prefix = src.text(t.span);
                r.from = t.span.start as usize;
                end -= 1;
            }
        }
        AnalysisKind::Signature => {
            let mut stack: Vec<(usize, usize)> = vec![];
            for (i, t) in ts.iter().enumerate().take(end) {
                match t.kind {
                    T::LParen | T::LBracket => stack.push((i, 0)),
                    T::RParen | T::RBracket => {
                        stack.pop();
                    }
                    T::Comma => {
                        if let Some((_, arg)) = stack.last_mut() {
                            *arg += 1;
                        }
                    }
                    _ => {}
                }
            }
            let Some((i, arg)) = stack.into_iter().rev().find(|(i, _)| {
                ts.get(*i).is_some_and(|t| t.kind == T::LParen)
                    && i.checked_sub(1)
                        .and_then(|i| ts.get(i))
                        .is_some_and(|t| matches!(t.kind, T::Ident | T::Kw(_)))
            }) else {
                return r;
            };
            r.anchor = ts.get(i).map(|t| t.span.start as usize);
            r.argument = Some(arg);
            end = i;
            if end == 0 {
                return r;
            }
            let Some(t) = end.checked_sub(1).and_then(|i| ts.get(i)).copied() else {
                return r;
            };
            prefix = src.text(t.span);
            end -= 1;
        }
    }
    let constructors = end.checked_sub(1).and_then(|i| ts.get(i)).is_some_and(|t| {
        ["Новый", "New"]
            .iter()
            .any(|n| super::same(&src.text(t.span), n))
    });
    if matches!(kind, AnalysisKind::Completion) {
        let suffix: String = text
            .chars()
            .scan(0usize, |off, c| {
                let before = *off;
                *off += c.len_utf16();
                Some((before, c))
            })
            .skip_while(|(off, _)| *off < p)
            .map(|(_, c)| c)
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        r.to = p + suffix.encode_utf16().count();
    }
    let member = end
        .checked_sub(1)
        .and_then(|i| ts.get(i))
        .is_some_and(|t| t.kind == T::Dot);
    let recv_end = if member { end - 1 } else { end };
    let mut start = recv_end;
    let mut depth = 0usize;
    while start > 0 {
        let Some(t) = ts.get(start - 1).copied() else {
            return r;
        };
        match t.kind {
            T::RParen | T::RBracket => depth += 1,
            T::LParen | T::LBracket if depth > 0 => depth -= 1,
            T::Semicolon | T::Eq | T::Comma | T::LParen | T::LBracket if depth == 0 => break,
            _ => {}
        }
        start -= 1;
    }
    if ts
        .get(start..recv_end)
        .unwrap_or_default()
        .iter()
        .filter(|t| t.kind == T::Dot)
        .count()
        > 16
    {
        r.analysis_status = "limited";
        return r;
    }
    let begin = ts.get(start).map_or(0, |t| t.span.start as usize);
    let mut statement_start = start;
    while statement_start > 0
        && ts
            .get(statement_start - 1)
            .is_some_and(|t| t.kind != T::Semicolon)
    {
        statement_start -= 1;
    }
    let flow_end = ts.get(statement_start).map_or(0, |t| t.span.start as usize);
    let mut resolver = resolve::Resolver::new(cfg);
    let mut env = HashMap::new();
    let flow = parsed(u.get(..flow_end).unwrap_or_default(), ParseMode::Fragment);
    if flow.error.is_none() {
        for st in &flow.module.body {
            if !resolver.step() {
                break;
            }
            match &st.kind {
                StmtKind::Assign { target, value } => {
                    let v = resolver.expr(value, &env, 0);
                    if let ExprKind::Name(n) = &target.kind {
                        if resolver.has_call(value) {
                            env.clear();
                        }
                        env.insert(bsl_prim::fold_name(&n.name), v);
                    } else {
                        env.clear();
                    }
                }
                _ => env.clear(),
            }
        }
    }
    let mut out = list(&prefix, lang);
    if member && recv_end > start {
        let e = ts
            .get(recv_end - 1)
            .and_then(|t| u.get(begin..t.span.end as usize))
            .and_then(expression);
        if let Some(e) = e {
            let ty = resolver.expr(&e, &env, 0);
            resolver.members(&ty, &mut out);
        }
    } else {
        out.result = complete(cfg, &[], &prefix, constructors, lang);
    }
    if matches!(kind, AnalysisKind::Signature) {
        out.result.items.retain(|i| {
            (i.kind == "function" || constructors && i.kind == "class")
                && (super::same(&i.name, &prefix) || super::same(&i.name_other, &prefix))
        });
        out.result.total = out.result.items.len();
    }
    if resolver.limited || resolver.started.elapsed() > Duration::from_millis(50) {
        r.analysis_status = "limited";
    } else if !out.result.items.is_empty() {
        r.analysis_status = "complete";
    }
    out.result.items.sort_by(|a, b| a.name.cmp(&b.name));
    if r.analysis_status != "limited" {
        r.items = out.result.items;
        r.total = out.result.total;
    }
    r
}
