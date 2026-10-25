use super::expr::{Clause, ExprCtx, bool_like, is_type_ref, unlimited_view};
use super::pos::{Frame, elem_at, root_at};
use super::scope::{Level, SourceCols};
use super::{Analyzer, R};
use crate::CompileError;
use crate::managed::{self as mem, CompileArena};
use crate::schema::{FieldRole, QTableId, StdRole, TableKind};
use crate::tir::{
    self, ControlPoint, OrderKey, TExpr, TKind, Totals, TotalsField, TotalsHierarchy, TotalsPeriods,
};
use crate::types::rules::PERIOD_UNITS;
use crate::types::{NumQ, Other, QType};
use query_syntax::ast::{self, BinOp, ExprKind, GroupKind, IndexKind, Lit, OrderDir};
use query_syntax::{Func, Kw, MsgId, QueryError, Span};
use std::mem::Discriminant;
mod keys;
type QueryOrder<'a> = (
    mem::NativeOwned<'a, Vec<OrderKey>>,
    mem::NativeOwned<'a, Option<Totals>>,
);
type TotalsItems<'a> = (
    mem::NativeOwned<'a, Vec<(TExpr, usize)>>,
    mem::NativeOwned<'a, Vec<(usize, Span)>>,
);
type TotalsPoints<'a> = (
    mem::NativeOwned<'a, Vec<ControlPoint>>,
    mem::Index<usize, ()>,
);

use keys::{field_key_name, key_name};

pub(crate) fn each_child<'a>(e: &'a TExpr, f: &mut dyn FnMut(&'a TExpr)) {
    match &e.kind {
        TKind::Const(_)
        | TKind::Value(_)
        | TKind::Param(_)
        | TKind::Field { .. }
        | TKind::Unit(_) => {}
        TKind::Neg(x) | TKind::Not(x) => f(x),
        TKind::Arith { first, steps } => {
            f(first);
            steps.iter().for_each(|s| f(&s.rhs));
        }
        TKind::And(v) | TKind::Or(v) => v.iter().for_each(f),
        TKind::Compare(_, a, b) => {
            f(a);
            f(b);
        }
        TKind::InList { l, rhs, .. } => {
            l.iter().for_each(&mut *f);
            rhs.iter().for_each(f);
        }
        TKind::InQuery { l, .. } => l.iter().for_each(f),
        TKind::Between { l, lo, hi, .. } => {
            f(l);
            f(lo);
            f(hi);
        }
        TKind::IsNull { l, .. } | TKind::Refs { l, .. } => f(l),
        TKind::Like { l, pattern, .. } => {
            f(l);
            f(pattern);
        }
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                f(o);
            }
            for (c, t) in whens {
                f(c);
                f(t);
            }
            if let Some(x) = else_ {
                f(x);
            }
        }
        TKind::Cast { e, .. } => f(e),
        TKind::Func { args, .. } => args.iter().for_each(f),
        TKind::Agg { arg, .. } => {
            if let Some(a) = arg {
                f(a);
            }
        }
    }
}

fn has_agg(e: &TExpr) -> bool {
    if matches!(e.kind, TKind::Agg { nested: true, .. }) {
        return false;
    }
    if matches!(e.kind, TKind::Agg { .. }) {
        return true;
    }
    let mut found = false;
    each_child(e, &mut |c| found = found || has_agg(c));
    found
}

fn agg_spans(memory: Option<&CompileArena>, e: &TExpr, out: &mut mem::ScratchVec<Span>) -> R<()> {
    if matches!(e.kind, TKind::Agg { nested: false, .. }) {
        return out.push(memory, e.span);
    }
    let mut result = Ok(());
    each_child(e, &mut |child| {
        if result.is_ok() {
            result = agg_spans(memory, child, out);
        }
    });
    result
}

fn has_field(e: &TExpr) -> bool {
    if matches!(e.kind, TKind::Field { .. }) {
        return true;
    }
    let mut found = false;
    each_child(e, &mut |c| found = found || has_field(c));
    found
}

fn first_call(e: &TExpr, f: Func) -> Option<Span> {
    if matches!(&e.kind, TKind::Func { f: g, .. } if *g == f) {
        return Some(e.span);
    }
    let mut found = None;
    each_child(e, &mut |child| {
        if found.is_none() {
            found = first_call(child, f);
        }
    });
    found
}

pub(crate) struct GroupCtx {
    keys: mem::Index<mem::NameKey, ()>,
    tags: mem::Index<Discriminant<TKind>, ()>,
}
impl GroupCtx {
    fn new(memory: Option<&CompileArena>, sets: &[Vec<TExpr>]) -> R<Self> {
        let mut group = Self {
            keys: mem::Index::new(),
            tags: mem::Index::new(),
        };
        for expression in sets.iter().flatten() {
            group
                .keys
                .insert(memory, key_name(memory, expression)?, ())?;
            if !matches!(expression.kind, TKind::Field { .. }) {
                group
                    .tags
                    .insert(memory, std::mem::discriminant(&expression.kind), ())?;
            }
        }
        Ok(group)
    }
}
pub(crate) fn grouping_of(
    memory: Option<&CompileArena>,
    items: &[tir::Item],
    group: Option<&[Vec<TExpr>]>,
    having: Option<&TExpr>,
) -> R<Option<GroupCtx>> {
    let grouped = group.is_some()
        || items.iter().any(|item| has_agg(&item.expr))
        || having.is_some_and(has_agg);
    grouped
        .then(|| GroupCtx::new(memory, group.unwrap_or(&[])))
        .transpose()
}

fn item_with_key(
    memory: Option<&CompileArena>,
    items: &[tir::Item],
    wanted: &str,
) -> R<Option<usize>> {
    for (index, item) in items.iter().enumerate() {
        if key_name(memory, &item.expr)?.as_str() == wanted {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn item_named(memory: Option<&CompileArena>, items: &[tir::Item], name: &str) -> R<Option<usize>> {
    let wanted = mem::NameKey::folded(memory, name)?;
    for (index, item) in items.iter().enumerate() {
        if mem::NameKey::folded(memory, &item.name)? == wanted {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn presentation(e: &TExpr) -> bool {
    matches!(
        &e.kind,
        TKind::Func {
            f: Func::Presentation,
            ..
        }
    )
}

fn int_constant(e: &ast::Expr) -> Option<Option<usize>> {
    match &e.kind {
        ExprKind::Lit(Lit::Number(n)) => Some(n.digits.parse::<usize>().ok()),
        ExprKind::Lit(_) => Some(None),
        ExprKind::Unary { expr, .. } => int_constant(expr).map(|_| None),
        ExprKind::Paren(x) => int_constant(x),
        _ => None,
    }
}

fn top_of(e: &ast::Expr) -> &ast::Expr {
    let mut e = e;
    while let ExprKind::Paren(x) = &e.kind {
        e = x;
    }
    e
}

fn is_agg_func(f: Func) -> bool {
    matches!(
        f,
        Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Count
    )
}

fn whole_hit(hits: &[(usize, Span)], e: &ast::Expr) -> Option<usize> {
    let mut e = e;
    while let ExprKind::Paren(x) = &e.kind {
        e = x;
    }
    match hits {
        [(c, sp)] if *sp == e.span => Some(*c),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Dcs,
    Order,
    OrderUnion,
    TotalsItem,
    TotalsPoint,
}

pub(crate) struct ColumnScope<'a> {
    kind: ScopeKind,
    cols: mem::ScratchVec<mem::NativeOwned<'a, TExpr>>,
    index: mem::Index<mem::NameKey, usize>,
    nested: mem::Index<mem::NameKey, (usize, mem::NativeOwned<'a, TExpr>)>,
    hits: mem::ScratchVec<(usize, Span)>,
    ordered: mem::ScratchVec<mem::NativeOwned<'a, TExpr>>,
}

fn same_field(a: &TExpr, b: &TExpr) -> bool {
    match (&a.kind, &b.kind) {
        (
            TKind::Field {
                level: l1,
                source: s1,
                col: c1,
                derefs: d1,
            },
            TKind::Field {
                level: l2,
                source: s2,
                col: c2,
                derefs: d2,
            },
        ) => {
            l1 == l2
                && s1 == s2
                && c1 == c2
                && d1.len() == d2.len()
                && d1.iter().zip(d2).all(|(x, y)| x.targets == y.targets)
        }
        _ => false,
    }
}

pub(crate) fn dcs_scope<'a>(
    memory: Option<&'a CompileArena>,
    items: &[tir::Item],
) -> R<ColumnScope<'a>> {
    ColumnScope::new(memory, ScopeKind::Dcs, items)
}

impl<'a> ColumnScope<'a> {
    fn new(memory: Option<&'a CompileArena>, kind: ScopeKind, items: &[tir::Item]) -> R<Self> {
        let mut index = mem::Index::new();
        let mut nested = mem::Index::new();
        let mut cols = mem::ScratchVec::new(memory, items.len())?;
        for (i, item) in items.iter().enumerate() {
            let key = mem::NameKey::folded(memory, &item.name)?;
            if index.get(key.as_str()).is_none() {
                index.insert(memory, key, i)?;
            }
            cols.push(memory, mem::owned_copy(memory, &item.expr)?)?;
            for item in item.nested.iter().flatten() {
                let key = mem::NameKey::folded(memory, &item.name)?;
                if nested.get(key.as_str()).is_none() {
                    nested.insert(memory, key, (i, mem::owned_copy(memory, &item.expr)?))?;
                }
            }
        }
        Ok(Self {
            kind,
            cols,
            index,
            nested,
            hits: mem::ScratchVec::new(memory, 0)?,
            ordered: mem::ScratchVec::new(memory, 0)?,
        })
    }
}

fn copy_hit<'a>(
    memory: Option<&'a CompileArena>,
    hits: &mut mem::ScratchVec<(usize, Span)>,
    column: &mem::NativeOwned<'_, TExpr>,
    index: usize,
    span: Span,
) -> R<mem::NativeOwned<'a, TExpr>> {
    let value = mem::owned_copy(memory, column.trusted_borrow())?;
    hits.push(memory, (index, span))?;
    Ok(value)
}

impl<'scope> Analyzer<'scope> {
    pub(crate) fn nested(&self) -> bool {
        self.frames.contains(&Frame::Subquery)
    }

    pub(crate) fn substituted_error<'a>(
        &self,
        msg: MsgId,
        at: u32,
        arguments: impl IntoIterator<Item = mem::Argument<'a>>,
    ) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.semantic_subst_error(msg, Some(self.map.at(self.text, at)), arguments);
        }
        let mut error = self.err(msg, at);
        for argument in arguments {
            error = error.with_subst(argument.legacy());
        }
        error
    }
    fn kw_pair(&self, a: Kw, b: Kw) -> mem::Argument<'static> {
        let script = self.schema.script();
        mem::Argument::Words(a.name(script), b.name(script))
    }

    fn not_allowed(&self, kw: Kw, at: u32) -> CompileError {
        self.quoted_argument_error(
            MsgId::SELECT_ERROR_OPERATIONNOTALLOWEDINCLAUSE,
            mem::Argument::Kw(kw),
            at,
        )
    }

    fn last_name<'a>(&'a self, e: &'a ast::Expr) -> &'a str {
        match &e.kind {
            ExprKind::Path(path) => path
                .parts
                .last()
                .map(|part| &*part.text)
                .unwrap_or_default(),
            _ => self.text_of(e.span),
        }
    }
    fn text_of(&self, span: Span) -> &str {
        self.text
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default()
    }

    pub(crate) fn column_hit_owned(
        &mut self,
        path: &ast::Path,
    ) -> R<Option<mem::ExprValue<'scope>>> {
        let span = path.span();
        let Some(scope) = self.columns.as_mut() else {
            return Ok(None);
        };
        if let [one] = path.parts.as_slice() {
            let key = mem::NameKey::folded(self.memory, &one.text)?;
            if let Some(&i) = scope.index.get(key.as_str())
                && let Some(column) = scope.cols.get(i)
            {
                let value = copy_hit(self.memory, &mut scope.hits, column, i, span)?;
                return Ok(Some(mem::ExprValue::copied(value).span(span)));
            }
            if scope.kind == ScopeKind::Order
                && let Some((i, column)) = scope.nested.get(key.as_str())
            {
                let value = copy_hit(self.memory, &mut scope.hits, column, *i, span)?;
                return Ok(Some(mem::ExprValue::copied(value).span(span)));
            }
        }
        if scope.kind == ScopeKind::Dcs
            && let [first, ..] = path.parts.as_slice()
            && path.parts.len() > 1
        {
            let key = mem::NameKey::folded(self.memory, &first.text)?;
            if let Some(&i) = scope.index.get(key.as_str())
                && let Some(column) = scope.cols.get(i)
            {
                let column =
                    mem::ExprValue::copied(mem::owned_copy(self.memory, column.trusted_borrow())?);
                let nested = self.nested_table(&column);
                let ty = mem::type_copy(self.memory, &column.ty)?.without_null();
                let (derefs, ty) = self.derefs_owned(ty, nested, &path.parts, 1)?;
                let value = column.extend_field(self.memory, derefs, ty, span)?;
                return Ok(Some(value));
            }
        }
        let Some(scope) = self.columns.as_ref() else {
            return Ok(None);
        };
        match scope.kind {
            ScopeKind::OrderUnion => {
                let saved = self.columns.take();
                let result = self.path_expr_owned(path);
                self.columns = saved;
                let field = match result {
                    Ok(field) => Some(field),
                    Err(error) if self.memory.is_some_and(|arena| arena.error().is_some()) => {
                        return Err(error);
                    }
                    Err(_) => {
                        if let Some(arena) = self.memory {
                            drop(arena.take_diagnostic());
                        }
                        None
                    }
                };
                let index = field.as_ref().and_then(|field| {
                    self.columns
                        .as_ref()?
                        .cols
                        .iter()
                        .position(|column| same_field(column.trusted_borrow(), field))
                });
                if let Some(index) = index
                    && let Some(scope) = self.columns.as_mut()
                    && let Some(column) = scope.cols.get(index)
                {
                    let value = copy_hit(self.memory, &mut scope.hits, column, index, span)?;
                    return Ok(Some(mem::ExprValue::copied(value).span(span)));
                }
                Err(self.substituted_error(
                    MsgId::SELECT_ERROR_WRONGORDERFIELDSWITHUNION,
                    span.start,
                    [
                        mem::Argument::Kw(Kw::Union),
                        mem::Argument::Kw(Kw::Order),
                        mem::Argument::Kw(Kw::Totals),
                    ],
                ))
            }
            ScopeKind::TotalsItem => {
                let saved = self.columns.take();
                let result = self.path_expr_owned(path);
                self.columns = saved;
                let field = result?;
                let index = self.columns.as_ref().and_then(|scope| {
                    scope
                        .cols
                        .iter()
                        .position(|column| same_field(column.trusted_borrow(), &field))
                });
                if let Some(index) = index
                    && let Some(scope) = self.columns.as_mut()
                    && let Some(column) = scope.cols.get(index)
                {
                    let value = copy_hit(self.memory, &mut scope.hits, column, index, span)?;
                    return Ok(Some(mem::ExprValue::copied(value).span(span)));
                }
                if self.columns.as_ref().is_some_and(|scope| {
                    scope
                        .ordered
                        .iter()
                        .any(|key| same_field(key.trusted_borrow(), &field))
                }) {
                    return Ok(Some(field));
                }
                Err(self.message_error(MsgId::SELECT_ERROR_FIELDFORTOTALMUSTBEINSELECT, span.start))
            }
            ScopeKind::Order | ScopeKind::TotalsPoint | ScopeKind::Dcs => Ok(None),
        }
    }

    fn hits_len(&self) -> usize {
        self.columns.as_ref().map_or(0, |c| c.hits.len())
    }

    fn hits_since(&self, n: usize) -> &[(usize, Span)] {
        self.columns
            .as_ref()
            .map(|scope| scope.hits.get(n..).unwrap_or_default())
            .unwrap_or_default()
    }

    pub(crate) fn condition(&mut self, c: &ast::Expr, clause: Clause, kw: Kw) -> R<TExpr> {
        if self.memory.is_some() {
            return crate::managed::allocation_error(
                self.memory,
                memory_budget::BudgetError::Overflow,
            );
        }
        self.condition_owned(c, clause, kw)
            .map(mem::NativeOwned::into_native)
    }

    pub(crate) fn condition_owned(
        &mut self,
        c: &ast::Expr,
        clause: Clause,
        kw: Kw,
    ) -> R<mem::NativeOwned<'scope, TExpr>> {
        let at = root_at(c);
        let mut ctx = ExprCtx::new(clause, at);
        if let ExprKind::Chain { rest, .. } = &c.kind
            && rest.iter().all(|o| o.op == BinOp::And)
            && matches!(clause, Clause::Where | Clause::Having)
        {
            ctx.and_root = Some(c.span);
        }
        let e = self.expr_value(c, ctx)?;
        if !bool_like(&e.ty) {
            return Err(self.not_allowed(kw, at));
        }
        e.into_native_owned()
    }

    pub(crate) fn group_by(
        &mut self,
        g: &ast::GroupBy,
    ) -> R<mem::NativeOwned<'scope, Vec<Vec<TExpr>>>> {
        let mut sets = mem::ScratchVec::new(self.memory, 0)?;
        match &g.kind {
            GroupKind::Exprs(v) => sets.push(self.memory, v.as_slice())?,
            GroupKind::Sets(s) => sets.extend(self.memory, s.iter().map(Vec::as_slice))?,
        }
        let mut out = mem::NativeVec::new(self.memory, sets.len())?;
        for set in &sets {
            let mut v = mem::NativeVec::new(self.memory, set.len())?;
            for e in *set {
                let t = self.expr_value(e, ExprCtx::new(Clause::GroupBy, root_at(e)))?;
                if has_agg(&t) {
                    return Err(self.err(MsgId::SELECT_ERROR_GROUPBYAGGREGATE, e.span.start));
                }
                if t.ty.has_unlimited_string() && self.stored_field(&t) {
                    return Err(self.quoted_text_error(
                        MsgId::SELECT_ERROR_WRONGGROUPFIELD,
                        self.last_name(e),
                        e.span.start,
                    ));
                }
                v.push_owned(t.into_native_owned()?)?;
            }
            out.push_owned(v.finish())?;
        }
        Ok(out.finish())
    }

    pub(crate) fn check_group(&self, e: &TExpr, g: &GroupCtx) -> R<()> {
        match &e.kind {
            TKind::Agg { .. }
            | TKind::Const(_)
            | TKind::Value(_)
            | TKind::Param(_)
            | TKind::Unit(_) => Ok(()),
            TKind::Field { level, .. } if *level > 0 => Ok(()),
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                for k in (0..=derefs.len()).rev() {
                    let key = field_key_name(
                        self.memory,
                        *level,
                        *source,
                        *col,
                        derefs.get(..k).unwrap_or_default(),
                    )?;
                    if g.keys.get(key.as_str()).is_some() {
                        return Ok(());
                    }
                }
                if *level == 0 && self.ref_grouped(*source, g)? {
                    return Ok(());
                }
                Err(self.quoted_text_error(
                    MsgId::SELECT_ERROR_NOTGROUPBY,
                    self.text_of(e.span),
                    e.span.start,
                ))
            }
            _ => {
                if g.tags.get(&std::mem::discriminant(&e.kind)).is_some() {
                    let key = key_name(self.memory, e)?;
                    if g.keys.get(key.as_str()).is_some() {
                        return Ok(());
                    }
                }
                let mut r = Ok(());
                each_child(e, &mut |c| {
                    if r.is_ok() {
                        r = self.check_group(c, g);
                    }
                });
                r
            }
        }
    }

    pub(crate) fn for_update_tables(
        &self,
        f: &ast::ForUpdate,
    ) -> R<mem::NativeOwned<'scope, Vec<QTableId>>> {
        let mut out = mem::NativeVec::new(self.memory, f.tables.len())?;
        for p in &f.tables {
            let id = self
                .schema
                .find_table_managed(self.memory, &p.parts)?
                .filter(|id| {
                    matches!(self.schema.table(*id).kind, TableKind::Object(_))
                        && self.levels.last().is_some_and(|l: &Level| {
                            l.sources.iter().any(
                            |s| matches!(&s.cols, SourceCols::Table { id: sid, .. } if sid == id),
                        )
                        })
                });
            match id {
                Some(id) => out.push_copy(id)?,
                None => {
                    return Err(self.substituted_error(
                        MsgId::SELECT_ERROR_FORUPDATEONNESTEDTABLE,
                        p.span().start,
                        [mem::Argument::Kw(Kw::For), mem::Argument::Kw(Kw::Update)],
                    ));
                }
            }
        }
        Ok(out.finish())
    }

    pub(crate) fn union_columns(&self, parts: &[tir::Select]) -> R<()> {
        let n = parts.first().map(|p| p.items.len());
        if parts.iter().any(|p| Some(p.items.len()) != n) {
            return Err(QueryError::new(
                MsgId::SELECT_ERROR_DIFFERENTFIELDSCOUNTINUNIONS,
                None,
            ));
        }
        Ok(())
    }

    pub(crate) fn autonumber_checks(&self, parts: &[tir::Select], into: bool) -> R<()> {
        let mut first = None;
        for p in parts {
            for it in &p.items {
                if matches!(
                    &it.expr.kind,
                    TKind::Func {
                        f: Func::RecordAutoNumber,
                        ..
                    }
                ) {
                    first = Some((it.expr.span, true));
                } else if let Some(span) = first_call(&it.expr, Func::RecordAutoNumber) {
                    first = Some((span, false));
                }
                if first.is_some() {
                    break;
                }
            }
            if first.is_some() {
                break;
            }
            for e in p
                .where_
                .iter()
                .chain(p.having.iter())
                .chain(p.group.iter().flatten().flatten())
            {
                if let Some(span) = first_call(e, Func::RecordAutoNumber) {
                    first = Some((span, false));
                    break;
                }
            }
            if first.is_some() {
                break;
            }
        }
        let Some((span, alone)) = first else {
            return Ok(());
        };
        let name = mem::Argument::Func(Func::RecordAutoNumber);
        let (msg, second) = if !into {
            (
                MsgId::SELECT_ERROR_RECORDAUTONUMBERCANBEUSEDINSELECTIONTOTEMPTABLE,
                None,
            )
        } else if parts.len() > 1 {
            (
                MsgId::SELECT_ERROR_RECORDAUTONUMBERCANTBEUSEDINUNION,
                Some(mem::Argument::Kw(Kw::Union)),
            )
        } else if !alone {
            (
                MsgId::SELECT_ERROR_RECORDAUTONUMBERCANTBEUSEDINEXPRESSION,
                None,
            )
        } else {
            return Ok(());
        };
        Err(self.substituted_error(msg, span.start, std::iter::once(name).chain(second)))
    }

    pub(crate) fn order_and_totals(
        &mut self,
        q: &ast::Query,
        parts: &[tir::Select],
        nested: bool,
    ) -> R<QueryOrder<'scope>> {
        let Some(first) = parts.first() else {
            return Ok((
                mem::NativeVec::new(self.memory, 0)?.finish(),
                mem::NativeOwned::none(self.memory),
            ));
        };
        let into = q.parts.first().is_some_and(|s| s.into.is_some());
        if let Some(a) = &q.autoorder {
            if nested {
                return Err(self.substituted_error(
                    MsgId::SELECT_ERROR_AUTOORDERBYINNESTEDSELECT,
                    a.start,
                    [mem::Argument::Kw(Kw::AutoOrder)],
                ));
            }
            if into {
                return Err(self.autoorder_into(a.start));
            }
            if first.distinct
                && parts.len() == 1
                && q.order_by.is_none()
                && first.items.iter().any(|i| !i.expr.ty.refs.is_empty())
                && let Some(src) = q
                    .parts
                    .first()
                    .and_then(|p| p.from.as_ref())
                    .and_then(|f| f.sources.first())
            {
                return Err(self.substituted_error(
                    MsgId::SELECT_ERROR_DISTINCTWITHORDERBY,
                    src.span.start,
                    [mem::Argument::Kw(Kw::Distinct)],
                ));
            }
        }
        let order = match &q.order_by {
            Some(o) => {
                if into && first.top.is_none() {
                    return Err(self.substituted_error(
                        MsgId::SELECT_ERROR_INTOORDERMUSTBEWITHTOP,
                        o.kw.start,
                        [mem::Argument::Kw(Kw::Order), mem::Argument::Kw(Kw::Top)],
                    ));
                }
                let kind = if parts.len() > 1 {
                    ScopeKind::OrderUnion
                } else {
                    ScopeKind::Order
                };
                self.columns = Some(ColumnScope::new(self.memory, kind, &first.items)?);
                let r = self.order_keys(o, parts);
                self.columns = None;
                r?
            }
            None => mem::NativeVec::new(self.memory, 0)?.finish(),
        };
        let totals = match &q.totals {
            Some(t) => {
                if nested {
                    return Err(self.substituted_error(
                        MsgId::SELECT_ERROR_CANTUSETOTALSINNESTEDSELECT,
                        t.kw.start,
                        [mem::Argument::Kw(Kw::Totals)],
                    ));
                }
                if into {
                    return Err(self.autoorder_into(t.kw.start));
                }
                if let Some(k) = order
                    .trusted_borrow()
                    .iter()
                    .zip(q.order_by.iter().flat_map(|o| o.items.iter()))
                    .find(|(k, _)| matches!(k.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc))
                    .map(|(_, it)| it)
                {
                    return Err(
                        self.err(MsgId::SELECT_ERROR_HIERSORTINGWITHTOTALS, k.expr.span.start)
                    );
                }
                self.totals(t, first, order.trusted_borrow())?.into_some()
            }
            None => mem::NativeOwned::none(self.memory),
        };
        Ok((order, totals))
    }

    fn autoorder_into(&self, at: u32) -> CompileError {
        self.substituted_error(
            MsgId::SELECT_ERROR_AUTOORDERCANTBEUSEDINTO,
            at,
            [
                mem::Argument::Kw(Kw::AutoOrder),
                mem::Argument::Kw(Kw::Totals),
            ],
        )
    }

    fn order_keys(
        &mut self,
        o: &ast::OrderBy,
        parts: &[tir::Select],
    ) -> R<mem::NativeOwned<'scope, Vec<OrderKey>>> {
        let Some(first) = parts.first() else {
            return Ok(mem::NativeVec::new(self.memory, 0)?.finish());
        };
        let group = if parts.len() == 1 {
            grouping_of(
                self.memory,
                &first.items,
                first.group.as_deref(),
                first.having.as_ref(),
            )?
        } else {
            None
        };
        let shown = (parts.len() == 1 && first.distinct)
            .then(|| self.shown_fields(first))
            .transpose()?;
        let mut out = mem::NativeVec::new(self.memory, o.items.len())?;
        for item in &o.items {
            let at = item.expr.span.start;
            let before = self.hits_len();
            let mut numbered = None;
            let expr = match int_constant(&item.expr) {
                Some(n) => {
                    let col = n.filter(|n| *n >= 1).and_then(|n| first.items.get(n - 1));
                    let Some(col) = col else {
                        return Err(self.constant_in_order(at));
                    };
                    if presentation(&col.expr) {
                        return Err(self.not_allowed(Kw::Order, col.expr.span.start));
                    }
                    let value = mem::owned_copy(self.memory, &col.expr)?;
                    numbered = n.map(|n| n - 1);
                    mem::ExprValue::copied(value).span(item.expr.span)
                }
                None => {
                    let ctx = ExprCtx::new(Clause::OrderBy, root_at(&item.expr));
                    self.expr_value(&item.expr, ctx)?
                }
            };
            let hits = self.hits_since(before);
            let col = numbered.or_else(|| whole_hit(hits, &item.expr));
            for (col, _) in hits.iter().copied() {
                if let Some(c) = first.items.get(col)
                    && presentation(&c.expr)
                {
                    return Err(self.not_allowed(Kw::Order, c.expr.span.start));
                }
            }
            if expr.ty.has_unlimited_string() && self.stored_field(&expr) {
                return Err(self.quoted_text_error(
                    MsgId::SELECT_ERROR_WRONGORDERFIELD,
                    self.last_name(&item.expr),
                    at,
                ));
            }
            if let Some(shown) = &shown
                && hidden_field(self.memory, &expr, shown)?
            {
                return Err(self.substituted_error(
                    MsgId::SELECT_ERROR_DISTINCTWITHORDERBY,
                    at,
                    [mem::Argument::Kw(Kw::Distinct)],
                ));
            }
            if let Some(g) = &group {
                self.check_group(&expr, g)?;
            }
            out.push_owned(expr.into_native_owned()?.map(|expr| OrderKey {
                expr,
                col,
                dir: item.dir,
            }))?;
        }
        Ok(out.finish())
    }

    fn constant_in_order(&self, at: u32) -> CompileError {
        self.substituted_error(
            MsgId::SELECT_ERROR_CONSTANTINODERBY,
            at,
            [
                self.kw_pair(Kw::Order, Kw::By),
                self.kw_pair(Kw::Index, Kw::By),
            ],
        )
    }

    fn totals(
        &mut self,
        t: &ast::Totals,
        first: &tir::Select,
        order: &[tir::OrderKey],
    ) -> R<mem::NativeOwned<'scope, Totals>> {
        self.columns = Some(ColumnScope::new(
            self.memory,
            ScopeKind::TotalsPoint,
            &first.items,
        )?);
        let points = self.control_points(t);
        let (points, point_cols) = match points {
            Ok(p) => p,
            Err(e) => {
                self.columns = None;
                return Err(e);
            }
        };
        let group = grouping_of(
            self.memory,
            &first.items,
            first.group.as_deref(),
            first.having.as_ref(),
        );
        let checked = group.and_then(|g| {
            let Some(g) = g else { return Ok(()) };
            for p in points.trusted_borrow() {
                if let ControlPoint::Field(f) = p
                    && f.col.is_none()
                {
                    self.check_group(&f.expr, &g)?;
                }
            }
            Ok(())
        });
        if let Err(e) = checked {
            self.columns = None;
            return Err(e);
        }
        if let Some(s) = self.columns.as_mut() {
            s.kind = ScopeKind::TotalsItem;
            for key in order.iter().filter(|key| key.col.is_none()) {
                if let Err(e) = mem::owned_copy(self.memory, &key.expr)
                    .and_then(|key| s.ordered.push(self.memory, key))
                {
                    self.columns = None;
                    return Err(e);
                }
            }
        }
        let items = self.totals_items(t, first, &point_cols);
        self.columns = None;
        let (items, hits) = items?;
        let items = mem::allocation_result(self.memory, items.zip(hits))?;
        Ok(mem::allocation_result(self.memory, items.zip(points))?.map(
            |((items, hits), points)| Totals {
                items,
                hits,
                points,
            },
        ))
    }

    fn control_points(&mut self, t: &ast::Totals) -> R<TotalsPoints<'scope>> {
        let mut points = mem::NativeVec::new(self.memory, t.points.len())?;
        let mut cols = mem::Index::new();
        for p in &t.points {
            match p {
                ast::ControlPoint::Overall(_) => points.push_owned(
                    mem::NativeOwned::inline(self.memory, ()).map(|()| ControlPoint::Overall),
                )?,
                ast::ControlPoint::Field {
                    expr,
                    hierarchy,
                    periods,
                    alias,
                    ..
                } => {
                    let before = self.hits_len();
                    let ctx = ExprCtx::new(Clause::Totals, root_at(expr));
                    let e = self.expr_value(expr, ctx)?;
                    let hits = self.hits_since(before);
                    let col = whole_hit(hits, expr);
                    for (column, _) in hits {
                        cols.insert(self.memory, *column, ())?;
                    }
                    let periods = match periods {
                        Some(p) => self.totals_periods(p)?.into_some(),
                        None => mem::NativeOwned::none(self.memory),
                    };
                    let alias = match alias {
                        Some(a) => mem::OwnedString::new(self.memory, &a.name.text)?
                            .into_native_owned()
                            .into_some(),
                        None => mem::NativeOwned::none(self.memory),
                    };
                    let field = mem::allocation_result(
                        self.memory,
                        e.into_native_owned()?
                            .zip(periods)
                            .and_then(|v| v.zip(alias)),
                    )?
                    .map(|((expr, periods), alias)| TotalsField {
                        expr,
                        col,
                        hierarchy: match hierarchy {
                            None => TotalsHierarchy::No,
                            Some(ast::Hierarchy::All(_)) => TotalsHierarchy::All,
                            Some(ast::Hierarchy::Only(_)) => TotalsHierarchy::Only,
                        },
                        periods,
                        alias,
                    });
                    let field = mem::allocation_result(self.memory, field.boxed())?;
                    points.push_owned(field.map(ControlPoint::Field))?;
                }
            }
        }
        Ok((points.finish(), cols))
    }

    fn totals_periods(&mut self, p: &ast::Periods) -> R<mem::NativeOwned<'scope, TotalsPeriods>> {
        let wrong = |s: &Self, at: u32| {
            s.quoted_argument_error(
                MsgId::SELECT_ERROR_WRONGPARAMETER,
                mem::Argument::Kw(Kw::Periods),
                at,
            )
        };
        let (set, _) = query_syntax::kw::lookup(&p.unit.text);
        let unit = PERIOD_UNITS.into_iter().find(|k| set.contains(*k));
        let Some(unit) = unit else {
            return Err(wrong(self, p.unit.span.start));
        };
        let date =
            |s: &mut Self, e: &Option<ast::Expr>| -> R<mem::NativeOwned<'scope, Option<TExpr>>> {
                let Some(e) = e else {
                    return Ok(mem::NativeOwned::none(s.memory));
                };
                let x = s.expr_value(e, ExprCtx::new(Clause::Totals, root_at(e)))?;
                if !(x.ty.only_date() || x.ty.atom_count() == 0) {
                    return Err(wrong(s, root_at(e)));
                }
                Ok(x.into_native_owned()?.into_some())
            };
        let begin = date(self, &p.begin)?;
        let end = date(self, &p.end)?;
        Ok(
            mem::allocation_result(self.memory, begin.zip(end))?
                .map(|(begin, end)| TotalsPeriods { unit, begin, end }),
        )
    }

    fn totals_items(
        &mut self,
        t: &ast::Totals,
        first: &tir::Select,
        point_cols: &mem::Index<usize, ()>,
    ) -> R<TotalsItems<'scope>> {
        let mut out = mem::NativeVec::new(self.memory, t.items.len())?;
        let mut all_hits = mem::NativeVec::new(self.memory, 0)?;
        let mut seen = mem::Index::<usize, mem::NameKey>::new();
        for ti in &t.items {
            let at = ti.expr.span.start;
            let before = self.hits_len();
            let e = match self.expr_value(&ti.expr, ExprCtx::new(Clause::Totals, root_at(&ti.expr)))
            {
                Ok(e) => e,
                Err(err) if err.msg == MsgId::SELECT_ERROR_FIELDFORTOTALMUSTBEINSELECT => {
                    return Err(self.totals_unknown_result(ti)?.unwrap_or(err));
                }
                Err(err) => return Err(err),
            };
            let hits = self.hits_since(before);
            for h in hits {
                all_hits.push_copy(*h)?;
            }
            let mut aggs = mem::ScratchVec::new(self.memory, 0)?;
            agg_spans(self.memory, &e, &mut aggs)?;
            let inside = |sp: &Span| {
                aggs.iter()
                    .filter(|a| !hits.iter().any(|(_, hit)| hit == *a))
                    .any(|a| a.start <= sp.start && sp.end <= a.end)
            };
            let outside = hits.iter().filter(|(_, sp)| !inside(sp)).count();
            let k = key_name(self.memory, &e)?;
            let col = match &ti.alias {
                Some(a) => {
                    let key = mem::NameKey::folded(self.memory, &a.name.text)?;
                    self.columns
                        .as_ref()
                        .and_then(|scope| scope.index.get(key.as_str()).copied())
                }
                None => {
                    let one = hits
                        .first()
                        .map(|(column, _)| *column)
                        .filter(|column| hits.iter().all(|(hit, _)| hit == column));
                    let same = item_with_key(self.memory, &first.items, k.as_str())?;
                    let top_agg = matches!(top_of(&ti.expr).kind, ExprKind::Func { func, .. } if is_agg_func(func));
                    match (whole_hit(hits, &ti.expr), outside) {
                        (Some(c), _) => Some(c),
                        (None, 0) if !hits.is_empty() && top_agg => one,
                        _ if top_agg && hits.is_empty() => match &e.kind {
                            TKind::Agg { arg: Some(a), .. } => {
                                let ak = key_name(self.memory, a)?;
                                item_with_key(self.memory, &first.items, ak.as_str())?
                            }
                            _ => None,
                        },
                        _ if matches!(e.kind, TKind::Const(_)) => same,
                        _ => None,
                    }
                }
            };
            let Some(col) = col else {
                return Err(self.message_error(
                    MsgId::SELECT_ERROR_TOTALUNKNOWNRESULTFIELD,
                    elem_at(&ti.expr),
                ));
            };
            for (c, sp) in hits {
                if !inside(sp) && point_cols.get(c).is_none() {
                    return Err(self.err(MsgId::SELECT_ERROR_FIELDMUSTBEINTOTALTOCALC, sp.start));
                }
            }
            match seen.get(&col) {
                None => {
                    seen.insert(self.memory, col, k)?;
                }
                Some(prev) if *prev == k => {}
                Some(_) => {
                    let item_at = first.items.get(col).map_or(at, |i| i.expr.span.start);
                    return Err(self.err(MsgId::SELECT_ERROR_DUPLICATEDTOTALS, item_at));
                }
            }
            out.push_owned(e.into_native_owned()?.map(|e| (e, col)))?;
        }
        Ok((out.finish(), all_hits.finish()))
    }

    fn totals_unknown_result(&self, ti: &ast::TotalsItem) -> R<Option<CompileError>> {
        let unknown = match &ti.alias {
            Some(a) => {
                let key = mem::NameKey::folded(self.memory, &a.name.text)?;
                self.columns
                    .as_ref()
                    .is_none_or(|c| c.index.get(key.as_str()).is_none())
            }
            None => {
                !matches!(top_of(&ti.expr).kind, ExprKind::Func { func, .. } if is_agg_func(func))
            }
        };
        Ok(unknown.then(|| {
            self.message_error(
                MsgId::SELECT_ERROR_TOTALUNKNOWNRESULTFIELD,
                elem_at(&ti.expr),
            )
        }))
    }

    pub(crate) fn index_sets(
        &mut self,
        ix: &ast::IndexBy,
        parts: &[tir::Select],
        types: &[mem::ScopeType],
    ) -> R<mem::NativeOwned<'scope, Vec<Vec<usize>>>> {
        let mut sets = mem::ScratchVec::new(self.memory, 0)?;
        match &ix.kind {
            IndexKind::Fields { fields, .. } => sets.push(self.memory, fields.as_slice())?,
            IndexKind::Sets(s) => {
                sets.extend(self.memory, s.iter().map(|set| set.fields.as_slice()))?;
            }
        }
        let items = parts
            .first()
            .map(|p| p.items.as_slice())
            .unwrap_or_default();
        let mut out = mem::NativeVec::new(self.memory, sets.len())?;
        for &set in &sets {
            let mut cols = mem::NativeVec::new(self.memory, set.len())?;
            for e in set {
                let col = match int_constant(e) {
                    Some(n) => match n.filter(|n| *n >= 1).filter(|n| *n <= items.len()) {
                        Some(n) => n - 1,
                        None => return Err(self.constant_in_order(e.span.start)),
                    },
                    None => {
                        let by_name = match &e.kind {
                            ExprKind::Path(p) => match p.parts.as_slice() {
                                [one] => item_named(self.memory, items, &one.text)?,
                                _ => None,
                            },
                            _ => None,
                        };
                        let found = if by_name.is_some() {
                            by_name
                        } else {
                            let ctx = ExprCtx::new(Clause::OrderBy, root_at(e));
                            match self.expr_value(e, ctx) {
                                Ok(expression) => {
                                    let wanted = key_name(self.memory, &expression)?;
                                    item_with_key(self.memory, items, wanted.as_str())?
                                }
                                Err(error) => {
                                    if self.memory.is_some_and(|memory| memory.error().is_some()) {
                                        return Err(error);
                                    }
                                    if let Some(memory) = self.memory {
                                        drop(memory.take_diagnostic());
                                    }
                                    None
                                }
                            }
                        };
                        let Some(c) = found else {
                            return Err(self.message_error(
                                MsgId::SELECT_ERROR_INDEXEDFIELDSMUSTBEINSELECT,
                                e.span.start,
                            ));
                        };
                        c
                    }
                };
                if let Some(t) = types.get(col)
                    && !items.get(col).is_some_and(|i| i.unknown)
                    && (t.is_composite_beyond_refs()
                        || (t.atom_count() == 0
                            && items.get(col).is_some_and(|i| {
                                matches!(i.expr.kind, TKind::Const(crate::QValue::Null))
                            }))
                        || t.has_unlimited_string()
                        || t.others.contains(&Other::Storage))
                {
                    let at = items.get(col).map_or(e.span.start, |i| i.expr.span.start);
                    return Err(self.err(MsgId::SELECT_ERROR_INDEXFIELDSCANTBEDOMAINS, at));
                }
                cols.push_copy(col)?;
            }
            out.push_owned(cols.finish())?;
        }
        Ok(out.finish())
    }
}

struct Shown {
    keys: mem::Index<mem::NameKey, ()>,
    refs: mem::Index<u16, ()>,
}

impl Shown {
    fn new<'a>(
        memory: Option<&CompileArena>,
        expressions: impl IntoIterator<Item = &'a TExpr>,
        references: impl IntoIterator<Item = u16>,
    ) -> R<Self> {
        let mut keys = mem::Index::new();
        for expression in expressions {
            keys.insert(memory, key_name(memory, expression)?, ())?;
        }
        let mut refs = mem::Index::new();
        for source in references {
            refs.insert(memory, source, ())?;
        }
        Ok(Self { keys, refs })
    }
}

impl<'scope> Analyzer<'scope> {
    fn ref_grouped(&self, source: u16, group: &GroupCtx) -> R<bool> {
        let Some(SourceCols::Table { id, .. }) = self
            .levels
            .last()
            .and_then(|level| level.sources.get(usize::from(source)))
            .map(|source| &source.cols)
        else {
            return Ok(false);
        };
        for (index, field) in self.schema.table(*id).fields.iter().enumerate() {
            if field.role == FieldRole::Std(StdRole::Ref)
                && let Ok(index) = u16::try_from(index)
            {
                let key = field_key_name(
                    self.memory,
                    0,
                    source,
                    tir::Col::Field(crate::QFieldId(index)),
                    &[],
                )?;
                if group.keys.get(key.as_str()).is_some() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn comparable_view<'a>(&'a self, e: &'a TExpr) -> R<crate::managed::TypeView<'a>> {
        use crate::managed::{self as mem, TypeView};
        if is_type_ref(e) {
            return Ok(TypeView::Owned(mem::type_copy(
                self.memory,
                &unlimited_view(),
            )?));
        }
        if e.ty.has_unlimited_string() && !self.stored_field(e) {
            Ok(TypeView::Owned(
                mem::type_copy(self.memory, &e.ty)?.with_string(crate::types::StrQ {
                    len: 1,
                    fixed: false,
                }),
            ))
        } else {
            Ok(TypeView::Borrowed(&e.ty))
        }
    }

    fn stored_field(&self, e: &TExpr) -> bool {
        let TKind::Field {
            level,
            source,
            col,
            derefs,
        } = &e.kind
        else {
            return false;
        };
        let computed = |t, f| {
            self.schema.field(t, f).is_none_or(|f| {
                matches!(
                    f.role,
                    FieldRole::Virtual | FieldRole::Std(StdRole::Presentation)
                )
            })
        };
        if let Some(last) = derefs.last() {
            return last.targets.iter().any(|(t, f)| !computed(*t, *f));
        }
        let tir::Col::Field(f) = col else {
            return false;
        };
        let depth = usize::from(*level);
        let Some(lv) = self
            .levels
            .len()
            .checked_sub(1 + depth)
            .and_then(|i| self.levels.get(i))
        else {
            return true;
        };
        match lv.sources.get(usize::from(*source)).map(|s| &s.cols) {
            Some(SourceCols::Table { id, .. }) => !computed(*id, *f),
            _ => false,
        }
    }

    fn shown_fields(&self, first: &tir::Select) -> R<Shown> {
        let level = self.levels.last();
        let refs = first.items.iter().filter_map(|i| match &i.expr.kind {
            TKind::Field {
                level: 0,
                source,
                col: tir::Col::Field(f),
                derefs,
            } if derefs.is_empty() => {
                let src = level?.sources.get(usize::from(*source))?;
                let SourceCols::Table { id, .. } = &src.cols else {
                    return None;
                };
                let field = self.schema.field(*id, *f)?;
                (field.role == FieldRole::Std(StdRole::Ref)).then_some(*source)
            }
            _ => None,
        });
        Shown::new(self.memory, first.items.iter().map(|item| &item.expr), refs)
    }
}

fn hidden_field(memory: Option<&CompileArena>, e: &TExpr, shown: &Shown) -> R<bool> {
    let key = key_name(memory, e)?;
    if shown.keys.get(key.as_str()).is_some() {
        return Ok(false);
    }
    match &e.kind {
        TKind::Field { level, .. } if *level > 0 => Ok(false),
        TKind::Field {
            source,
            col,
            derefs,
            ..
        } => {
            if shown.refs.get(source).is_some() {
                return Ok(false);
            }
            for n in 0..derefs.len() {
                let key = field_key_name(
                    memory,
                    0,
                    *source,
                    *col,
                    derefs.get(..n).unwrap_or_default(),
                )?;
                if shown.keys.get(key.as_str()).is_some() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        TKind::Agg { .. } => Ok(false),
        _ => {
            let mut hidden = Ok(false);
            each_child(e, &mut |child| {
                if matches!(hidden, Ok(false)) {
                    hidden = hidden_field(memory, child, shown);
                }
            });
            hidden
        }
    }
}

pub(crate) fn column_types(q: &tir::Query) -> Vec<QType> {
    column_types_of(&q.parts, q.totals.as_ref())
}

pub(crate) fn column_types_budgeted(
    q: &tir::Query,
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::BudgetedVec<memory_budget::Charged<QType>>, memory_budget::BudgetError> {
    let capacity = q.parts.first().map_or(0, |p| p.items.len());
    let mut types = memory_budget::BudgetedVec::try_with_capacity(
        budget,
        memory_budget::Category::Execution,
        capacity,
    )?;
    let Some((first, rest)) = q.parts.split_first() else {
        return Ok(types);
    };
    for (i, item) in first.items.iter().enumerate() {
        let mut ty = item.expr.ty.try_clone_budgeted(budget)?;
        for other in rest.iter().filter_map(|p| p.items.get(i)) {
            ty = ty.try_merge_budgeted(&other.expr.ty, budget)?;
        }
        types.try_push(ty)?;
    }
    if let Some(totals) = &q.totals {
        let count = QType::num(NumQ {
            digits: 0,
            scale: 0,
            nonneg: true,
        });
        for (expr, column) in &totals.items {
            let Some(slot) = types.get_mut(*column) else {
                continue;
            };
            match &expr.kind {
                TKind::Agg { f: Func::Count, .. } => {
                    *slot = slot.try_merge_budgeted(&count, budget)?;
                }
                TKind::Field { .. } => {}
                TKind::Agg {
                    f: Func::Min | Func::Max,
                    arg: Some(arg),
                    ..
                } if has_field(arg) => {}
                _ if expr.ty.atom_count() == 0 => {}
                _ => *slot = expr.ty.try_clone_budgeted(budget)?,
            }
        }
    }
    Ok(types)
}

pub(crate) fn column_types_scoped(
    memory: Option<&CompileArena>,
    parts: &[tir::Select],
    totals: Option<&Totals>,
) -> R<mem::ScratchVec<mem::ScopeType>> {
    let Some((first, rest)) = parts.split_first() else {
        return mem::ScratchVec::new(memory, 0);
    };
    let mut types = mem::ScratchVec::new(memory, first.items.len())?;
    for (index, item) in first.items.iter().enumerate() {
        let mut ty = mem::ScopeType::copy(memory, &item.expr.ty)?;
        for item in rest.iter().filter_map(|part| part.items.get(index)) {
            ty = ty.merge(memory, &item.expr.ty)?;
        }
        types.push(memory, ty)?;
    }
    if let Some(totals) = totals {
        let count = QType::num(NumQ {
            digits: 0,
            scale: 0,
            nonneg: true,
        });
        for (expr, column) in &totals.items {
            let Some(slot) = types.get_mut(*column) else {
                continue;
            };
            match &expr.kind {
                TKind::Agg { f: Func::Count, .. } => *slot = slot.merge(memory, &count)?,
                TKind::Field { .. } => {}
                TKind::Agg {
                    f: Func::Min | Func::Max,
                    arg: Some(arg),
                    ..
                } if has_field(arg) => {}
                _ if expr.ty.atom_count() == 0 => {}
                _ => *slot = mem::ScopeType::copy(memory, &expr.ty)?,
            }
        }
    }
    Ok(types)
}

pub(crate) fn column_types_of(parts: &[tir::Select], totals: Option<&tir::Totals>) -> Vec<QType> {
    let Some((first, rest)) = parts.split_first() else {
        return Vec::new();
    };
    let mut types: Vec<QType> = first
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            rest.iter()
                .filter_map(|p| p.items.get(i))
                .fold(it.expr.ty.clone(), |t, x| t.merge(&x.expr.ty))
        })
        .collect();
    if let Some(t) = totals {
        let count = QType::num(NumQ {
            digits: 0,
            scale: 0,
            nonneg: true,
        });
        for (e, col) in &t.items {
            let Some(slot) = types.get_mut(*col) else {
                continue;
            };
            match &e.kind {
                TKind::Agg { f: Func::Count, .. } => *slot = slot.merge(&count),
                TKind::Field { .. } => {}
                TKind::Agg {
                    f: Func::Min | Func::Max,
                    arg: Some(a),
                    ..
                } if has_field(a) => {}
                _ if e.ty.atom_count() == 0 => {}
                _ => *slot = e.ty.clone(),
            }
        }
    }
    types
}
