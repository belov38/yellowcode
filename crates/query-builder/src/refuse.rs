use crate::model::{Operator, Schema, SelectQuery, Source, Stmt, Table};
use bsl_prim::fold_name;
use query_ir::{CompiledStmt, tir};
use query_syntax::ast::{self, ItemKind};
use query_syntax::{Kw, MsgArg, MsgId, QueryError, SourceMap, Span};
use std::collections::HashSet;
use std::sync::Arc;

fn error(text: &str, msg: MsgId, offset: u32) -> QueryError {
    QueryError::new(msg, Some(SourceMap::new(text).at(text, offset)))
}

pub(crate) fn syntax(b: &ast::Batch, text: &str, strict_dcs: bool) -> Result<(), QueryError> {
    for st in &b.stmts {
        let ast::Stmt::Query(q) = st else { continue };
        if strict_dcs && let Some(t) = &q.query.totals {
            return Err(error(text, MsgId::SELECT_ERROR_SYNTAXERROR, t.kw.start));
        }
        query(&q.query, text)?;
    }
    Ok(())
}

fn query(q: &ast::Query, text: &str) -> Result<(), QueryError> {
    for p in &q.parts {
        let Some(from) = &p.from else { continue };
        if p.into.is_some()
            && from
                .sources
                .iter()
                .any(|s| s.joins.is_empty() && is_param(s))
        {
            let at = [
                p.group_by.as_ref().map(|g| g.kw),
                p.having.as_ref().map(|h| h.kw),
                p.for_update.as_ref().map(|f| f.kw),
            ]
            .into_iter()
            .flatten()
            .map(|s: Span| s.start)
            .min();
            if let Some(at) = at {
                return Err(
                    error(text, MsgId::SELECT_ERROR_WRONGWORDINTOWITHDATASOURCE, at)
                        .with_subst(MsgArg::Kw(Kw::Group))
                        .with_subst(MsgArg::Kw(Kw::Having))
                        .with_subst(MsgArg::Kw(Kw::For))
                        .with_subst(MsgArg::Kw(Kw::Update)),
                );
            }
        }
        for s in &from.sources {
            source(s, text)?;
        }
    }
    Ok(())
}

fn is_param(s: &ast::Source) -> bool {
    matches!(s.table, ast::TableRef::Param(_))
}

fn source(s: &ast::Source, text: &str) -> Result<(), QueryError> {
    if let ast::TableRef::Subquery { query: q, .. } = &s.table {
        query(q, text)?;
    }
    for j in &s.joins {
        if let ast::TableRef::Param(p) = &j.right.table {
            return Err(error(
                text,
                MsgId::SELECT_ERROR_TABLENAMEEXPECTED,
                p.span.start,
            ));
        }
        source(&j.right, text)?;
    }
    Ok(())
}

pub(crate) fn typed(
    schema: &Schema,
    compiled: &[Arc<CompiledStmt>],
    text: &str,
) -> Result<(), QueryError> {
    for (st, c) in schema.batch.iter().zip(compiled) {
        if let (Stmt::Select(q), tir::StmtKind::Query(tq)) = (st, &c.typed.kind) {
            let mut cx = Cx {
                text,
                scopes: Vec::new(),
                collect: true,
                known: HashSet::new(),
                in_order: false,
                in_item: false,
            };
            cx.select_query(q, tq)?;
            cx.collect = false;
            cx.select_query(q, tq)?;
        }
    }
    Ok(())
}

struct Cx<'a, 't> {
    text: &'a str,
    scopes: Vec<Vec<&'t tir::Source>>,
    collect: bool,
    known: HashSet<(usize, String)>,
    in_order: bool,
    in_item: bool,
}

type R = Result<(), QueryError>;

fn flat<'t>(ts: &'t [tir::Source], out: &mut Vec<&'t tir::Source>) {
    for t in ts {
        out.push(t);
        for j in &t.joins {
            flat(std::slice::from_ref(&j.right), out);
        }
    }
}

fn is_open(s: &tir::Source) -> bool {
    match &s.from {
        tir::From::Param { schema, .. } => schema.open,
        tir::From::Temp(t) => t.open,
        _ => false,
    }
}

impl<'a, 't> Cx<'a, 't> {
    fn err(&self, msg: MsgId, at: u32) -> QueryError {
        error(self.text, msg, at)
    }

    fn select_query(&mut self, q: &SelectQuery, tq: &'t tir::Query) -> R {
        if let Some(first) = tq.parts.first()
            && first.distinct
            && !self.collect
        {
            for k in &tq.order {
                if let (tir::TKind::Param(name), None) = (&k.expr.kind, k.col)
                    && !first
                        .items
                        .iter()
                        .any(|i| matches!(&i.expr.kind, tir::TKind::Param(n) if n == name))
                {
                    return Err(self
                        .err(MsgId::SELECT_ERROR_DISTINCTWITHORDERBY, k.expr.span.start)
                        .with_subst(MsgArg::Kw(Kw::Distinct)));
                }
            }
        }
        for (op, sel) in q.operators.iter().zip(&tq.parts) {
            self.operator(op, sel)?;
        }
        if let Some(sel) = tq.parts.first() {
            let mut sources = Vec::new();
            flat(&sel.sources, &mut sources);
            self.scopes.push(sources);
            self.in_order = true;
            let r = tq
                .order
                .iter()
                .filter(|k| k.col.is_none())
                .try_for_each(|k| self.expr(&k.expr));
            self.scopes.pop();
            self.in_order = false;
            r?;
        }
        Ok(())
    }

    fn operator(&mut self, op: &Operator, sel: &'t tir::Select) -> R {
        let mut sources = Vec::new();
        flat(&sel.sources, &mut sources);
        for f in &op.fields {
            if !self.collect
                && let ItemKind::PathStar(p) = &f.kind
                && let Some(first) = p.parts.first()
                && sources
                    .iter()
                    .any(|s| is_open(s) && fold_name(&s.alias) == fold_name(&first.text))
            {
                return Err(self.err(MsgId::SELECT_ERROR_TABLENOTFOUND, f.span.start));
            }
        }
        for f in &op.fields {
            if !self.collect
                && let ItemKind::Nested { table, .. } = &f.kind
                && let ast::ExprKind::Path(p) = &table.kind
                && p.parts.len() >= 2
                && let Some(first) = p.parts.first()
                && sources
                    .iter()
                    .any(|s| is_open(s) && fold_name(&s.alias) == fold_name(&first.text))
                && let Some(written) = self
                    .text
                    .get(table.span.start as usize..table.span.end as usize)
            {
                return Err(self
                    .err(MsgId::SELECT_ERROR_TABLENOTFOUND, table.span.start)
                    .with_quoted(MsgArg::text(written)));
            }
        }
        self.scopes.push(sources);
        let r = self.select(op, sel);
        self.scopes.pop();
        r
    }

    fn select(&mut self, op: &Operator, sel: &'t tir::Select) -> R {
        for it in &sel.items {
            if it.nested.is_none() {
                self.in_item = true;
                let r = self.expr(&it.expr);
                self.in_item = false;
                r?;
            }
        }
        if let Some(e) = &sel.where_ {
            self.expr(e)?;
        }
        for set in sel.group.iter().flatten() {
            for e in set {
                self.expr(e)?;
            }
        }
        if let Some(e) = &sel.having {
            self.expr(e)?;
        }
        self.sources(&op.sources, &sel.sources)
    }

    fn sources(&mut self, ms: &[Source], ts: &'t [tir::Source]) -> R {
        for (m, t) in ms.iter().zip(ts) {
            if let (Table::Subquery(q), tir::From::Subquery(tq)) = (&m.table, &t.from) {
                let saved = std::mem::take(&mut self.scopes);
                let r = self.select_query(q, tq);
                self.scopes = saved;
                r?;
            }
            for (mj, tj) in m.joins.iter().zip(&t.joins) {
                self.expr(&tj.on)?;
                self.sources(
                    std::slice::from_ref(&mj.right),
                    std::slice::from_ref(&tj.right),
                )?;
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &'t tir::TExpr) -> R {
        use tir::TKind as K;
        match &e.kind {
            K::Field {
                level,
                source,
                derefs,
                ..
            } => self.field(e.span, *level, *source, derefs),
            K::Not(a) => self.expr(a),
            K::Neg(a) => self.expr(a),
            K::Arith { first, steps } => {
                self.expr(first)?;
                steps.iter().try_for_each(|s| self.expr(&s.rhs))
            }
            K::And(v) | K::Or(v) => v.iter().try_for_each(|x| self.expr(x)),
            K::Compare(_, a, b) => {
                self.expr(a)?;
                self.expr(b)
            }
            K::InList { l, rhs, .. } => {
                l.iter().try_for_each(|x| self.expr(x))?;
                rhs.iter().try_for_each(|x| self.expr(x))
            }
            K::InQuery { l, q, .. } => {
                l.iter().try_for_each(|x| self.expr(x))?;
                for sel in &q.parts {
                    let mut sources = Vec::new();
                    flat(&sel.sources, &mut sources);
                    self.scopes.push(sources);
                    let r = self.nested_select(sel);
                    self.scopes.pop();
                    r?;
                }
                Ok(())
            }
            K::Between { l, lo, hi, .. } => {
                self.expr(l)?;
                self.expr(lo)?;
                self.expr(hi)
            }
            K::IsNull { l, .. } | K::Refs { l, .. } => self.expr(l),
            K::Like { l, pattern, .. } => {
                self.expr(l)?;
                self.expr(pattern)
            }
            K::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(o) = operand {
                    self.expr(o)?;
                }
                for (w, t) in whens {
                    self.expr(w)?;
                    self.expr(t)?;
                }
                match else_ {
                    Some(x) => self.expr(x),
                    None => Ok(()),
                }
            }
            K::Cast { e, .. } => self.expr(e),
            K::Func { args, .. } => args.iter().try_for_each(|x| self.expr(x)),
            K::Agg { arg, .. } => match arg {
                Some(x) => self.expr(x),
                None => Ok(()),
            },
            K::Const(_) | K::Value(_) | K::Param(_) | K::Unit(_) => Ok(()),
        }
    }

    fn nested_select(&mut self, sel: &'t tir::Select) -> R {
        for it in &sel.items {
            if it.nested.is_none() {
                self.expr(&it.expr)?;
            }
        }
        for e in [&sel.where_, &sel.having].into_iter().flatten() {
            self.expr(e)?;
        }
        for set in sel.group.iter().flatten() {
            for e in set {
                self.expr(e)?;
            }
        }
        Ok(())
    }

    fn field(&mut self, span: Span, level: u16, source: u16, derefs: &[tir::Deref]) -> R {
        let Some(scope) = self
            .scopes
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.scopes.get(i))
        else {
            return Ok(());
        };
        let Some(src) = scope.get(usize::from(source)) else {
            return Ok(());
        };
        let Some(written) = self.text.get(span.start as usize..span.end as usize) else {
            return Ok(());
        };
        if !self.collect {
            let mut off = 0usize;
            for (i, part) in written.split('.').enumerate() {
                let name = part.trim();
                if i > 0 && matches!(fold_name(name).as_str(), "поместить" | "into") {
                    let lead = part.len() - part.trim_start().len();
                    return Err(self
                        .err(
                            MsgId::SELECT_ERROR_TOKENEXPECTED,
                            span.start + (off + lead) as u32,
                        )
                        .with_quoted(MsgArg::text("(")));
                }
                off += part.len() + 1;
            }
        }
        let key = *src as *const tir::Source as usize;
        if is_open(src) {
            if self.collect {
                if let Some(name) = written.split('.').nth(1) {
                    self.known.insert((key, fold_name(name)));
                }
                return Ok(());
            }
            if is_name(written)
                && !((self.in_order || self.in_item)
                    && self.known.contains(&(key, fold_name(written))))
            {
                return Err(self
                    .err(MsgId::SELECT_ERROR_FIELDNOTFOUND, span.start)
                    .with_quoted(MsgArg::text(written)));
            }
            return Ok(());
        }
        if !self.collect
            && let Some(k) = derefs.iter().position(|d| d.targets.is_empty())
        {
            let parts = written.split('.').count();
            let skip = parts.saturating_sub(derefs.len() - k);
            let at = written
                .split('.')
                .take(skip)
                .map(|p| p.len() as u32 + 1)
                .sum::<u32>();
            return Err(self
                .err(MsgId::SELECT_ERROR_FIELDNOTFOUND, span.start + at)
                .with_quoted(MsgArg::text(written)));
        }
        Ok(())
    }
}

fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && cs.all(|c| c.is_alphanumeric() || c == '_')
}
