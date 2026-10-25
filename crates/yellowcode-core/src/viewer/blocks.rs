use bsl_syntax::{Kw, SourceText, TokenKind, tokenize};
use serde::Serialize;

use super::module::split_lines;
use super::outline::{OutlineItem, outline};

pub const MAX_DEPTH: usize = 256;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: &'static str,
    pub start: u32,
    pub end: u32,
}

pub fn blocks(text: &str) -> Vec<Block> {
    let (items, error) = outline(text);
    blocks_with(text, &items, error.as_deref())
}

fn error_line(error: &str) -> Option<u32> {
    error
        .strip_prefix('(')?
        .split([',', ')'])
        .next()?
        .parse()
        .ok()
}

pub fn blocks_with(text: &str, items: &[OutlineItem], error: Option<&str>) -> Vec<Block> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let limit = error.and_then(error_line).unwrap_or(u32::MAX);
    let lines = split_lines(text);
    let mut out: Vec<Block> = Vec::new();
    let mut push = |kind, start: u32, end: u32| {
        if start < end && end < limit {
            out.push(Block { kind, start, end });
        }
    };
    for it in items {
        if let Some(end) = it.end_line {
            push("proc", it.line, end);
        }
        if let Some((s, e)) = comment_above(&lines, it.line, true) {
            push("proc_comment", s, e);
        }
    }

    let src = SourceText::from_str("", text);
    let lexed = tokenize(&src);
    let mut stack: Vec<(&'static str, u32)> = Vec::new();
    for t in &lexed.tokens {
        let (kind, open) = match t.kind {
            TokenKind::Kw(Kw::If) => ("if", true),
            TokenKind::Kw(Kw::EndIf) => ("if", false),
            TokenKind::Kw(Kw::For | Kw::While) => ("loop", true),
            TokenKind::Kw(Kw::EndDo) => ("loop", false),
            TokenKind::Directive => match region_directive(&src.text(t.span)) {
                Some(open) => ("region", open),
                None => continue,
            },
            _ => continue,
        };
        let line = src.pos(t.span.start).line;
        if line >= limit {
            break;
        }
        if open {
            if stack.len() >= MAX_DEPTH {
                break;
            }
            stack.push((kind, line));
            continue;
        }
        match stack.pop() {
            Some((k, start)) if k == kind => {
                push(kind, start, line);
                if kind == "region"
                    && let Some((s, e)) = comment_above(&lines, start, false)
                {
                    push("region_comment", s, e);
                }
            }
            _ => break,
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    out
}

fn region_directive(d: &str) -> Option<bool> {
    let word = d
        .strip_prefix('#')?
        .split_whitespace()
        .next()?
        .to_lowercase();
    match word.as_str() {
        "область" | "region" => Some(true),
        "конецобласти" | "endregion" => Some(false),
        _ => None,
    }
}

fn comment_above(lines: &[&str], line: u32, skip_annotations: bool) -> Option<(u32, u32)> {
    let mut i = usize::try_from(line).ok()?.checked_sub(1)?;
    if skip_annotations {
        while i > 0
            && lines
                .get(i - 1)
                .is_some_and(|l| l.trim_start().starts_with('&'))
        {
            i -= 1;
        }
    }
    let last = i;
    let mut first = last;
    while first > 0
        && lines
            .get(first - 1)
            .is_some_and(|l| l.trim_start().starts_with("//"))
    {
        first -= 1;
    }
    if last == 0 || first == last {
        return None;
    }
    let (f, l) = (u32::try_from(first + 1).ok()?, u32::try_from(last).ok()?);
    (f < l).then_some((f, l))
}
