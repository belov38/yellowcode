use bsl_syntax::{SourceText, TokenKind, tokenize};
use query_syntax::ParseMode;

pub const MAX_LITERAL: usize = 256 * 1024;
pub const MAX_MODULE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Query,
    Composite,
    Other,
    TooLarge,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Literal {
    pub line: u32,
    pub col: u32,
    pub text: String,
    pub kind: Kind,
}

pub fn find_literals(module_text: &str) -> Vec<Literal> {
    collect(module_text, false)
}

pub fn find_all_literals(module_text: &str) -> Vec<Literal> {
    collect(module_text, true)
}

fn looks_like_query(text: &str) -> bool {
    match select_head(text) {
        Some(rest) => query_shaped(rest) || from_table(rest),
        None => false,
    }
}

fn select_head(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let word_end = t.find(|c: char| !c.is_alphabetic()).unwrap_or(t.len());
    let (head, rest) = t.split_at(word_end);
    let head = head.to_lowercase();
    (head == "выбрать" || head == "select").then_some(rest)
}

fn query_shaped(rest: &str) -> bool {
    let rest = rest.trim_start();
    let Some(first) = rest.chars().next() else {
        return false;
    };
    if first.is_ascii_digit() || "*(&\"'".contains(first) {
        return true;
    }
    let mut words = rest
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase);
    let second = words.next().unwrap_or_default();
    if matches!(
        second.as_str(),
        "разрешенные" | "различные" | "первые" | "allowed" | "distinct" | "top"
    ) {
        return true;
    }
    std::iter::once(second)
        .chain(words)
        .any(|w| matches!(w.as_str(), "как" | "as" | "поместить" | "into"))
}

fn from_table(rest: &str) -> bool {
    let mut words = rest.split_whitespace().map(str::to_lowercase);
    while let Some(w) = words.next() {
        if matches!(w.as_str(), "из" | "from") {
            return words.next().is_some_and(|t| {
                t.contains('.')
                    && t.chars().next().is_some_and(char::is_alphabetic)
                    && t.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            });
        }
    }
    false
}

fn is_candidate(text: &str) -> bool {
    match select_head(text) {
        Some(rest) => {
            query_shaped(rest)
                || (from_table(rest) && query_builder::parse(text, ParseMode::Query).is_ok())
        }
        None => query_builder::parse(text, ParseMode::Query).is_ok(),
    }
}

fn has_substitution(text: &str) -> bool {
    text.as_bytes()
        .windows(2)
        .any(|w| matches!(w, [b'%', b'1'..=b'9']))
}

fn cut(text: &str) -> &str {
    if text.len() <= MAX_MODULE {
        return text;
    }
    let mut end = MAX_MODULE;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or("")
}

fn collect(module_text: &str, all: bool) -> Vec<Literal> {
    let src = SourceText::from_str("module", cut(module_text));
    let lexed = tokenize(&src);
    let toks = &lexed.tokens;
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        let TokenKind::Str(idx) = t.kind else {
            continue;
        };
        let pos = src.pos(t.span.start);
        let mk = |text: String, kind| Literal {
            line: pos.line,
            col: pos.col,
            text,
            kind,
        };
        let from_lexer = || {
            lexed
                .strings
                .get(idx as usize)
                .map(|u| String::from_utf16_lossy(u))
                .unwrap_or_default()
        };
        if (t.span.end.saturating_sub(t.span.start)) as usize > MAX_LITERAL {
            let head: String = from_lexer().chars().take(256).collect();
            if looks_like_query(&head) {
                out.push(mk(String::new(), Kind::TooLarge));
            } else if all {
                out.push(mk(String::new(), Kind::Other));
            }
            continue;
        }
        let text = query_builder::literal::from_bsl(&src.text(t.span)).unwrap_or_else(from_lexer);
        if !is_candidate(&text) {
            if all {
                out.push(mk(text, Kind::Other));
            }
            continue;
        }
        let prev = k.checked_sub(1).and_then(|p| toks.get(p));
        let next = toks.get(k + 1);
        let glued = prev.is_some_and(|p| p.kind == TokenKind::Plus)
            || next.is_some_and(|n| n.kind == TokenKind::Plus)
            || has_substitution(&text)
            || is_wrapper_arg(&src, toks, k);
        out.push(mk(text, if glued { Kind::Composite } else { Kind::Query }));
    }
    out
}

fn is_wrapper_arg(src: &SourceText, toks: &[bsl_syntax::Token], k: usize) -> bool {
    let (Some(p1), Some(p2)) = (
        k.checked_sub(1).and_then(|i| toks.get(i)),
        k.checked_sub(2).and_then(|i| toks.get(i)),
    ) else {
        return false;
    };
    if p1.kind != TokenKind::LParen || p2.kind != TokenKind::Ident {
        return false;
    }
    matches!(
        src.text(p2.span).to_lowercase().as_str(),
        "стршаблон"
            | "strtemplate"
            | "подставитьпараметрывстроку"
            | "substitutestringparameters"
            | "стрзаменить"
            | "strreplace"
            | "нстр"
            | "nstr"
    )
}
