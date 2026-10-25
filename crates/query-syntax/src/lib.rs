#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod ast;
pub mod diag;
mod expr_entry;
pub mod kw;
pub mod lang;
pub mod lexer;
pub mod msg;
mod msg_gen;
mod parser;
pub mod print;
mod resource;
mod worker_parse;
pub use worker_parse::{WorkerParsed, parse_batch_on_worker};
pub mod source;
mod unresolved;
pub use unresolved::{first_unresolved_dcs_tail_at, unresolved_dcs_tails};

pub use diag::{MsgArg, QueryError, SyntaxError};
pub use expr_entry::{ExprTree, parse_expr};
pub use kw::{Func, Kw, KwSet};
pub use lang::{Lang, ScriptVariant};
pub use msg::MsgId;
pub use source::{ErrAt, Pos, SourceMap, Span};

use lexer::{Punct, TokKind, Token};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParseMode {
    Query,
    Dcs,
}

pub const MAX_DEPTH: u32 = 2048;
pub const BUDGET_OPS: u32 = 512;
pub const BUDGET_UNITS: u32 = 505;
pub const BRACKET_UNITS: u32 = 7;
pub const MAX_BRACKETS: u32 = BUDGET_UNITS / BRACKET_UNITS;
pub const MAX_SIGNS: usize = BUDGET_OPS as usize;
pub const MAX_NOTS: usize = BUDGET_UNITS as usize;
pub const MAX_COMPARISONS: usize = BUDGET_OPS as usize;
pub const INLINE_DEPTH: u32 = 32;

pub fn parse_batch(text: &str, mode: ParseMode) -> Result<ast::Batch, SyntaxError> {
    parse_batch_values(text, mode).batch
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueUse {
    pub path: String,
    pub at: u32,
}

#[derive(Debug)]
pub struct Parsed {
    pub batch: Result<ast::Batch, SyntaxError>,
    pub values: Vec<ValueUse>,
}

pub fn parse_batch_values(text: &str, mode: ParseMode) -> Parsed {
    let (batch, values) = parse_with(text, mode, |mut p| {
        let r = p.batch();
        let parser::Parser {
            map,
            max_depth,
            values,
            ..
        } = p;
        let batch = r.map(|stmts| ast::Batch {
            stmts,
            map,
            mode,
            max_depth,
        });
        (batch, values)
    });
    Parsed { batch, values }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicParseLimit {
    Statements,
    Columns,
    Depth,
}
pub fn parse_batch_public(text: &str, mode: ParseMode) -> Result<Parsed, PublicParseLimit> {
    preflight_public(text)?;
    let (batch, values, limit) = parse_with_public(text, mode);
    if let Some(limit) = limit {
        return Err(limit);
    }
    Ok(Parsed { batch, values })
}
fn preflight_public(text: &str) -> Result<(), PublicParseLimit> {
    public_preflight(text).map(|_| ())
}

pub const PUBLIC_MAX_STATEMENTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preflight {
    pub tokens: u32,
    pub statements: u16,
    pub depth: u32,
    pub tokens_per_statement: [u32; PUBLIC_MAX_STATEMENTS],
    pub dots_per_statement: [u32; PUBLIC_MAX_STATEMENTS],
    pub stars_per_statement: [u32; PUBLIC_MAX_STATEMENTS],
    pub chained_dots_per_statement: [u32; PUBLIC_MAX_STATEMENTS],
}

pub fn public_preflight(text: &str) -> Result<Preflight, PublicParseLimit> {
    let depth = estimate_depth_tokens(lexer::Tokens::new(text));
    if depth > 128 {
        return Err(PublicParseLimit::Depth);
    }
    let mut out = Preflight {
        tokens: 0,
        statements: 1,
        depth,
        tokens_per_statement: [0; PUBLIC_MAX_STATEMENTS],
        dots_per_statement: [0; PUBLIC_MAX_STATEMENTS],
        stars_per_statement: [0; PUBLIC_MAX_STATEMENTS],
        chained_dots_per_statement: [0; PUBLIC_MAX_STATEMENTS],
    };
    let mut current = 0usize;
    let mut after_semi = false;
    let mut path_dots = 0u32;
    for t in lexer::Tokens::new(text) {
        if t.kind == TokKind::Eof {
            break;
        }
        if after_semi {
            current += 1;
            after_semi = false;
            if current >= PUBLIC_MAX_STATEMENTS {
                return Err(PublicParseLimit::Statements);
            }
        }
        out.tokens = out.tokens.saturating_add(1);
        if let Some(n) = out.tokens_per_statement.get_mut(current) {
            *n = n.saturating_add(1);
        }
        let in_path = matches!(t.kind, TokKind::Punct(Punct::Dot) | TokKind::Word { .. });
        match t.kind {
            TokKind::Punct(Punct::Semi) => after_semi = true,
            TokKind::Punct(Punct::Dot) => {
                if let Some(n) = out.dots_per_statement.get_mut(current) {
                    *n = n.saturating_add(1);
                }
                path_dots = path_dots.saturating_add(1);
                if path_dots > 1
                    && let Some(n) = out.chained_dots_per_statement.get_mut(current)
                {
                    *n = n.saturating_add(1);
                }
            }
            TokKind::Punct(Punct::Star) => {
                if let Some(n) = out.stars_per_statement.get_mut(current) {
                    *n = n.saturating_add(1);
                }
            }
            _ => {}
        }
        if !in_path {
            path_dots = 0;
        }
    }
    out.statements = u16::try_from(current + 1).unwrap_or(u16::MAX);
    Ok(out)
}
fn parse_with_public(
    text: &str,
    mode: ParseMode,
) -> (
    Result<ast::Batch, SyntaxError>,
    Vec<ValueUse>,
    Option<PublicParseLimit>,
) {
    let (toks, map) = lexer::tokenize(text);
    let depth = estimate_depth(&toks);
    with_deep_stack(depth, move || {
        let mut p = match parser::Parser::new(text, toks, map, mode) {
            Ok(p) => p,
            Err(e) => return (Err(e), Vec::new(), None),
        };
        p.public_limits = true;
        let r = p.batch();
        let parser::Parser {
            map,
            max_depth,
            values,
            limit_error,
            ..
        } = p;
        (
            r.map(|stmts| ast::Batch {
                stmts,
                map,
                mode,
                max_depth,
            }),
            values,
            limit_error,
        )
    })
}

pub fn parse_access_condition(text: &str) -> Result<ast::AccessCondition, SyntaxError> {
    parse_with(text, ParseMode::Query, |mut p| {
        let r = p.condition();
        let parser::Parser { map, max_depth, .. } = p;
        (
            r.map(|form| ast::AccessCondition {
                form,
                map,
                max_depth,
            }),
            Vec::new(),
        )
    })
    .0
}

fn parse_with<T: Send>(
    text: &str,
    mode: ParseMode,
    f: impl FnOnce(parser::Parser<'_>) -> (Result<T, SyntaxError>, Vec<ValueUse>) + Send,
) -> (Result<T, SyntaxError>, Vec<ValueUse>) {
    let (toks, map) = lexer::tokenize(text);
    let depth = estimate_depth(&toks);
    with_deep_stack(depth, move || {
        match parser::Parser::new(text, toks, map, mode) {
            Ok(p) => f(p),
            Err(e) => (Err(e), Vec::new()),
        }
    })
}

fn estimate_depth(toks: &[Token]) -> u32 {
    estimate_depth_tokens(toks.iter().copied())
}
fn estimate_depth_tokens(toks: impl IntoIterator<Item = Token>) -> u32 {
    const LEVELS: usize = MAX_DEPTH as usize + 2;
    const WORDS: usize = LEVELS.div_ceil(64);
    #[derive(Clone, Copy, PartialEq)]
    enum Open {
        Bracket,
        Case,
        Join,
    }
    #[derive(Clone, Copy, Default)]
    struct Runs {
        not: u16,
        sign: u16,
        cmp: u16,
    }
    impl Runs {
        fn levels(self) -> u32 {
            u32::from(self.not) + u32::from(self.sign) + u32::from(self.cmp.saturating_sub(1))
        }
    }
    let bit = |bits: &[u64; WORDS], i: u32| {
        bits.get((i / 64) as usize)
            .is_some_and(|w| w & (1u64 << (i % 64)) != 0)
    };
    let put = |bits: &mut [u64; WORDS], i: u32, on: bool| {
        if let Some(w) = bits.get_mut((i / 64) as usize) {
            let b = 1u64 << (i % 64);
            *w = if on { *w | b } else { *w & !b };
        }
    };
    let (mut case, mut join, mut between) = ([0u64; WORDS], [0u64; WORDS], [0u64; WORDS]);
    let mut runs = [Runs::default(); LEVELS];
    let (mut depth, mut max, mut sum) = (0u32, 0u32, 0u32);
    let (mut name_next, mut prev_ends_operand, mut after_top) = (false, false, false);
    let mut join_head = false;
    let mut not_after_operand = false;
    let mut toks = toks.into_iter().peekable();
    while let Some(t) = toks.next() {
        let next = toks.peek().map(|n| n.kind);
        let before_dot = next == Some(TokKind::Punct(Punct::Dot));
        let call_head =
            matches!(t.kind, TokKind::Word { .. }) && next == Some(TokKind::Punct(Punct::LParen));
        let kws = match t.kind {
            TokKind::Word { func: Some(_), .. } if call_head => KwSet::default(),
            TokKind::Word { kws, .. } if !name_next && !before_dot => kws,
            _ => KwSet::default(),
        };
        let join_kind = [Kw::Inner, Kw::Left, Kw::Right, Kw::Full]
            .iter()
            .any(|k| kws.contains(*k));
        let dotted_join = before_dot
            && !name_next
            && !call_head
            && prev_ends_operand
            && match t.kind {
                TokKind::Word { kws, .. } => {
                    kws.contains(Kw::Join)
                        || [Kw::Inner, Kw::Left, Kw::Right, Kw::Full]
                            .iter()
                            .any(|k| kws.contains(*k))
                }
                _ => false,
            };
        let opens = match t.kind {
            TokKind::Punct(Punct::LParen | Punct::LBrace) => Some(Open::Bracket),
            _ if kws.contains(Kw::Case) => Some(Open::Case),
            _ if dotted_join || join_kind || (kws.contains(Kw::Join) && !join_head) => {
                Some(Open::Join)
            }
            _ => None,
        };
        let closes = match t.kind {
            TokKind::Punct(Punct::RParen | Punct::RBrace) => depth > 0,
            _ => {
                depth > 0
                    && ((kws.contains(Kw::End) && bit(&case, depth - 1))
                        || (kws.contains(Kw::On) && bit(&join, depth - 1)))
            }
        };
        if closes {
            if let Some(r) = runs.get_mut(depth as usize) {
                sum = sum.saturating_sub(r.levels());
                *r = Runs::default();
            }
            depth -= 1;
        } else if let Some(r) = runs.get_mut(depth as usize) {
            let before = r.levels();
            let unary_sign =
                matches!(t.kind, TokKind::Punct(Punct::Minus | Punct::Plus)) && !prev_ends_operand;
            if (prev_ends_operand || not_after_operand)
                && [Kw::In, Kw::Between, Kw::Is, Kw::Like, Kw::Refs]
                    .iter()
                    .any(|k| kws.contains(*k))
            {
                r.cmp = r.cmp.saturating_add(1);
            }
            match t.kind {
                _ if unary_sign => r.sign = r.sign.saturating_add(1),
                TokKind::Punct(
                    Punct::Eq | Punct::Ne | Punct::Lt | Punct::Gt | Punct::Le | Punct::Ge,
                ) => r.cmp = r.cmp.saturating_add(1),
                TokKind::Punct(Punct::Comma | Punct::Semi) => {
                    *r = Runs::default();
                    put(&mut between, depth, false);
                }
                _ if kws.contains(Kw::Not) && !prev_ends_operand => {
                    r.not = r.not.saturating_add(1);
                }
                _ if kws.contains(Kw::And) && bit(&between, depth) => {
                    put(&mut between, depth, false);
                }
                _ if kws.intersects(kw::CMP_RUN_END) => {
                    *r = Runs::default();
                    put(&mut between, depth, false);
                }
                _ if kws.contains(Kw::Between) => put(&mut between, depth, true),
                _ => {}
            }
            if !unary_sign && opens.is_none() && !call_head {
                r.sign = 0;
            }
            sum = sum.saturating_sub(before).saturating_add(r.levels());
        }
        if let Some(open) = opens {
            put(&mut case, depth, open == Open::Case);
            put(&mut join, depth, open == Open::Join);
            depth += 1;
            put(&mut between, depth, false);
        }
        not_after_operand = prev_ends_operand && kws.contains(Kw::Not);
        prev_ends_operand = match t.kind {
            TokKind::Number => !after_top,
            TokKind::Str | TokKind::Param => true,
            TokKind::Punct(Punct::RParen | Punct::RBrace) => true,
            TokKind::Word { .. } => {
                kws.contains(Kw::End)
                    || !(kws.intersects(kw::RESERVED_PREFIX) || kws.contains(Kw::Case))
            }
            _ => false,
        };
        name_next = t.kind == TokKind::Punct(Punct::Dot) || kws.contains(Kw::As);
        join_head = join_kind || (join_head && kws.contains(Kw::Outer));
        after_top = kws.contains(Kw::Top);
        max = max.max(depth.saturating_add(sum));
        if max > MAX_DEPTH {
            return MAX_DEPTH + 1;
        }
    }
    max
}

#[allow(clippy::expect_used)]
pub fn with_deep_stack<R: Send>(depth: u32, f: impl FnOnce() -> R + Send) -> R {
    if depth <= INLINE_DEPTH {
        return f();
    }
    let size = (4 << 20) + (64 << 10) * depth.min(MAX_DEPTH + 1) as usize;
    let mut job = Some(f);
    let mut out = None;
    std::thread::scope(|s| {
        let (job, out) = (&mut job, &mut out);
        let spawned = std::thread::Builder::new()
            .stack_size(size)
            .spawn_scoped(s, move || *out = job.take().map(|f| f()));
        if let Ok(h) = spawned
            && let Err(p) = h.join()
        {
            std::panic::resume_unwind(p);
        }
    });
    if let Some(f) = job.take() {
        out = Some(f());
    }
    out.expect("the job ran on the thread or inline")
}

impl Drop for ast::Batch {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let stmts = std::mem::take(&mut self.stmts);
            with_deep_stack(self.max_depth, move || drop(stmts));
        }
    }
}

impl Drop for ast::AccessCondition {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let null = ast::Expr {
                kind: ast::ExprKind::Lit(ast::Lit::Null),
                span: Span::new(0, 0),
            };
            let form = std::mem::replace(
                &mut self.form,
                ast::AccessForm::Short {
                    where_kw: Span::new(0, 0),
                    cond: null,
                },
            );
            with_deep_stack(self.max_depth, move || drop(form));
        }
    }
}

#[doc(hidden)]
pub mod probe {
    use std::cell::Cell;

    thread_local! {
        static WORK: Cell<u64> = const { Cell::new(0) };
    }

    pub fn reset() {
        WORK.with(|w| w.set(0));
    }

    pub fn get() -> u64 {
        WORK.with(Cell::get)
    }

    #[inline]
    pub fn hit() {
        WORK.with(|w| w.set(w.get().wrapping_add(1)));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicParseError {
    Structure(PublicParseLimit),
    Memory(memory_budget::BudgetError),
}
impl std::fmt::Display for PublicParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::Structure(limit) => f.write_str(match limit {
                PublicParseLimit::Statements => "MEMORY_LIMIT: compile statements",
                PublicParseLimit::Columns => "MEMORY_LIMIT: compile columns",
                PublicParseLimit::Depth => "MEMORY_LIMIT: compile depth",
            }),
        }
    }
}
impl std::error::Error for PublicParseError {}
pub struct BudgetedParsed {
    parsed: Parsed,
    _scope: resource::ParseScope,
}
impl BudgetedParsed {
    pub fn parsed(&self) -> &Parsed {
        &self.parsed
    }
}
impl std::fmt::Debug for BudgetedParsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetedParsed")
            .field("batch_ok", &self.parsed.batch.is_ok())
            .finish()
    }
}
pub fn parse_batch_budgeted(
    text: &str,
    mode: ParseMode,
    memory: &memory_budget::RequestMemory,
) -> Result<BudgetedParsed, PublicParseError> {
    if text.len() > (u32::MAX / 2) as usize {
        return Err(PublicParseError::Memory(
            memory_budget::BudgetError::Overflow,
        ));
    }
    preflight_public(text).map_err(PublicParseError::Structure)?;
    let scope = resource::ParseScope::new(&memory.budget).map_err(PublicParseError::Memory)?;
    let result = worker_parse::parse_paid(text, mode, &scope, false);
    if let Some(error) = scope.error() {
        return Err(PublicParseError::Memory(error));
    }
    match result {
        Ok((_, Some(limit), _, _)) => Err(PublicParseError::Structure(limit)),
        Ok((parsed, None, _, _)) => Ok(BudgetedParsed {
            parsed,
            _scope: scope,
        }),
        Err(error) => Ok(BudgetedParsed {
            parsed: Parsed {
                batch: Err(error),
                values: Vec::new(),
            },
            _scope: scope,
        }),
    }
}
