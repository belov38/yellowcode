mod expr;
mod module;
mod stmt;

use crate::ast::{BinOp, Expr, Ident, MethodKind, Module, Region, Stmt, StmtKind};
use crate::diag::{MsgId, SyntaxError, msg};
use crate::lexer::{Token, TokenKind, tokenize};
use crate::preproc::{ExecContext, Origin, PreprocSymbols, preprocess};
use crate::source::{SourceText, Span};

pub const MAX_NESTING: u32 = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseMode {
    Module,
    Fragment,
    Expression,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BuiltinSig {
    pub min: u8,
    pub max: Option<u8>,
}

pub trait BuiltinTable {
    fn builtin(&self, name: &str) -> Option<BuiltinSig>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoBuiltins;

impl BuiltinTable for NoBuiltins {
    fn builtin(&self, _name: &str) -> Option<BuiltinSig> {
        None
    }
}

#[derive(Clone, Copy)]
pub struct ParseOptions<'a> {
    pub mode: ParseMode,
    pub context: ExecContext,
    pub methods_only: bool,
    pub extension: bool,
    pub builtins: &'a dyn BuiltinTable,
}

impl std::fmt::Debug for ParseOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseOptions")
            .field("mode", &self.mode)
            .field("context", &self.context)
            .field("methods_only", &self.methods_only)
            .field("extension", &self.extension)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseOutput {
    pub module: Module,
    pub error: Option<SyntaxError>,
}

pub fn parse(src: &SourceText, opts: &ParseOptions<'_>) -> ParseOutput {
    let lexed = tokenize(src);
    let symbols = match opts.mode {
        ParseMode::Module => PreprocSymbols::for_context(opts.context),
        ParseMode::Fragment | ParseMode::Expression => PreprocSymbols::none(),
    };
    let pre = preprocess(src, &lexed, symbols, opts.extension);
    let regions = pre
        .regions
        .iter()
        .map(|&(name, span)| Region {
            name: Ident {
                name: src.text(name),
                span: name,
            },
            span,
        })
        .collect();
    let mut p = Parser::new(src, &pre.tokens, lexed.strings, opts.builtins);
    p.origin = &pre.origin;
    p.deleted = &pre.deleted;
    let mut module = match opts.mode {
        ParseMode::Expression => Module {
            body: p.value_text(),
            ..Module::default()
        },
        ParseMode::Fragment => Module {
            body: p.text_body(),
            ..Module::default()
        },
        ParseMode::Module => p.module(opts.methods_only),
    };
    module.regions = regions;
    module.incomplete = p.error.is_some();
    ParseOutput {
        module,
        error: p.error,
    }
}

pub(crate) struct Parser<'a> {
    src: &'a SourceText,
    toks: &'a [Token],
    strings: Vec<Box<[u16]>>,
    pos: usize,
    last_good_end: u32,
    error: Option<SyntaxError>,
    depth: u32,
    builtins: &'a dyn BuiltinTable,
    is_async: bool,
    ops: Vec<(BinOp, Expr)>,
    loops: u32,
    handlers: u32,
    method: Option<MethodKind>,
    body: stmt::BodyState,
    origin: &'a [Origin],
    deleted: &'a [(u32, Vec<Token>)],
    module_body: bool,
}

const EOF: Token = Token {
    kind: TokenKind::Eof,
    span: Span { start: 0, end: 0 },
};

impl<'a> Parser<'a> {
    fn new(
        src: &'a SourceText,
        toks: &'a [Token],
        strings: Vec<Box<[u16]>>,
        builtins: &'a dyn BuiltinTable,
    ) -> Self {
        Parser {
            src,
            toks,
            strings,
            pos: 0,
            last_good_end: 0,
            error: None,
            depth: 0,
            builtins,
            is_async: false,
            ops: Vec::new(),
            loops: 0,
            handlers: 0,
            method: None,
            body: stmt::BodyState::default(),
            origin: &[],
            deleted: &[],
            module_body: false,
        }
    }

    fn tok(&self) -> Token {
        match self.toks.get(self.pos) {
            Some(&t) => t,
            None => self.toks.last().copied().unwrap_or(EOF),
        }
    }

    fn kind(&self) -> TokenKind {
        self.tok().kind
    }

    fn kind_at(&self, n: usize) -> TokenKind {
        self.toks
            .get(self.pos.saturating_add(n))
            .map_or(TokenKind::Eof, |t| t.kind)
    }

    fn bump(&mut self) {
        if let Some(t) = self.toks.get(self.pos)
            && self.pos + 1 < self.toks.len()
        {
            self.last_good_end = t.span.end;
            self.pos += 1;
        }
    }

    fn failed(&self) -> bool {
        self.error.is_some()
    }

    fn fail_after(&mut self, m: MsgId, arg: Option<String>) {
        self.fail_at(m, arg, self.last_good_end);
    }

    fn fail_at(&mut self, m: MsgId, arg: Option<String>, offset: u32) {
        if self.error.is_some() {
            return;
        }
        let t = self.tok();
        let (m, arg, offset) = match t.kind {
            TokenKind::Bad(b) if b == msg::MIS_DOUBLEQUOT || b == msg::MIS_SINGLEQUOT => {
                (b, None, t.span.end)
            }
            TokenKind::Bad(b) => (b, None, t.span.start),
            _ => (m, arg, offset),
        };
        self.fail_exact(m, arg, offset);
    }

    fn fail_exact(&mut self, m: MsgId, arg: Option<String>, offset: u32) {
        if self.error.is_none() {
            self.error = Some(SyntaxError {
                msg: m,
                arg,
                pos: self.src.pos(offset),
            });
        }
    }

    fn enter(&mut self) -> bool {
        if self.depth >= MAX_NESTING {
            let at = self.tok().span.start;
            self.fail_at(msg::UNKNOWN, None, at);
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn ident(&self, t: Token) -> Ident {
        Ident {
            name: self.src.text(t.span),
            span: t.span,
        }
    }

    fn units(&self, span: Span) -> &'a [u16] {
        self.src
            .units()
            .get(span.start as usize..span.end as usize)
            .unwrap_or(&[])
    }

    fn value_text(&mut self) -> Vec<Stmt> {
        let start = self.tok().span.start;
        let kind = match self.expr(msg::EXP_EXPR) {
            Ok(e) => {
                if self.kind() != TokenKind::Eof {
                    let at = self.tok().span.start;
                    self.fail_at(msg::UNRECOGN_STMT, None, at);
                }
                StmtKind::Value(e)
            }
            Err(parts) => StmtKind::Incomplete(parts),
        };
        vec![Stmt {
            kind,
            span: Span {
                start,
                end: self.last_good_end.max(start),
            },
        }]
    }
}
