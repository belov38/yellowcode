use crate::kw::{Func, KwSet, lookup};
use crate::source::{SourceMap, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Punct {
    Comma,
    Dot,
    LParen,
    RParen,
    Semi,
    Plus,
    Minus,
    Star,
    Slash,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LBrace,
    RBrace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexErr {
    BadChar,
    BadParam,
    UnclosedString,
    NumberTooLong,
    BadNumber,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Word { kws: KwSet, func: Option<Func> },
    Number,
    Str,
    Param,
    Punct(Punct),
    Error(LexErr),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    pub span: Span,
}

const MAX_DIGITS: usize = 38;

fn is_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}'
    )
}

fn is_basic_cyrillic(c: char) -> bool {
    matches!(c, 'А'..='я' | 'Ё' | 'ё')
}

fn is_word_start(c: char) -> bool {
    c == '_'
        || c == '#'
        || c.is_ascii_alphabetic()
        || is_basic_cyrillic(c)
        || (!c.is_ascii() && c.is_alphabetic() && (c as u32) <= 0xFFFF)
}

fn is_word_char(c: char) -> bool {
    c == '_'
        || c == '#'
        || c.is_ascii_alphanumeric()
        || is_basic_cyrillic(c)
        || (!c.is_ascii() && c.is_alphanumeric() && (c as u32) <= 0xFFFF)
}

pub(crate) fn split_number(text: &str) -> (&str, &str) {
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    let int_t = int.trim_start_matches('0');
    let int = if int_t.is_empty() {
        int.get(int.len().saturating_sub(1)..).unwrap_or("0")
    } else {
        int_t
    };
    (int, frac.trim_end_matches('0'))
}

pub fn canonical_number(text: &str) -> String {
    let (int, frac) = split_number(text);
    if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    }
}

fn significant_digits(text: &str) -> usize {
    let (int, frac) = split_number(text);
    if frac.is_empty() {
        int.len()
    } else if int == "0" {
        frac.len()
    } else {
        int.len() + frac.len()
    }
}

pub fn string_value(token_text: &str) -> String {
    let inner = token_text.strip_prefix('"').unwrap_or(token_text);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    inner.replace("\"\"", "\"")
}

fn span(start: usize, end: usize) -> Span {
    Span::new(start as u32, end as u32)
}

pub(crate) fn glued_end(text: &str, start: usize) -> usize {
    scan(text, start, |c| !is_space(c))
}

fn scan(text: &str, from: usize, pred: impl Fn(char) -> bool) -> usize {
    match text.get(from..) {
        Some(rest) => rest
            .char_indices()
            .find(|&(_, c)| !pred(c))
            .map_or(text.len(), |(i, _)| from + i),
        None => text.len(),
    }
}

pub struct Tokens<'a> {
    text: &'a str,
    offset: usize,
    eof: bool,
    comment_eof: bool,
}
impl<'a> Tokens<'a> {
    pub(crate) fn comment_eof(&self) -> bool {
        self.comment_eof
    }
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            offset: 0,
            eof: false,
            comment_eof: false,
        }
    }
}
impl Iterator for Tokens<'_> {
    type Item = Token;
    fn next(&mut self) -> Option<Token> {
        if self.eof {
            return None;
        }
        let text = self.text;
        if text.len() > (u32::MAX / 2) as usize && self.offset == 0 {
            self.offset = text.len();
            if let Some(c) = text.chars().next() {
                return Some(Token {
                    kind: TokKind::Error(LexErr::BadChar),
                    span: span(0, c.len_utf8()),
                });
            }
        }
        let len = text.len();
        let mut i = self.offset;
        while let Some(rest) = text.get(i..) {
            let Some(c) = rest.chars().next() else { break };
            let start = i;
            let next = rest.get(c.len_utf8()..).and_then(|r| r.chars().next());
            if is_space(c) {
                i += c.len_utf8();
                continue;
            }
            if c == '/' && next == Some('/') {
                i = scan(text, i, |c| c != '\n' && c != '\r');
                if i >= len {
                    self.comment_eof = true;
                }
                continue;
            }
            let (kind, end) = if is_word_start(c) {
                let end = scan(text, i, is_word_char);
                let (kws, func) = lookup(text.get(start..end).unwrap_or(""));
                (TokKind::Word { kws, func }, end)
            } else if c.is_ascii_digit() {
                let mut end = scan(text, i, |c| c.is_ascii_digit());
                if text.get(end..).is_some_and(|r| r.starts_with('.')) {
                    end = scan(text, end + 1, |c| c.is_ascii_digit());
                }
                let dotted = text.get(start..end).is_some_and(|t| t.contains('.'));
                let glued = text
                    .get(end..)
                    .and_then(|r| r.chars().next())
                    .is_some_and(|c| {
                        if dotted {
                            c == '.' || is_word_char(c)
                        } else {
                            c != '_' && is_word_char(c)
                        }
                    });
                let n = significant_digits(text.get(start..end).unwrap_or(""));
                if glued {
                    let end = scan(text, end, |c| c == '.' || is_word_char(c));
                    (TokKind::Error(LexErr::BadNumber), end)
                } else if n > MAX_DIGITS {
                    (TokKind::Error(LexErr::NumberTooLong), end)
                } else {
                    (TokKind::Number, end)
                }
            } else if c == '"' {
                let mut j = i + 1;
                loop {
                    match text.get(j..).and_then(|r| r.find('"')) {
                        None => break (TokKind::Error(LexErr::UnclosedString), len),
                        Some(k) => {
                            j += k + 1;
                            if text.get(j..).is_some_and(|r| r.starts_with('"')) {
                                j += 1;
                            } else {
                                break (TokKind::Str, j);
                            }
                        }
                    }
                }
            } else if c == '&' {
                match next {
                    Some(n) if n != '#' && is_word_start(n) => {
                        (TokKind::Param, scan(text, i + 1, is_word_char))
                    }
                    Some(n) if n == '#' || (n.is_alphanumeric() && (n as u32) <= 0xFFFF) => (
                        TokKind::Error(LexErr::BadParam),
                        scan(text, i + 1, is_word_char),
                    ),
                    _ => (TokKind::Error(LexErr::BadParam), i + 1),
                }
            } else {
                let one = |p| (TokKind::Punct(p), i + 1);
                match c {
                    ',' => one(Punct::Comma),
                    '.' => one(Punct::Dot),
                    '(' => one(Punct::LParen),
                    ')' => one(Punct::RParen),
                    ';' => one(Punct::Semi),
                    '+' => one(Punct::Plus),
                    '-' => one(Punct::Minus),
                    '*' => one(Punct::Star),
                    '/' => one(Punct::Slash),
                    '=' => one(Punct::Eq),
                    '{' => one(Punct::LBrace),
                    '}' => one(Punct::RBrace),
                    '<' => match next {
                        Some('>') => (TokKind::Punct(Punct::Ne), i + 2),
                        Some('=') => (TokKind::Punct(Punct::Le), i + 2),
                        _ => one(Punct::Lt),
                    },
                    '>' => match next {
                        Some('=') => (TokKind::Punct(Punct::Ge), i + 2),
                        _ => one(Punct::Gt),
                    },
                    '[' => {
                        let rest = text.get(i..).unwrap_or("");
                        let close = rest
                            .find([']', '\n', '\r'])
                            .filter(|k| rest[*k..].starts_with(']'));
                        (
                            TokKind::Error(LexErr::BadChar),
                            i + close.map_or(1, |k| k + 1),
                        )
                    }
                    _ => (TokKind::Error(LexErr::BadChar), i + c.len_utf8()),
                }
            };
            self.offset = end;
            return Some(Token {
                kind,
                span: span(start, end),
            });
        }
        self.eof = true;
        Some(Token {
            kind: TokKind::Eof,
            span: span(text.len(), text.len()),
        })
    }
}
pub fn tokenize(text: &str) -> (Vec<Token>, SourceMap) {
    let mut map = SourceMap::new(text);
    let mut tokens = Tokens::new(text);
    let mut out = Vec::with_capacity(text.len() / 4 + 1);
    out.extend(tokens.by_ref());
    if tokens.comment_eof {
        map.add_eof_line();
    }
    (out, map)
}
