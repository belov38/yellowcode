use crate::resource::AliasScope;

use super::{PResult, Parser, reserved_prefix};
use crate::ast::{Alias, FromClause, Join, JoinKind, OrderDir, Source, TableRef, VtArg, VtArgs};
use crate::kw::Kw;
use crate::lexer::{Punct, TokKind};
use crate::msg::MsgId;
use crate::source::Span;

const JOIN_WORDS: [(Kw, JoinKind); 5] = [
    (Kw::Join, JoinKind::Inner),
    (Kw::Inner, JoinKind::Inner),
    (Kw::Left, JoinKind::Left),
    (Kw::Right, JoinKind::Right),
    (Kw::Full, JoinKind::Full),
];

impl<'t> Parser<'t> {
    #[allow(clippy::wrong_self_convention)]
    pub fn from_clause(&mut self) -> PResult<FromClause> {
        let kw = self.expect_kw(Kw::From)?;
        let mut scope = AliasScope::new(self.memory);
        let mut sources = crate::resource::list(self.memory, [self.source(&mut scope, false)?])?;
        while self.eat_source_comma()?.is_some() {
            match sources.last_mut() {
                Some(prev) if self.at_join(true).is_some() => {
                    self.joins(&mut scope, &mut prev.joins, true)?;
                    prev.span = Span::new(prev.span.start, self.prev_end());
                }
                _ => crate::resource::push(
                    self.memory,
                    &mut sources,
                    self.source(&mut scope, true)?,
                )?,
            }
        }
        Ok(FromClause { kw, sources })
    }

    fn eat_source_comma(&mut self) -> PResult<Option<Span>> {
        self.hash_lead = true;
        let r = self.eat_punct(Punct::Comma);
        self.hash_lead = false;
        r
    }

    pub fn source(&mut self, scope: &mut AliasScope, after_join: bool) -> PResult<Source> {
        let start = self.tok().span.start;
        let table = self.table(after_join)?;
        let alias = self.source_alias()?;
        if let Some(a) = &alias {
            self.declare_source_alias(scope, a)?;
        }
        let mut joins = Vec::new();
        self.joins(scope, &mut joins, false)?;
        Ok(Source {
            table,
            alias,
            joins,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn joins(
        &mut self,
        scope: &mut AliasScope,
        joins: &mut Vec<Join>,
        mut after_comma: bool,
    ) -> PResult<()> {
        while let Some((kind, optional)) = self.at_join(after_comma) {
            after_comma = false;
            if optional {
                self.optional_joins(scope, kind, joins)?;
            } else {
                crate::resource::push(self.memory, &mut *joins, self.join(scope, kind)?)?;
            }
        }
        Ok(())
    }

    fn at_join(&self, after_comma: bool) -> Option<(JoinKind, bool)> {
        if let Some(kind) = self.nth_join_kind(0) {
            return Some((kind, false));
        }
        if (self.dcs() || after_comma) && self.at_punct(Punct::LBrace) {
            return self.nth_join_kind(1).map(|kind| (kind, true));
        }
        None
    }

    #[inline(never)]
    fn optional_joins(
        &mut self,
        scope: &mut AliasScope,
        kind: JoinKind,
        joins: &mut Vec<Join>,
    ) -> PResult<()> {
        let open = self.tok().span;
        self.enter(open.start)?;
        self.bump()?;
        let mut block = crate::resource::list(self.memory, [self.join(scope, kind)?])?;
        self.joins(scope, &mut block, false)?;
        self.expect_punct(Punct::RBrace)?;
        self.leave();
        let optional = self.dcs();
        for j in block {
            crate::resource::push(self.memory, &mut *joins, Join { optional, ..j })?;
        }
        Ok(())
    }

    fn table(&mut self, after_join: bool) -> PResult<TableRef> {
        match self.tok().kind {
            TokKind::Word { kws, .. } if !reserved_prefix(kws) => {
                let path = self.table_path()?;
                let args = if self.at_punct(Punct::LParen) {
                    Some(self.vt_args()?)
                } else {
                    None
                };
                Ok(TableRef::Named { path, args })
            }
            _ if self.at_hash() => {
                let path = self.table_path()?;
                let args = if self.at_punct(Punct::LParen) {
                    Some(self.vt_args()?)
                } else {
                    None
                };
                Ok(TableRef::Named { path, args })
            }
            TokKind::Param => Ok(TableRef::Param(self.param()?)),
            TokKind::Punct(Punct::LParen) => self.subquery(),
            _ => Err(self.err_here(if after_join {
                MsgId::SELECT_ERROR_TABLENAMEEXPECTED
            } else {
                MsgId::SELECT_ERROR_INVALIDJOIN
            })),
        }
    }

    fn table_path(&mut self) -> PResult<crate::ast::Path> {
        let mut parts = crate::resource::list(self.memory, [self.hash_name()?])?;
        loop {
            let glued_hash = self.nth_punct(0, Punct::Dot) && {
                let n = self.nth(1);
                n.kind == TokKind::Error(crate::lexer::LexErr::BadChar)
                    && n.span.text(self.text) == "#"
                    && n.span.start == self.tok().span.end
            };
            if !(self.at_punct(Punct::Dot)
                && (glued_hash || matches!(self.nth(1).kind, TokKind::Word { .. })))
            {
                break;
            }
            self.hash_cont = glued_hash;
            let dot = self.bump();
            self.hash_cont = false;
            dot?;
            crate::resource::push(self.memory, &mut parts, self.hash_name()?)?;
        }
        if self.at_punct(Punct::Dot)
            && matches!(self.nth(1).kind, TokKind::Error(_))
            && self.nth(1).span.text(self.text).starts_with('[')
        {
            self.bump()?;
        }
        Ok(crate::ast::Path { parts })
    }

    fn subquery(&mut self) -> PResult<TableRef> {
        let open = self.open_paren()?;
        let query = crate::resource::boxed(self.memory, self.query(false)?)?;
        let close = self.close_paren()?;
        Ok(TableRef::Subquery {
            query,
            span: open.to(close),
        })
    }

    fn vt_args(&mut self) -> PResult<VtArgs> {
        let open = self.open_paren()?;
        let mut args = Vec::new();
        let close = if self.at_punct(Punct::RParen) {
            self.close_paren()?
        } else {
            loop {
                crate::resource::push(self.memory, &mut args, self.vt_arg()?)?;
                if self.eat_punct(Punct::Comma)?.is_none() {
                    break self.close_paren()?;
                }
            }
        };
        Ok(VtArgs {
            args,
            span: open.to(close),
        })
    }

    fn vt_arg(&mut self) -> PResult<Option<VtArg>> {
        if self.at_punct(Punct::Comma) || self.at_punct(Punct::RParen) {
            return Ok(None);
        }
        let (expr, dir) = if self.dcs() && self.at_punct(Punct::LBrace) {
            (self.dcs_vt_arg()?, None)
        } else {
            let expr = self.expr()?;
            let dir = if let Some(s) = self.eat_kw(Kw::Asc)? {
                Some((OrderDir::Asc, s))
            } else {
                self.eat_kw(Kw::Desc)?.map(|s| (OrderDir::Desc, s))
            };
            (expr, dir)
        };
        let mut dcs = Vec::new();
        while self.dcs() && self.at_punct(Punct::LBrace) {
            crate::resource::push(self.memory, &mut dcs, self.dcs_group()?.0)?;
        }
        Ok(Some(VtArg { expr, dir, dcs }))
    }

    fn nth_join_kind(&self, k: usize) -> Option<JoinKind> {
        JOIN_WORDS
            .iter()
            .find(|(w, _)| self.nth_kw(k, *w))
            .map(|(_, kind)| *kind)
    }

    fn join(&mut self, scope: &mut AliasScope, kind: JoinKind) -> PResult<Join> {
        let kw = self.tok().span;
        self.enter(kw.start)?;
        if self.eat_kw(Kw::Join)?.is_none() {
            self.bump()?;
            if kind != JoinKind::Inner {
                self.eat_kw(Kw::Outer)?;
            }
            self.expect_kw(Kw::Join)?;
        }
        if self.tok().kind == TokKind::Param {
            return Err(self.err_here(MsgId::SELECT_ERROR_TABLENAMEEXPECTED));
        }
        let right = self.source(scope, true)?;
        self.leave();
        let Some(on_kw) = self.eat_kw(Kw::On)? else {
            return Err(self.err_here(MsgId::SELECT_ERROR_TABLENAMEEXPECTED));
        };
        let on = self.expr()?;
        Ok(Join {
            kind,
            kw,
            right,
            on_kw,
            on,
            optional: false,
        })
    }

    fn declare_source_alias(&self, scope: &mut AliasScope, a: &Alias) -> PResult<()> {
        crate::probe::hit();
        if !scope.insert(
            self.memory,
            crate::resource::alias_key(self.memory, &a.name.text)?,
        )? {
            let at = a.as_kw.unwrap_or(a.name.span).start;
            return Err(self
                .err(MsgId::SELECT_ERROR_DUPLICATEDALIAS, at)
                .with_quoted(crate::resource::arg(self.memory, &a.name.text)));
        }
        Ok(())
    }
}
