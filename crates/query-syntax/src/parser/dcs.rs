use super::query::is_nested_table;
use super::{PResult, Parser};
use crate::ParseMode;
use crate::ast::{
    CharSource, Characteristics, DcsBlock, DcsItem, DcsKind, DcsList, Expr, ExprKind, OrderDir,
    Path,
};
use crate::kw::{Kw, KwSet};
use crate::lexer::{Punct, TokKind};
use crate::source::Span;

impl<'t> Parser<'t> {
    pub fn dcs(&self) -> bool {
        self.mode == ParseMode::Dcs
    }

    pub fn dcs_stop_word(&self, kws: KwSet) -> bool {
        self.dcs() && kws.contains(Kw::Characteristics)
    }

    pub fn dcs_blocks(&mut self, out: &mut Vec<DcsBlock>) -> PResult<()> {
        while self.dcs() && self.at_punct(Punct::LBrace) && !self.nth_kw(1, Kw::Characteristics) {
            crate::resource::push(self.memory, &mut *out, self.dcs_block()?)?;
        }
        Ok(())
    }

    pub fn skip_query_characteristics(&mut self) -> PResult<bool> {
        if self.dcs() || !self.at_punct(Punct::LBrace) || !self.nth_kw(1, Kw::Characteristics) {
            return Ok(false);
        }
        self.dcs_block()?;
        Ok(true)
    }

    pub fn query_dcs_block(&mut self) -> PResult<Option<DcsBlock>> {
        if !self.dcs() {
            return Ok(None);
        }
        if self.at_punct(Punct::LBrace) {
            return Ok(Some(self.dcs_block()?));
        }
        if !self.at_kw(Kw::Characteristics) {
            return Ok(None);
        }
        let c = self.characteristics()?;
        Ok(Some(DcsBlock {
            span: Span::new(c.kw.start, self.prev_end()),
            kind: DcsKind::Characteristics(crate::resource::boxed(self.memory, c)?),
        }))
    }

    fn dcs_block(&mut self) -> PResult<DcsBlock> {
        let open = self.tok().span;
        self.enter(open.start)?;
        self.bump()?;
        let kind = if self.eat_kw(Kw::Select)?.is_some() {
            DcsKind::Select(self.dcs_items()?)
        } else if self.eat_kw(Kw::Where)?.is_some() {
            DcsKind::Where(self.dcs_items()?)
        } else if self.eat_kw(Kw::Order)?.is_some() {
            self.expect_kw(Kw::By)?;
            DcsKind::OrderBy(self.dcs_items()?)
        } else if self.eat_kw(Kw::Group)?.is_some() {
            self.expect_kw(Kw::By)?;
            DcsKind::GroupBy(self.dcs_items()?)
        } else if self.eat_kw(Kw::Totals)?.is_some() {
            self.eat_lead_kw(Kw::By)?;
            DcsKind::Totals(self.dcs_items()?)
        } else if self.at_kw(Kw::Characteristics) {
            DcsKind::Characteristics(crate::resource::boxed(
                self.memory,
                self.characteristics()?,
            )?)
        } else {
            return Err(self.unexpected());
        };
        let close = self.expect_punct(Punct::RBrace)?;
        self.leave();
        Ok(DcsBlock {
            kind,
            span: open.to(close),
        })
    }

    pub fn dcs_vt_arg(&mut self) -> PResult<Expr> {
        let (list, span) = self.dcs_group_inner(true)?;
        Ok(Expr {
            kind: ExprKind::Dcs(list),
            span,
        })
    }

    pub fn dcs_group(&mut self) -> PResult<(DcsList, Span)> {
        self.dcs_group_inner(false)
    }

    fn dcs_group_inner(&mut self, vt_argument: bool) -> PResult<(DcsList, Span)> {
        let open = self.tok().span;
        self.enter(open.start)?;
        self.bump()?;
        let mut list = self.dcs_items()?;
        let observed_stars = list.items.len() == 4 && list.items.iter().all(|item| {
            item.star.is_some() && item.nested.is_none() && item.dcs.is_empty()
                && item.dir.is_none() && item.alias.is_none()
                && matches!(&item.expr.kind, ExprKind::Paren(inner) if matches!(inner.kind, ExprKind::Path(_)))
        });
        if vt_argument && self.dcs() && observed_stars && self.tok().kind == TokKind::Param {
            let parameter = self.param()?;
            list.span = list.span.to(parameter.span);
            list.unresolved_tail = Some(crate::ast::DcsUnresolvedTail::AdjacentParam(parameter));
        }
        let close = self.expect_punct(Punct::RBrace)?;
        self.leave();
        Ok((list, open.to(close)))
    }

    fn dcs_items(&mut self) -> PResult<DcsList> {
        let start = self.tok().span.start;
        let mut items = crate::resource::list(self.memory, [self.dcs_item()?])?;
        while self.eat_punct(Punct::Comma)?.is_some() {
            crate::resource::push(self.memory, &mut items, self.dcs_item()?)?;
        }
        Ok(DcsList {
            items,
            unresolved_tail: None,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn dcs_item(&mut self) -> PResult<DcsItem> {
        let wrapped_star = self.parenthesized_dcs_star();
        if wrapped_star {
            self.open_paren()?;
            self.enter_bracket(false)?;
        }
        let expr = self.expr()?;
        let (mut star, mut nested) = (None, None);
        if self.at_punct(Punct::Dot) && self.nth_punct(1, Punct::Star) {
            let dot = self.bump()?.span;
            star = Some(dot.to(self.bump()?.span));
        } else if is_nested_table(&expr)
            && self.at_punct(Punct::Dot)
            && self.nth_punct(1, Punct::LParen)
        {
            self.bump()?;
            self.open_paren()?;
            nested = Some(self.dcs_items()?);
            self.close_paren()?;
        }
        if wrapped_star {
            self.close_paren()?;
            self.leave_bracket(false);
        }
        let mut dcs = Vec::new();
        while self.at_punct(Punct::LBrace) {
            crate::resource::push(self.memory, &mut dcs, self.dcs_group()?.0)?;
        }
        let dir = if let Some(s) = self.eat_kw(Kw::Asc)? {
            Some((OrderDir::Asc, s))
        } else if let Some(s) = self.eat_kw(Kw::Desc)? {
            Some((OrderDir::Desc, s))
        } else {
            self.eat_kw(Kw::Hierarchy)?
                .map(|s| (OrderDir::Hierarchy, s))
        };
        let alias = self.alias()?;
        Ok(DcsItem {
            expr,
            star,
            nested,
            dcs,
            dir,
            alias,
        })
    }

    fn parenthesized_dcs_star(&self) -> bool {
        if !self.at_punct(Punct::LParen) {
            return false;
        }
        let mut k = 1;
        loop {
            if !matches!(self.nth(k).kind, TokKind::Word { .. })
                || !self.nth_punct(k + 1, Punct::Dot)
            {
                return false;
            }
            if self.nth_punct(k + 2, Punct::Star) {
                return self.nth_punct(k + 3, Punct::RParen);
            }
            k += 2;
        }
    }

    fn characteristics(&mut self) -> PResult<Characteristics> {
        let kw = self.bump()?.span;
        self.expect_kw(Kw::Type)?;
        self.open_paren()?;
        let type_name = self.name_path()?;
        self.close_paren()?;
        self.expect_kw(Kw::CharacteristicTypes)?;
        let kinds = self.char_source()?;
        let key_field = self.char_field(Kw::KeyField)?;
        let name_field = self.char_field(Kw::NameField)?;
        let value_type_field = self.opt_char_field(Kw::ValueTypeField)?;
        self.expect_kw(Kw::CharacteristicValues)?;
        let values = self.char_source()?;
        let object_field = self.char_field(Kw::ObjectField)?;
        let kind_field = self.char_field(Kw::TypeField)?;
        let value_field = self.opt_char_field(Kw::ValueField)?;
        Ok(Characteristics {
            kw,
            type_name,
            kinds,
            key_field,
            name_field,
            value_type_field,
            values,
            object_field,
            kind_field,
            value_field,
        })
    }

    fn char_source(&mut self) -> PResult<CharSource> {
        if !self.at_punct(Punct::LParen) {
            return Ok(CharSource::Table(self.name_path()?));
        }
        self.open_paren()?;
        let query = self.query(false)?;
        self.close_paren()?;
        Ok(CharSource::Query(crate::resource::boxed(
            self.memory,
            query,
        )?))
    }

    fn char_field(&mut self, word: Kw) -> PResult<Path> {
        self.expect_kw(word)?;
        self.name_path()
    }

    fn opt_char_field(&mut self, word: Kw) -> PResult<Option<Path>> {
        match self.eat_kw(word)? {
            Some(_) => Ok(Some(self.name_path()?)),
            None => Ok(None),
        }
    }
}
