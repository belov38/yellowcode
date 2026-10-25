use crate::resource::AliasScope;

use super::{PResult, Parser, reserved_alias, reserved_prefix, reserved_src_alias};
use crate::ast::{
    Alias, Clause, ControlPoint, DropStmt, Expr, ExprKind, ForUpdate, GroupBy, GroupKind,
    Hierarchy, Ident, IndexBy, IndexKind, IndexSet, IntoClause, IntoKind, ItemKind, OrderBy,
    OrderDir, OrderItem, Periods, Query, QueryStmt, Select, SelectItem, Stmt, Top, Totals,
    TotalsItem, UnionOp,
};
use crate::diag::MsgArg;
use crate::kw::Kw;
use crate::lexer::{Punct, TokKind};
use crate::msg::MsgId;
use crate::source::Span;

pub(super) fn is_nested_table(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) => true,
        ExprKind::Cast { deref, .. } | ExprKind::Deref { deref, .. } => !deref.is_empty(),
        _ => false,
    }
}

impl<'t> Parser<'t> {
    pub fn batch(&mut self) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        loop {
            if self.public_limits && stmts.len() >= 256 {
                self.limit_error = Some(crate::PublicParseLimit::Statements);
                return Err(self.err_here(MsgId::SELECT_ERROR_SYNTAXERROR));
            }
            crate::resource::push(self.memory, &mut stmts, self.stmt()?)?;
            if self.eat_punct(Punct::Semi)?.is_some() {
                if self.at_eof() {
                    return Ok(stmts);
                }
            } else if self.at_eof() {
                return Ok(stmts);
            } else {
                return Err(self.unexpected());
            }
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        if self.at_kw(Kw::Drop) {
            let kw = self.bump()?.span;
            let name = self.table_name()?;
            return Ok(Stmt::Drop(DropStmt {
                kw,
                span: kw.to(name.span),
                name,
            }));
        }
        if !self.at_kw(Kw::Select) {
            return Err(self
                .err_here(MsgId::SELECT_ERROR_TOKENEXPECTED)
                .with_quoted(MsgArg::Kw(Kw::Select)));
        }
        let start = self.tok().span.start;
        let query = self.query(true)?;
        Ok(Stmt::Query(crate::resource::boxed(
            self.memory,
            QueryStmt {
                query,
                span: Span::new(start, self.prev_end()),
            },
        )?))
    }

    pub fn query(&mut self, top: bool) -> PResult<Query> {
        let start = self.tok().span.start;
        let mut parts = crate::resource::list(self.memory, [self.select(top)?])?;
        let mut unions = Vec::new();
        while let Some(kw) = self.eat_kw(Kw::Union)? {
            let all = self.eat_kw(Kw::All)?.is_some();
            crate::resource::push(self.memory, &mut unions, UnionOp { kw, all })?;
            crate::resource::push(self.memory, &mut parts, self.select(false)?)?;
        }
        self.query_tail(start, parts, unions)
    }

    #[inline(never)]
    fn query_tail(
        &mut self,
        start: u32,
        mut parts: Vec<Select>,
        unions: Vec<UnionOp>,
    ) -> PResult<Query> {
        let order_by = if self.at_kw(Kw::Order) {
            Some(self.order_by()?)
        } else {
            None
        };
        if order_by.is_some() && self.at_kw(Kw::Index) {
            match parts.last_mut() {
                Some(last) if last.index_by.is_none() => last.index_by = Some(self.index_by()?),
                _ => return Err(self.unexpected()),
            }
        }
        let (mut autoorder, mut totals, mut dcs) = (None, None, Vec::new());
        let placement = parts.first().is_some_and(|part| part.into.is_some());
        let mut terminal_index = false;
        loop {
            if self.at_kw(Kw::AutoOrder) {
                if autoorder.is_some() || terminal_index {
                    return Err(self.unexpected());
                }
                autoorder = Some(self.bump()?.span);
            } else if self.at_kw(Kw::Totals) {
                if totals.is_some() || terminal_index {
                    return Err(self.unexpected());
                }
                totals = Some(self.totals()?);
            } else if placement && self.at_kw(Kw::Index) {
                match parts.last_mut() {
                    Some(last) if last.index_by.is_none() => {
                        last.index_by = Some(self.index_by()?);
                        terminal_index = true;
                    }
                    _ => return Err(self.unexpected()),
                }
            } else if self.skip_query_characteristics()? {
            } else if let Some(block) = self.query_dcs_block()? {
                crate::resource::push(self.memory, &mut dcs, block)?;
            } else {
                break;
            }
        }
        Ok(Query {
            parts,
            unions,
            order_by,
            autoorder,
            totals,
            dcs,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn order_by(&mut self) -> PResult<OrderBy> {
        let kw = self.bump()?.span;
        self.expect_kw(Kw::By)?;
        let mut items = crate::resource::list(self.memory, [self.order_item()?])?;
        while self.eat_punct(Punct::Comma)?.is_some() {
            crate::resource::push(self.memory, &mut items, self.order_item()?)?;
        }
        Ok(OrderBy { kw, items })
    }

    fn order_item(&mut self) -> PResult<OrderItem> {
        let expr = self.expr()?;
        let (dir, dir_kw) = if let Some(h) = self.eat_kw(Kw::Hierarchy)? {
            if let Some(d) = self.eat_kw(Kw::Desc)? {
                (OrderDir::HierarchyDesc, Some(h.to(d)))
            } else {
                let end = self.eat_kw(Kw::Asc)?.unwrap_or(h);
                (OrderDir::Hierarchy, Some(h.to(end)))
            }
        } else if let Some(d) = self.eat_kw(Kw::Desc)? {
            (OrderDir::Desc, Some(d))
        } else if let Some(a) = self.eat_kw(Kw::Asc)? {
            (OrderDir::Asc, Some(a))
        } else {
            (OrderDir::Asc, None)
        };
        Ok(OrderItem { expr, dir, dir_kw })
    }

    fn totals(&mut self) -> PResult<Totals> {
        let kw = self.bump()?.span;
        let mut items = Vec::new();
        if !self.at_lead_kw(Kw::By) {
            loop {
                let expr = self.expr()?;
                let alias = self.alias()?;
                crate::resource::push(self.memory, &mut items, TotalsItem { expr, alias })?;
                if self.eat_punct(Punct::Comma)?.is_none() {
                    break;
                }
            }
        }
        let by_kw = self.expect_kw(Kw::By)?;
        let mut points = crate::resource::list(self.memory, [self.control_point()?])?;
        while self.eat_punct(Punct::Comma)?.is_some() {
            crate::resource::push(self.memory, &mut points, self.control_point()?)?;
        }
        Ok(Totals {
            kw,
            items,
            by_kw,
            points,
        })
    }

    fn control_point(&mut self) -> PResult<ControlPoint> {
        if let Some(s) = self.eat_lead_kw(Kw::Overall)? {
            return Ok(ControlPoint::Overall(s));
        }
        let expr = self.expr()?;
        let start = expr.span.start;
        let hierarchy = if let Some(h) = self.eat_kw(Kw::Hierarchy)? {
            Some(Hierarchy::All(h))
        } else if let Some(only) = self.eat_kw(Kw::Only)? {
            let h = self.expect_kw(Kw::Hierarchy)?;
            Some(Hierarchy::Only(only.to(h)))
        } else {
            None
        };
        let periods = if self.at_kw(Kw::Periods) {
            Some(self.periods()?)
        } else {
            None
        };
        let alias = self.alias()?;
        Ok(ControlPoint::Field {
            expr,
            hierarchy,
            periods,
            alias,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn periods(&mut self) -> PResult<Periods> {
        let kw = self.bump()?.span;
        self.open_paren()?;
        if !matches!(self.tok().kind, TokKind::Word { .. }) {
            return Err(self.unexpected());
        }
        let unit = self.word_ident()?;
        let (mut begin, mut end) = (None, None);
        if self.eat_punct(Punct::Comma)?.is_some() {
            begin = Some(self.expr()?);
            if self.eat_punct(Punct::Comma)?.is_some() {
                end = Some(self.expr()?);
            }
        }
        self.close_paren()?;
        Ok(Periods {
            kw,
            unit,
            begin,
            end,
        })
    }

    fn select(&mut self, top: bool) -> PResult<Select> {
        let kw = self.expect_kw(Kw::Select)?;
        let (mut allowed, mut distinct, mut first_n) = (None, None, None);
        loop {
            if self.at_lead_kw(Kw::Allowed) {
                if !top {
                    let mut error = self.err_here(MsgId::SELECT_ERROR_ALLOWEDONLYATFIRST);
                    error.subst = crate::resource::list(self.memory, [MsgArg::Kw(Kw::Allowed)])?;
                    return Err(error);
                }
                if allowed.is_some() {
                    return Err(self.unexpected());
                }
                allowed = Some(self.bump()?.span);
            } else if self.at_lead_kw(Kw::Distinct) {
                if distinct.is_some() {
                    return Err(self.unexpected());
                }
                distinct = Some(self.bump()?.span);
            } else if self.at_lead_kw(Kw::Top) {
                if first_n.is_some() {
                    return Err(self.unexpected());
                }
                let top_kw = self.bump()?.span;
                let count_span = self.tok().span;
                first_n = Some(Top {
                    kw: top_kw,
                    count: self.top_count()?,
                    count_span,
                });
            } else {
                break;
            }
        }
        let mut dcs = Vec::new();
        let items = self.items()?;
        self.dcs_blocks(&mut dcs)?;
        let into = self.put_clause()?;
        self.dcs_blocks(&mut dcs)?;
        let from = if self.at_kw(Kw::From) {
            Some(self.from_clause()?)
        } else {
            None
        };
        self.dcs_blocks(&mut dcs)?;
        let where_ = self.clause(Kw::Where)?;
        self.dcs_blocks(&mut dcs)?;
        let group_by = if self.at_kw(Kw::Group) {
            Some(self.group_by()?)
        } else {
            None
        };
        self.dcs_blocks(&mut dcs)?;
        let having = self.clause(Kw::Having)?;
        self.dcs_blocks(&mut dcs)?;
        let for_update = if self.at_kw(Kw::For) {
            Some(self.for_update()?)
        } else {
            None
        };
        let index_by = if self.at_kw(Kw::Index) {
            Some(self.index_by()?)
        } else {
            None
        };
        self.dcs_blocks(&mut dcs)?;
        Ok(Select {
            kw,
            allowed,
            distinct,
            top: first_n,
            items,
            into,
            from,
            where_,
            group_by,
            having,
            for_update,
            index_by,
            dcs,
            span: Span::new(kw.start, self.prev_end()),
        })
    }

    fn for_update(&mut self) -> PResult<ForUpdate> {
        let kw = self.bump()?.span;
        self.expect_kw(Kw::Update)?;
        self.eat_kw(Kw::Of)?;
        let mut tables = Vec::new();
        if matches!(self.tok().kind, TokKind::Word { kws, .. }
            if !reserved_prefix(kws) && !self.dcs_stop_word(kws))
        {
            crate::resource::push(self.memory, &mut tables, self.path()?)?;
            while self.eat_punct(Punct::Comma)?.is_some() {
                crate::resource::push(self.memory, &mut tables, self.name_path()?)?;
            }
        }
        Ok(ForUpdate { kw, tables })
    }

    fn index_by(&mut self) -> PResult<IndexBy> {
        let kw = self.bump()?.span;
        self.expect_kw(Kw::By)?;
        let kind = if self.eat_lead_kw(Kw::Sets)?.is_some() {
            self.open_paren()?;
            let mut sets = crate::resource::list(self.memory, [self.index_set()?])?;
            while self.eat_punct(Punct::Comma)?.is_some() {
                crate::resource::push(self.memory, &mut sets, self.index_set()?)?;
            }
            self.close_paren()?;
            IndexKind::Sets(sets)
        } else {
            let fields = self.expr_list()?;
            let unique = self.eat_kw(Kw::Unique)?;
            IndexKind::Fields { fields, unique }
        };
        Ok(IndexBy { kw, kind })
    }

    fn index_set(&mut self) -> PResult<IndexSet> {
        self.open_paren()?;
        let fields = self.expr_list()?;
        self.close_paren()?;
        let unique = self.eat_kw(Kw::Unique)?;
        Ok(IndexSet { fields, unique })
    }

    fn put_clause(&mut self) -> PResult<Option<IntoClause>> {
        let kind = if self.at_kw(Kw::Into) {
            IntoKind::Put
        } else if self.at_kw(Kw::Add) {
            IntoKind::Add
        } else {
            return Ok(None);
        };
        let kw = self.bump()?.span;
        let name = self.table_name()?;
        Ok(Some(IntoClause { kind, kw, name }))
    }

    fn table_name(&mut self) -> PResult<Ident> {
        match self.tok().kind {
            TokKind::Word { kws, .. } if !reserved_prefix(kws) => self.hash_name(),
            _ if self.at_hash() => self.hash_name(),
            _ => Err(self.err_here(MsgId::SELECT_ERROR_TABLENAMEEXPECTED)),
        }
    }

    pub(super) fn clause(&mut self, word: Kw) -> PResult<Option<Clause>> {
        match self.eat_kw(word)? {
            Some(kw) => Ok(Some(Clause {
                kw,
                expr: self.expr()?,
            })),
            None => Ok(None),
        }
    }

    fn group_by(&mut self) -> PResult<GroupBy> {
        let kw = self.bump()?.span;
        self.expect_kw(Kw::By)?;
        let kind = if self.eat_lead_kw(Kw::Grouping)?.is_some() {
            self.expect_kw(Kw::Sets)?;
            self.open_paren()?;
            let mut sets = crate::resource::list(self.memory, [self.grouping_set()?])?;
            while self.eat_punct(Punct::Comma)?.is_some() {
                crate::resource::push(self.memory, &mut sets, self.grouping_set()?)?;
            }
            self.close_paren()?;
            GroupKind::Sets(sets)
        } else {
            GroupKind::Exprs(self.expr_list()?)
        };
        Ok(GroupBy { kw, kind })
    }

    fn grouping_set(&mut self) -> PResult<Vec<Expr>> {
        if !self.at_punct(Punct::LParen) {
            return crate::resource::list(self.memory, [self.expr()?]);
        }
        self.open_paren()?;
        let set = if self.at_punct(Punct::RParen) {
            Vec::new()
        } else {
            self.expr_list()?
        };
        self.close_paren()?;
        Ok(set)
    }

    fn items(&mut self) -> PResult<Vec<SelectItem>> {
        let mut items = Vec::new();
        let mut seen: AliasScope = AliasScope::new(self.memory);
        loop {
            if self.public_limits && items.len() >= 4096 {
                self.limit_error = Some(crate::PublicParseLimit::Columns);
                return Err(self.err_here(MsgId::SELECT_ERROR_SYNTAXERROR));
            }
            let item = self.item()?;
            crate::probe::hit();
            if let Some(a) = &item.alias
                && !seen.insert(
                    self.memory,
                    crate::resource::alias_key(self.memory, &a.name.text)?,
                )?
            {
                return Err(self
                    .err(MsgId::SELECT_ERROR_DUPLICATEDALIAS, item.span.start)
                    .with_quoted(crate::resource::arg(self.memory, &a.name.text)));
            }
            crate::resource::push(self.memory, &mut items, item)?;
            if self.eat_punct(Punct::Comma)?.is_none() {
                return Ok(items);
            }
        }
    }

    fn item(&mut self) -> PResult<SelectItem> {
        let start = self.tok().span.start;
        let kind = if self.eat_punct(Punct::Star)?.is_some() {
            ItemKind::Star
        } else if self.at_kw(Kw::EmptyTable)
            && self.nth_punct(1, Punct::Dot)
            && self.nth_punct(2, Punct::LParen)
        {
            self.empty_table()?
        } else {
            let dot_then = |p: &Self, next: Punct| p.at_punct(Punct::Dot) && p.nth_punct(1, next);
            match self.expr()? {
                Expr {
                    kind: ExprKind::Path(path),
                    ..
                } if dot_then(self, Punct::Star) => {
                    self.bump()?;
                    self.bump()?;
                    ItemKind::PathStar(path)
                }
                table if is_nested_table(&table) && dot_then(self, Punct::LParen) => {
                    self.bump()?;
                    self.open_paren()?;
                    let fields = self.items()?;
                    self.close_paren()?;
                    ItemKind::Nested { table, fields }
                }
                e => ItemKind::Expr(e),
            }
        };
        let alias = if self.at_lead_kw(Kw::End) {
            Some(Alias {
                name: self.word_ident()?,
                as_kw: None,
            })
        } else {
            self.alias()?
        };
        Ok(SelectItem {
            kind,
            alias,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn empty_table(&mut self) -> PResult<ItemKind> {
        let kw = self.bump()?.span;
        self.bump()?;
        self.bump()?;
        let mut names = Vec::new();
        loop {
            match self.tok().kind {
                TokKind::Word { .. } => {
                    crate::resource::push(self.memory, &mut names, Some(self.word_ident()?))?
                }
                TokKind::Punct(Punct::Comma) => {
                    crate::resource::push(self.memory, &mut names, None)?
                }
                TokKind::Punct(Punct::RParen) if !names.is_empty() => {
                    crate::resource::push(self.memory, &mut names, None)?
                }
                _ => return Err(self.unexpected()),
            }
            if self.eat_punct(Punct::Comma)?.is_none() {
                break;
            }
        }
        self.expect_punct(Punct::RParen)?;
        Ok(ItemKind::EmptyTable { kw, names })
    }

    pub fn alias(&mut self) -> PResult<Option<Alias>> {
        self.alias_in(false)
    }

    pub fn source_alias(&mut self) -> PResult<Option<Alias>> {
        self.alias_in(true)
    }

    fn check_alias_hash(&self, name: &Ident) -> PResult<()> {
        if name.text.contains('#') {
            return Err(self
                .err(MsgId::SELECT_ERROR_INVALIDALIAS, name.span.start)
                .with_quoted(crate::resource::arg(self.memory, &name.text)));
        }
        Ok(())
    }

    fn alias_in(&mut self, source: bool) -> PResult<Option<Alias>> {
        if let Some(as_kw) = self.eat_kw(Kw::As)? {
            let t = self.tok();
            return match t.kind {
                TokKind::Word { kws, .. }
                    if !(if source {
                        reserved_src_alias(kws)
                    } else {
                        reserved_alias(kws)
                    }) =>
                {
                    let name = self.word_ident()?;
                    self.check_alias_hash(&name)?;
                    Ok(Some(Alias {
                        name,
                        as_kw: Some(as_kw),
                    }))
                }
                _ => Err(self.err(MsgId::SELECT_ERROR_NAMEEXPECTED, t.span.start)),
            };
        }
        let start = self.tok().span.start;
        let is_word = |p: &Self| {
            matches!(p.tok().kind, TokKind::Word { kws, .. }
                if !reserved_prefix(kws)
                    && !p.dcs_stop_word(kws)
                    && !(if source { reserved_src_alias(kws) } else { reserved_alias(kws) }))
        };
        let dot_word =
            |p: &Self| p.at_punct(Punct::Dot) && matches!(p.nth(1).kind, TokKind::Word { .. });
        if !(is_word(self) || dot_word(self)) {
            return Ok(None);
        }
        let mut parts = 0;
        let mut end = start;
        if is_word(self) {
            end = self.bump()?.span.end;
            parts += 1;
        }
        while dot_word(self) {
            self.bump()?;
            end = self.bump()?.span.end;
            parts += 2;
        }
        let span = Span::new(start, end);
        if parts == 1 {
            let name = Ident {
                text: crate::resource::text(self.memory, span.text(self.text))?,
                span,
            };
            self.check_alias_hash(&name)?;
            return Ok(Some(Alias { name, as_kw: None }));
        }
        Err(self
            .err(MsgId::SELECT_ERROR_INVALIDALIAS, start)
            .with_quoted(crate::resource::arg(self.memory, span.text(self.text))))
    }
}
