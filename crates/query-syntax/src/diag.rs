use std::fmt;

use crate::kw::{Func, Kw};
use crate::lang::{Lang, ScriptVariant};
use crate::msg::MsgId;
use crate::source::{ErrAt, line_text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MsgArg {
    Text(Box<str>),
    Kw(Kw),
    Func(Func),
    Msg(MsgId),
}

impl MsgArg {
    pub fn text(s: &str) -> MsgArg {
        MsgArg::Text(s.into())
    }
    fn render(&self, lang: Lang, script: ScriptVariant) -> &str {
        match self {
            MsgArg::Msg(m) => m.text(lang),
            MsgArg::Text(s) => s,
            MsgArg::Kw(k) => k.name(script),
            MsgArg::Func(f) => f.name(script),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub msg: MsgId,
    pub subst: Vec<MsgArg>,
    pub quoted: Option<MsgArg>,
    pub at: Option<ErrAt>,
    pub cause: Option<Box<QueryError>>,
    pub detail: Option<Box<str>>,
}

pub type SyntaxError = QueryError;

const WS: [char; 6] = [' ', '\t', '\r', '\n', '\u{b}', '\u{c}'];

impl QueryError {
    pub fn new(msg: MsgId, at: Option<ErrAt>) -> QueryError {
        QueryError {
            msg,
            subst: Vec::new(),
            quoted: None,
            at,
            cause: None,
            detail: None,
        }
    }
    pub fn with_detail(mut self, d: &str) -> QueryError {
        self.detail = Some(d.into());
        self
    }
    pub fn with_subst(mut self, a: MsgArg) -> QueryError {
        self.subst.push(a);
        self
    }
    pub fn with_quoted(mut self, a: MsgArg) -> QueryError {
        self.quoted = Some(a);
        self
    }
    pub fn with_cause(mut self, c: QueryError) -> QueryError {
        self.cause = Some(Box::new(c));
        self
    }

    pub fn message(&self, lang: Lang, script: ScriptVariant) -> String {
        let text = self.msg.text(lang);
        let mut out = String::with_capacity(text.len() + 16);
        let mut chars = text.chars().peekable();
        let mut next = 0usize;
        while let Some(c) = chars.next() {
            if c == '%' && matches!(chars.peek(), Some('d' | 's')) {
                chars.next();
                if let Some(a) = self.subst.get(next) {
                    out.push_str(a.render(lang, script));
                }
                next += 1;
                continue;
            }
            let digit = if c == '%' {
                chars
                    .peek()
                    .and_then(|d| d.to_digit(10))
                    .filter(|&d| d >= 1)
            } else {
                None
            };
            match digit {
                Some(d) => {
                    chars.next();
                    if let Some(a) = self.subst.get(d as usize - 1) {
                        out.push_str(a.render(lang, script));
                    }
                }
                None => out.push(c),
            }
        }
        if let Some(q) = &self.quoted {
            out.push_str(" \"");
            out.push_str(q.render(lang, script));
            out.push('"');
        }
        if let Some(d) = &self.detail {
            out.push_str(d);
        }
        out
    }

    pub fn render(&self, text: &str, lang: Lang, script: ScriptVariant) -> String {
        let message = self.message(lang, script);
        let Some(at) = self.at else {
            return message;
        };
        let mut out = format!("{{({}, {})}}: {}", at.pos.line, at.pos.col, message);
        if let Some((start, line)) = line_text(text, at.pos.line)
            && !line.is_empty()
        {
            let mut rel = (at.offset.saturating_sub(start) as usize).min(line.len());
            while !line.is_char_boundary(rel) {
                rel -= 1;
            }
            let (a, b) = line.split_at(rel);
            let marked = format!("{a}<<?>>{b}");
            out.push('\n');
            out.push_str(marked.trim_matches(WS));
        }
        out
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self.message(Lang::Ru, ScriptVariant::Russian);
        match self.at {
            Some(at) => write!(f, "{{({}, {})}}: {}", at.pos.line, at.pos.col, message),
            None => f.write_str(&message),
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|c| c as &(dyn std::error::Error + 'static))
    }
}
