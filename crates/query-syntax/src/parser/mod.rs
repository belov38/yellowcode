mod access;
mod dcs;
mod expr;
mod from;
mod query;

use crate::ast::Ident;
use crate::diag::{MsgArg, SyntaxError};
use crate::kw::{Kw, KwSet};
use crate::lexer::{LexErr, Punct, TokKind, Token};
use crate::msg::MsgId;
use crate::source::{SourceMap, Span};
use crate::{BRACKET_UNITS, BUDGET_OPS, BUDGET_UNITS, MAX_DEPTH, ParseMode};

pub(crate) type PResult<T> = Result<T, SyntaxError>;

#[derive(Clone, Copy)]
pub(crate) struct Rec {
    pub cum: u32,
    pub at: u32,
}

pub(crate) const UNITS_LIMIT: u32 = BUDGET_UNITS + 3;

pub(crate) struct Parser<'t> {
    pub text: &'t str,
    pub memory: Option<&'t crate::resource::ParseScope>,
    pub public_limits: bool,
    pub limit_error: Option<crate::PublicParseLimit>,
    toks: Vec<Token>,
    i: usize,
    pub map: SourceMap,
    pub mode: ParseMode,
    depth: u32,
    pub max_depth: u32,
    pub p1: u32,
    p2: u32,
    pub rec: Vec<Rec>,
    pub values: Vec<crate::ValueUse>,
    hash_lead: bool,
    hash_cont: bool,
}

pub(crate) fn reserved_prefix(kws: KwSet) -> bool {
    kws.intersects(crate::kw::RESERVED_PREFIX)
}

pub(crate) fn reserved_alias(kws: KwSet) -> bool {
    kws.intersects(crate::kw::RESERVED_ALIAS)
}

pub(crate) fn reserved_src_alias(kws: KwSet) -> bool {
    kws.intersects(crate::kw::RESERVED_SRC_ALIAS)
}

fn punct_text(p: Punct) -> &'static str {
    match p {
        Punct::Comma => ",",
        Punct::Dot => ".",
        Punct::LParen => "(",
        Punct::RParen => ")",
        Punct::Semi => ";",
        Punct::Plus => "+",
        Punct::Minus => "-",
        Punct::Star => "*",
        Punct::Slash => "/",
        Punct::Eq => "=",
        Punct::Ne => "<>",
        Punct::Lt => "<",
        Punct::Gt => ">",
        Punct::Le => "<=",
        Punct::Ge => ">=",
        Punct::LBrace => "{",
        Punct::RBrace => "}",
    }
}

impl<'t> Parser<'t> {
    pub fn new(
        text: &'t str,
        toks: Vec<Token>,
        map: SourceMap,
        mode: ParseMode,
    ) -> PResult<Parser<'t>> {
        Self::new_with_memory(text, toks, map, mode, None)
    }
    pub fn new_with_memory(
        text: &'t str,
        toks: Vec<Token>,
        map: SourceMap,
        mode: ParseMode,
        memory: Option<&'t crate::resource::ParseScope>,
    ) -> PResult<Parser<'t>> {
        let p = Parser {
            text,
            memory,
            public_limits: false,
            limit_error: None,
            toks,
            i: 0,
            map,
            mode,
            depth: 0,
            max_depth: 0,
            p1: 0,
            p2: 0,
            rec: Vec::new(),
            values: Vec::new(),
            hash_lead: false,
            hash_cont: false,
        };
        p.check_lex()?;
        Ok(p)
    }

    fn eof(&self) -> Token {
        let len = self.text.len() as u32;
        Token {
            kind: TokKind::Eof,
            span: Span::new(len, len),
        }
    }

    pub fn tok(&self) -> Token {
        self.toks.get(self.i).copied().unwrap_or_else(|| self.eof())
    }

    pub fn nth(&self, k: usize) -> Token {
        self.toks
            .get(self.i.saturating_add(k))
            .or_else(|| self.toks.last())
            .copied()
            .unwrap_or_else(|| self.eof())
    }

    pub fn prev_end(&self) -> u32 {
        self.i
            .checked_sub(1)
            .and_then(|j| self.toks.get(j))
            .map_or(0, |t| t.span.end)
    }

    pub fn bump(&mut self) -> PResult<Token> {
        let t = self.tok();
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        self.check_lex()?;
        Ok(t)
    }

    fn check_lex(&self) -> PResult<()> {
        let t = self.tok();
        if self.hash_allowed(t) {
            return Ok(());
        }
        match t.kind {
            TokKind::Error(LexErr::BadChar | LexErr::BadParam | LexErr::BadNumber) => Err(self
                .err(MsgId::SELECT_ERROR_SYNTAXERROR, t.span.start)
                .with_quoted(crate::resource::arg(self.memory, t.span.text(self.text)))),
            TokKind::Error(LexErr::UnclosedString) => {
                Err(self.err(MsgId::SELECT_ERROR_NOTENDEDSTRING, t.span.start))
            }
            TokKind::Error(LexErr::NumberTooLong) => Err(self
                .err(MsgId::SELECT_ERROR_TOOLONGLENGTHDESCRIPTOR, t.span.start)
                .with_quoted(crate::resource::arg(
                    self.memory,
                    &crate::resource::number(self.memory, t.span.text(self.text))?,
                ))),
            _ => Ok(()),
        }
    }

    fn hash_allowed(&self, t: Token) -> bool {
        if t.kind != TokKind::Error(LexErr::BadChar) || t.span.text(self.text) != "#" {
            return false;
        }
        if self.hash_lead {
            return true;
        }
        let Some(prev) = self.i.checked_sub(1).and_then(|j| self.toks.get(j)) else {
            return false;
        };
        let after_kw = matches!(prev.kind, TokKind::Word { kws, .. }
            if [Kw::From, Kw::Join, Kw::Into, Kw::Add, Kw::Drop].iter().any(|k| kws.contains(*k)));
        after_kw || self.hash_cont && prev.span.end == t.span.start
    }

    pub fn at_hash(&self) -> bool {
        let t = self.tok();
        t.kind == TokKind::Error(LexErr::BadChar) && t.span.text(self.text) == "#"
    }

    pub fn hash_name(&mut self) -> PResult<Ident> {
        let start = self.tok().span.start;
        let mut end = start;
        self.hash_cont = true;
        loop {
            let t = self.tok();
            let part = match t.kind {
                TokKind::Word { .. } => true,
                TokKind::Number => end != start,
                _ => self.at_hash(),
            };
            if !part || t.span.start != end && end != start {
                break;
            }
            end = t.span.end;
            if let Err(e) = self.bump() {
                self.hash_cont = false;
                return Err(e);
            }
        }
        self.hash_cont = false;
        let span = Span::new(start, end);
        Ok(Ident {
            text: crate::resource::text(self.memory, span.text(self.text))?,
            span,
        })
    }

    pub fn at_kw(&self, kw: Kw) -> bool {
        matches!(self.tok().kind, TokKind::Word { kws, .. } if kws.contains(kw))
    }

    pub fn at_word_before_dot(&self) -> bool {
        matches!(self.tok().kind, TokKind::Word { .. }) && self.nth_punct(1, Punct::Dot)
    }

    pub fn at_lead_kw(&self, kw: Kw) -> bool {
        self.at_kw(kw) && !self.nth_punct(1, Punct::Dot)
    }

    pub fn eat_lead_kw(&mut self, kw: Kw) -> PResult<Option<Span>> {
        if self.at_lead_kw(kw) {
            Ok(Some(self.bump()?.span))
        } else {
            Ok(None)
        }
    }

    pub fn nth_kw(&self, k: usize, kw: Kw) -> bool {
        matches!(self.nth(k).kind, TokKind::Word { kws, .. } if kws.contains(kw))
    }

    pub fn at_punct(&self, p: Punct) -> bool {
        self.tok().kind == TokKind::Punct(p)
    }

    pub fn nth_punct(&self, k: usize, p: Punct) -> bool {
        self.nth(k).kind == TokKind::Punct(p)
    }

    pub fn at_eof(&self) -> bool {
        self.tok().kind == TokKind::Eof
    }

    pub fn eat_kw(&mut self, kw: Kw) -> PResult<Option<Span>> {
        if self.at_kw(kw) {
            Ok(Some(self.bump()?.span))
        } else {
            Ok(None)
        }
    }

    pub fn eat_punct(&mut self, p: Punct) -> PResult<Option<Span>> {
        if self.at_punct(p) {
            Ok(Some(self.bump()?.span))
        } else {
            Ok(None)
        }
    }

    pub fn expect_kw(&mut self, kw: Kw) -> PResult<Span> {
        match self.eat_kw(kw)? {
            Some(s) => Ok(s),
            None => Err(self
                .err_here(MsgId::SELECT_ERROR_TOKENEXPECTED)
                .with_quoted(MsgArg::Kw(kw))),
        }
    }

    pub fn expect_punct(&mut self, p: Punct) -> PResult<Span> {
        match self.eat_punct(p)? {
            Some(s) => Ok(s),
            None => Err(self
                .err_here(MsgId::SELECT_ERROR_TOKENEXPECTED)
                .with_quoted(crate::resource::arg(self.memory, punct_text(p)))),
        }
    }

    pub fn word_ident(&mut self) -> PResult<Ident> {
        let t = self.bump()?;
        Ok(Ident {
            text: crate::resource::text(self.memory, t.span.text(self.text))?,
            span: t.span,
        })
    }

    pub fn err(&self, msg: MsgId, offset: u32) -> SyntaxError {
        SyntaxError::new(msg, Some(self.map.at(self.text, offset)))
    }

    pub fn err_here(&self, msg: MsgId) -> SyntaxError {
        self.err(msg, self.tok().span.start)
    }

    pub fn unexpected(&self) -> SyntaxError {
        let t = self.tok();
        if t.kind == TokKind::Eof {
            self.err(MsgId::SELECT_ERROR_SYNTAXERROR, t.span.start)
        } else {
            let mut end = t.span.end;
            if t.kind == TokKind::Number {
                end = crate::lexer::glued_end(self.text, t.span.start as usize) as u32;
            }
            if matches!(t.kind, TokKind::Word { .. }) {
                let mut k = 1;
                loop {
                    let (dot, word) = (self.nth(k), self.nth(k + 1));
                    let joined = dot.kind == TokKind::Punct(Punct::Dot)
                        && dot.span.start == end
                        && matches!(word.kind, TokKind::Word { .. })
                        && word.span.start == dot.span.end;
                    if !joined {
                        break;
                    }
                    end = word.span.end;
                    k += 2;
                }
            }
            if matches!(t.kind, TokKind::Word { .. })
                && self.text.as_bytes().get(end as usize) == Some(&b';')
            {
                end += 1;
            }
            self.err(MsgId::SELECT_ERROR_SYNTAXERROR, t.span.start)
                .with_quoted(crate::resource::arg(
                    self.memory,
                    Span::new(t.span.start, end).text(self.text),
                ))
        }
    }

    pub fn enter(&mut self, at: u32) -> PResult<()> {
        if self.public_limits && self.depth >= 128 {
            self.limit_error = Some(crate::PublicParseLimit::Depth);
            return Err(self.err(MsgId::SELECT_ERROR_STACKOVERFLOW, at));
        }
        if self.depth >= MAX_DEPTH {
            return Err(self.err(MsgId::SELECT_ERROR_STACKOVERFLOW, at));
        }
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        Ok(())
    }

    pub fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    pub fn note(&mut self, cum: u32, at: u32) -> PResult<()> {
        push_rec(self.memory, &mut self.rec, cum, at)
    }

    pub fn overflow(&self, at: u32) -> SyntaxError {
        self.err(MsgId::SELECT_ERROR_STACKOVERFLOW, at)
    }

    pub fn charge_op(&mut self, at: u32) -> PResult<()> {
        self.p1 += 1;
        self.note(self.p1, at)?;
        if self.p1 > BUDGET_OPS {
            return Err(self.overflow(at));
        }
        Ok(())
    }

    pub fn uncharge_op(&mut self, n: u32) {
        self.p1 = self.p1.saturating_sub(n);
    }

    pub fn charge_units(&mut self, n: u32, at: u32) -> PResult<()> {
        self.p2 += n;
        if self.p2 > UNITS_LIMIT {
            return Err(self.overflow(at));
        }
        Ok(())
    }

    pub fn uncharge_units(&mut self, n: u32) {
        self.p2 = self.p2.saturating_sub(n);
    }

    pub fn check_leaf(&self, end: u32) -> PResult<()> {
        if self.p2 + 3 > UNITS_LIMIT {
            return Err(self.overflow(end));
        }
        Ok(())
    }

    pub fn enter_bracket(&mut self, ops: bool) -> PResult<()> {
        let at = self.tok().span.end;
        self.charge_units(BRACKET_UNITS, at)?;
        if ops {
            self.charge_op(at)?;
        }
        Ok(())
    }

    pub fn leave_bracket(&mut self, ops: bool) {
        self.uncharge_units(BRACKET_UNITS);
        if ops {
            self.uncharge_op(1);
        }
    }
}

pub(crate) fn push_rec(
    memory: Option<&crate::resource::ParseScope>,
    list: &mut Vec<Rec>,
    cum: u32,
    at: u32,
) -> PResult<()> {
    if list.last().is_none_or(|r| cum > r.cum) {
        crate::resource::push(memory, list, Rec { cum, at })?;
    }
    Ok(())
}

pub(crate) fn first_overflow(list: &[Rec], extra: u32) -> Option<u32> {
    let limit = BUDGET_OPS.saturating_sub(extra);
    let i = list.partition_point(|r| r.cum <= limit);
    list.get(i).map(|r| r.at)
}
