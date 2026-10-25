use std::collections::HashMap;

use super::{Builder, R, SortMeta, internal, later};
use crate::ir::{ColSel, Expr, ExprKind, SrcKind};
use crate::schema::QTableId;
use crate::tir::{self, ControlPoint, Query, TExpr, TKind, TotalsHierarchy};
use crate::totals::{
    AggArg, AggFunc, AggSpec, ArithOp, HierS, HierarchyMode, KeySource, OrderKey, PeriodsS, PointS,
    TotalsExpr, TotalsS,
};
use query_syntax::ast::BinOp;
use query_syntax::{Func, ScriptVariant, Span};

fn hit_spans(t: &tir::Totals) -> HashMap<(u32, u32), usize> {
    t.hits
        .iter()
        .map(|(c, sp)| ((sp.start, sp.end), *c))
        .collect()
}

fn span_key(s: Span) -> (u32, u32) {
    (s.start, s.end)
}

fn kids(e: &TExpr) -> Vec<&TExpr> {
    match &e.kind {
        TKind::Const(_)
        | TKind::Value(_)
        | TKind::Param(_)
        | TKind::Field { .. }
        | TKind::Unit(_) => Vec::new(),
        TKind::Neg(x) | TKind::Not(x) => vec![x],
        TKind::Arith { first, steps } => std::iter::once(first.as_ref())
            .chain(steps.iter().map(|s| &s.rhs))
            .collect(),
        TKind::And(v) | TKind::Or(v) => v.iter().collect(),
        TKind::Compare(_, l, r) => vec![l, r],
        TKind::InList { l, rhs, .. } => l.iter().chain(rhs.iter()).collect(),
        TKind::InQuery { l, .. } => l.iter().collect(),
        TKind::Between { l, lo, hi, .. } => vec![l, lo, hi],
        TKind::IsNull { l, .. } | TKind::Refs { l, .. } => vec![l],
        TKind::Like { l, pattern, .. } => vec![l, pattern],
        TKind::Case {
            operand,
            whens,
            else_,
        } => operand
            .iter()
            .map(|x| x.as_ref())
            .chain(whens.iter().flat_map(|(a, b)| [a, b]))
            .chain(else_.iter().map(|x| x.as_ref()))
            .collect(),
        TKind::Cast { e, .. } => vec![e],
        TKind::Func { args, .. } => args.iter().collect(),
        TKind::Agg { arg, .. } => arg.iter().map(|x| x.as_ref()).collect(),
    }
}

fn has_field(e: &TExpr) -> bool {
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        if matches!(x.kind, TKind::Field { .. } | TKind::InQuery { .. }) {
            return true;
        }
        stack.extend(kids(x));
    }
    false
}

fn agg_args<'a>(e: &'a TExpr, hits: &HashMap<(u32, u32), usize>, out: &mut Vec<&'a TExpr>) {
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        if let TKind::Agg { arg, .. } = &x.kind {
            if let Some(a) = arg
                && !hits.contains_key(&span_key(a.span))
                && has_field(a)
            {
                out.push(a);
            }
            continue;
        }
        stack.extend(kids(x));
    }
}

fn agg_func(f: Func) -> Option<AggFunc> {
    Some(match f {
        Func::Sum => AggFunc::Sum,
        Func::Avg => AggFunc::Avg,
        Func::Count => AggFunc::Count,
        Func::Min => AggFunc::Min,
        Func::Max => AggFunc::Max,
        _ => return None,
    })
}

fn arith_op(op: BinOp) -> R<ArithOp> {
    Ok(match op {
        BinOp::Add => ArithOp::Add,
        BinOp::Sub => ArithOp::Sub,
        BinOp::Mul => ArithOp::Mul,
        BinOp::Div => ArithOp::Div,
        _ => return internal("a logical operator in an arithmetic chain"),
    })
}

fn deref_key(key: &Expr, base: &Expr) -> Option<(Vec<(QTableId, crate::schema::QFieldId)>, bool)> {
    let (inner, not) = match &key.kind {
        ExprKind::Not(x) => (x.as_ref(), true),
        _ => (key, false),
    };
    match &inner.kind {
        ExprKind::Deref { base: b, steps } if b.as_ref() == base && steps.len() == 1 => {
            steps.first().map(|d| (d.targets.clone(), not))
        }
        _ => None,
    }
}

impl<'a> Builder<'a> {
    pub(super) fn totals_prepare(&mut self, q: &'a Query, t: &'a tir::Totals) -> R<()> {
        let hits = hit_spans(t);
        let mut extra: Vec<&'a TExpr> = Vec::new();
        for p in &t.points {
            if let ControlPoint::Field(f) = p
                && f.col.is_none()
            {
                extra.push(&f.expr);
            }
        }
        for (e, _) in &t.items {
            agg_args(e, &hits, &mut extra);
        }
        if q.parts.len() > 1 && !extra.is_empty() {
            return later("ИТОГИ по выражению вне выборки объединения", "Q4");
        }
        self.totals_extra = extra;
        self.totals_extra_cols.clear();
        self.totals_extra_built.clear();
        self.root_sort.clear();
        self.totals_open = true;
        Ok(())
    }

    fn extra_col(&self, e: &TExpr) -> R<usize> {
        let at = self
            .totals_extra
            .iter()
            .position(|x| std::ptr::eq(*x, e))
            .and_then(|i| self.totals_extra_cols.get(i));
        match at {
            Some(c) => Ok(usize::from(*c)),
            None => internal("a hidden column of the totals was not built"),
        }
    }

    fn extra_built(&self, e: &TExpr) -> Option<&Expr> {
        self.totals_extra
            .iter()
            .position(|x| std::ptr::eq(*x, e))
            .and_then(|i| self.totals_extra_built.get(i))
    }

    fn hidden_point_name(&self, e: &TExpr) -> String {
        if let TKind::Field { derefs, .. } = &e.kind
            && let Some(d) = derefs.last()
        {
            return d.name.clone();
        }
        if let Some(Expr {
            kind:
                ExprKind::Col {
                    src,
                    col: ColSel::Field(f),
                },
            ..
        }) = self.extra_built(e)
            && let Some(SrcKind::Table(t)) = self.sources.get(src.0 as usize).map(|s| &s.kind)
            && let Some(field) = self.schema.table(*t).fields.get(usize::from(f.0))
        {
            return match self.schema.script() {
                ScriptVariant::Russian => field.name_ru.clone(),
                ScriptVariant::English => field.name_en.clone(),
            };
        }
        String::new()
    }

    pub(super) fn totals_finish(
        &mut self,
        q: &'a Query,
        t: &'a tir::Totals,
        _types: &[crate::types::QType],
    ) -> R<TotalsS> {
        let Some(first) = q.parts.first() else {
            return internal("a query without parts");
        };
        let width = first.items.len();
        let hits = hit_spans(t);
        let mut points: Vec<PointS> = Vec::with_capacity(t.points.len());
        let mut point_exprs: Vec<Option<&'a TExpr>> = Vec::with_capacity(t.points.len());
        let mut seen: HashMap<String, usize> = HashMap::new();
        for p in &t.points {
            let ControlPoint::Field(f) = p else {
                points.push(PointS {
                    name: "ОБЩИЕ".to_string(),
                    column: None,
                    hierarchy: HierarchyMode::No,
                    periods: None,
                    hier: Vec::new(),
                });
                point_exprs.push(None);
                continue;
            };
            let column = match f.col {
                Some(c) => {
                    if u16::try_from(c).is_ok_and(|c| self.present.contains(&c)) {
                        return later("ИТОГИ по колонке представления", "Q4");
                    }
                    c
                }
                None => self.extra_col(&f.expr)?,
            };
            let base = match (&f.alias, f.col) {
                (Some(a), _) => a.clone(),
                (None, Some(c)) => self.col_names.get(c).cloned().unwrap_or_default(),
                (None, None) => self.hidden_point_name(&f.expr),
            };
            let n = seen.entry(bsl_prim::fold_name(&base)).or_insert(0);
            let name = if *n == 0 {
                base.clone()
            } else {
                format!("{base}{n}")
            };
            *n += 1;
            let source = match f.col {
                Some(c) => first.items.get(c).map(|i| &i.expr),
                None => Some(&f.expr),
            };
            let is_field = source.is_some_and(|e| matches!(e.kind, TKind::Field { .. }));
            let hierarchy = match f.hierarchy {
                _ if !is_field => HierarchyMode::No,
                TotalsHierarchy::No => HierarchyMode::No,
                TotalsHierarchy::All => HierarchyMode::All,
                TotalsHierarchy::Only => HierarchyMode::Only,
            };
            let mut hier: Vec<HierS> = Vec::new();
            if hierarchy != HierarchyMode::No {
                for atom in &f.expr.ty.refs {
                    if let Some(table) = self.schema.hierarchical_table(*atom)
                        && !hier.iter().any(|h| h.table == table)
                    {
                        self.touch(table);
                        hier.push(HierS {
                            table,
                            atom: *atom,
                            keys: Vec::new(),
                        });
                    }
                }
            }
            let periods = match &f.periods {
                Some(per) => Some(PeriodsS {
                    unit: per.unit,
                    begin: match &per.begin {
                        Some(e) => Some(self.expr(e)?),
                        None => None,
                    },
                    end: match &per.end {
                        Some(e) => Some(self.expr(e)?),
                        None => None,
                    },
                }),
                None => None,
            };
            points.push(PointS {
                name,
                column: Some(column),
                hierarchy,
                periods,
                hier,
            });
            point_exprs.push(Some(&f.expr));
        }
        let mut aggs: Vec<AggSpec> = Vec::new();
        let mut items = Vec::with_capacity(t.items.len());
        for (e, col) in &t.items {
            let x = self.totals_expr(e, &hits, &mut aggs)?;
            items.push((x, *col));
        }
        let order = self.totals_order(q, &mut points, &point_exprs, width)?;
        Ok(TotalsS {
            width,
            points,
            aggs,
            items,
            order,
        })
    }

    fn totals_order(
        &mut self,
        q: &'a Query,
        points: &mut [PointS],
        point_exprs: &[Option<&'a TExpr>],
        width: usize,
    ) -> R<Vec<OrderKey>> {
        if q.parts.len() > 1 {
            let mut keys = Vec::with_capacity(q.order.len());
            for k in &q.order {
                let Some(col) = k.col else {
                    return internal("an order key of a union outside the select list");
                };
                keys.push(OrderKey {
                    source: KeySource::Row(col),
                    desc: matches!(k.dir, query_syntax::ast::OrderDir::Desc),
                });
            }
            return Ok(keys);
        }
        let metas: Vec<SortMeta> = std::mem::take(&mut self.root_sort);
        for p in points.iter_mut() {
            for h in &mut p.hier {
                h.keys = vec![None; metas.len()];
            }
        }
        let mut keys = Vec::with_capacity(metas.len());
        for m in &metas {
            if m.col < width {
                keys.push(OrderKey {
                    source: KeySource::Row(m.col),
                    desc: m.desc,
                });
                continue;
            }
            let point = points
                .iter()
                .enumerate()
                .position(|(i, p)| match (m.base_col, p.column) {
                    (Some(bc), Some(pc)) if bc == pc && pc < width => true,
                    _ => point_exprs
                        .get(i)
                        .copied()
                        .flatten()
                        .and_then(|e| self.extra_built(e))
                        .is_some_and(|b| *b == m.base),
                });
            let source = match point {
                Some(p) if m.key == m.base => KeySource::Point {
                    column: m.col,
                    point: p,
                },
                Some(p) => {
                    if let Some((targets, not)) = deref_key(&m.key, &m.base)
                        && let Some(pt) = points.get_mut(p)
                    {
                        let k = keys.len();
                        for h in &mut pt.hier {
                            if let Some((_, f)) = targets.iter().find(|(t, _)| *t == h.table)
                                && let Some(slot) = h.keys.get_mut(k)
                            {
                                *slot = Some((*f, not));
                            }
                        }
                    }
                    KeySource::FirstRow {
                        column: m.col,
                        point: p,
                    }
                }
                None => KeySource::Null,
            };
            keys.push(OrderKey {
                source,
                desc: m.desc,
            });
        }
        Ok(keys)
    }

    fn totals_expr(
        &mut self,
        e: &'a TExpr,
        hits: &HashMap<(u32, u32), usize>,
        aggs: &mut Vec<AggSpec>,
    ) -> R<TotalsExpr> {
        if let Some(c) = hits.get(&span_key(e.span)) {
            return Ok(TotalsExpr::Column(*c));
        }
        Ok(match &e.kind {
            TKind::Agg {
                f,
                distinct,
                arg,
                nested,
            } => {
                if *nested {
                    return later("ИТОГИ: агрегат по вложенной таблице", "Q4");
                }
                let Some(func) = agg_func(*f) else {
                    return later("ИТОГИ: агрегатная функция", "Q4");
                };
                let arg = match arg {
                    None => AggArg::Star,
                    Some(a) => match hits.get(&span_key(a.span)) {
                        Some(c) => {
                            if matches!(func, AggFunc::Min | AggFunc::Max)
                                && u16::try_from(*c).is_ok_and(|c| self.present.contains(&c))
                            {
                                return later("ИТОГИ: МИНИМУМ/МАКСИМУМ представления", "Q4");
                            }
                            AggArg::Column(*c)
                        }
                        None if has_field(a) => AggArg::Column(self.extra_col(a)?),
                        None => match &a.kind {
                            TKind::Const(v) => AggArg::Const(v.clone()),
                            _ => return later("ИТОГИ: агрегат выражения без полей", "Q4"),
                        },
                    },
                };
                aggs.push(AggSpec {
                    f: func,
                    distinct: *distinct,
                    arg,
                });
                TotalsExpr::Agg(aggs.len() - 1)
            }
            TKind::Const(v) => TotalsExpr::Const(v.clone()),
            TKind::Param(name) => TotalsExpr::Param(self.param(name)?),
            TKind::Arith { first, steps } => {
                let mut acc = self.totals_expr(first, hits, aggs)?;
                for s in steps {
                    let r = self.totals_expr(&s.rhs, hits, aggs)?;
                    acc = TotalsExpr::Bin(arith_op(s.op)?, Box::new(acc), Box::new(r));
                }
                acc
            }
            TKind::Neg(x) => TotalsExpr::Neg(Box::new(self.totals_expr(x, hits, aggs)?)),
            TKind::Not(x) => TotalsExpr::Not(Box::new(self.totals_expr(x, hits, aggs)?)),
            TKind::And(v) => {
                let mut out = Vec::with_capacity(v.len());
                for x in v {
                    out.push(self.totals_expr(x, hits, aggs)?);
                }
                TotalsExpr::And(out)
            }
            TKind::Or(v) => {
                let mut out = Vec::with_capacity(v.len());
                for x in v {
                    out.push(self.totals_expr(x, hits, aggs)?);
                }
                TotalsExpr::Or(out)
            }
            TKind::Compare(op, l, r) => TotalsExpr::Cmp(
                *op,
                Box::new(self.totals_expr(l, hits, aggs)?),
                Box::new(self.totals_expr(r, hits, aggs)?),
            ),
            TKind::Case {
                operand: None,
                whens,
                else_,
            } => {
                let mut out = Vec::with_capacity(whens.len());
                for (c, v) in whens {
                    out.push((
                        self.totals_expr(c, hits, aggs)?,
                        self.totals_expr(v, hits, aggs)?,
                    ));
                }
                let otherwise = match else_ {
                    Some(x) => Some(Box::new(self.totals_expr(x, hits, aggs)?)),
                    None => None,
                };
                TotalsExpr::Case {
                    whens: out,
                    otherwise,
                }
            }
            TKind::Func {
                f: Func::IsNull,
                args,
            } if args.len() == 2 => {
                let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
                    return internal("ЕСТЬNULL without two arguments");
                };
                TotalsExpr::IsNull(
                    Box::new(self.totals_expr(a, hits, aggs)?),
                    Box::new(self.totals_expr(b, hits, aggs)?),
                )
            }
            _ => return later("ИТОГИ: выражение итога", "Q4"),
        })
    }
}
