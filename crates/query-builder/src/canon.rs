use crate::model::{JoinCond, Operator, Source, Table};
use crate::pretty::prec::{Prec, chain_operand, prec};
use crate::to_ast::{SP, and_chain};
use bsl_prim::fold_name;
use query_syntax::ast::{
    self, BinOp, ChainOp, CmpOp, ControlPoint, Expr, ExprKind, FuncArgs, GroupKind, InRhs,
    ItemKind, UnOp,
};
use query_syntax::kw::{Func, Kw};
use std::collections::HashSet;

fn paren(e: Expr) -> Expr {
    Expr {
        kind: ExprKind::Paren(Box::new(e)),
        span: SP,
    }
}

fn cast_paren(e: Expr) -> Expr {
    match &e.kind {
        ExprKind::Cast { deref, .. } if deref.is_empty() => paren(e),
        _ => e,
    }
}

fn at(e: Expr, min: Prec) -> Expr {
    if prec(&e) < min { paren(e) } else { e }
}

fn left(e: Expr) -> Expr {
    cast_paren(if prec(&e) == Prec::Pred {
        paren(e)
    } else {
        at(e, Prec::Cmp)
    })
}

pub(crate) fn vt_arg(mut a: ast::VtArg) -> ast::VtArg {
    if !matches!(a.expr.kind, ExprKind::Dcs(_)) {
        a.expr = expr(&a.expr);
    }
    if let ExprKind::Dcs(first) = &mut a.expr.kind {
        first.unresolved_tail = None;
        for g in a.dcs.drain(..) {
            first.items.extend(g.items);
        }
    } else if a.dcs.len() > 1 {
        let mut rest = a.dcs.drain(1..).collect::<Vec<_>>();
        if let Some(first) = a.dcs.first_mut() {
            for g in rest.drain(..) {
                first.items.extend(g.items);
            }
        }
    }
    a
}

fn boxed(e: &Expr) -> Box<Expr> {
    Box::new(expr(e))
}

pub(crate) fn expr(e: &Expr) -> Expr {
    let kind = match &e.kind {
        ExprKind::Paren(inner) => return expr(inner),
        ExprKind::Chain { first, rest } if rest.is_empty() => return expr(first),
        ExprKind::Chain { first, rest } => return chain(first, rest),
        ExprKind::Unary {
            op: UnOp::Not,
            expr: x,
            ..
        } => return not(x),
        ExprKind::Unary {
            op,
            op_span,
            expr: x,
        } => ExprKind::Unary {
            op: *op,
            op_span: *op_span,
            expr: Box::new(cast_paren(at(expr(x), Prec::Unary))),
        },
        ExprKind::Compare { op, op_span, l, r } => ExprKind::Compare {
            op: *op,
            op_span: *op_span,
            l: Box::new(left(expr(l))),
            r: Box::new(cast_paren(at(expr(r), Prec::Add))),
        },
        ExprKind::In {
            l,
            not,
            hierarchy,
            kw,
            rhs,
        } => {
            let inner = Expr {
                kind: ExprKind::In {
                    l: Box::new(left(expr(l))),
                    not: false,
                    hierarchy: *hierarchy,
                    kw: *kw,
                    rhs: match rhs {
                        InRhs::List(es) => InRhs::List(es.iter().map(expr).collect()),
                        InRhs::Query(q) => InRhs::Query(Box::new(crate::to_ast::embedded(q))),
                    },
                },
                span: e.span,
            };
            if !*not {
                return inner;
            }
            ExprKind::Unary {
                op: UnOp::Not,
                op_span: SP,
                expr: Box::new(inner),
            }
        }
        ExprKind::Between { l, not, kw, lo, hi } => ExprKind::Between {
            l: Box::new(left(expr(l))),
            not: *not,
            kw: *kw,
            lo: Box::new(cast_paren(at(expr(lo), Prec::Add))),
            hi: Box::new(cast_paren(at(expr(hi), Prec::Add))),
        },
        ExprKind::IsNull { l, not, kw } => ExprKind::IsNull {
            l: Box::new(left(expr(l))),
            not: *not,
            kw: *kw,
        },
        ExprKind::Refs { l, kw, table } => ExprKind::Refs {
            l: Box::new({
                let l = expr(l);
                if prec(&l) == Prec::Pred {
                    paren(l)
                } else {
                    at(l, Prec::Cmp)
                }
            }),
            kw: *kw,
            table: table.clone(),
        },
        ExprKind::Like {
            l,
            not,
            kw,
            pattern,
            escape,
        } => ExprKind::Like {
            l: Box::new(left(expr(l))),
            not: *not,
            kw: *kw,
            pattern: Box::new(cast_paren(at(expr(pattern), Prec::Add))),
            escape: escape.clone(),
        },
        ExprKind::Tuple(es) => ExprKind::Tuple(es.iter().map(expr).collect()),
        ExprKind::Case {
            kw,
            operand,
            whens,
            else_,
        } => ExprKind::Case {
            kw: *kw,
            operand: operand.as_deref().map(boxed),
            whens: whens
                .iter()
                .map(|w| ast::When {
                    kw: w.kw,
                    cond: expr(&w.cond),
                    then: expr(&w.then),
                })
                .collect(),
            else_: else_.as_deref().map(boxed),
        },
        ExprKind::Deref { expr: x, deref } => ExprKind::Deref {
            expr: Box::new(paren(expr(x))),
            deref: deref.clone(),
        },
        ExprKind::Cast {
            kw,
            expr: x,
            ty,
            deref,
        } => ExprKind::Cast {
            kw: *kw,
            expr: boxed(x),
            ty: ty.clone(),
            deref: deref.clone(),
        },
        ExprKind::Func {
            func,
            name_span,
            distinct,
            args,
        } => ExprKind::Func {
            func: *func,
            name_span: *name_span,
            distinct: *distinct,
            args: match args {
                FuncArgs::Star(s) => FuncArgs::Star(*s),
                FuncArgs::List(es) => {
                    let unit = unit_pos(*func);
                    FuncArgs::List(
                        es.iter()
                            .enumerate()
                            .map(|(i, x)| {
                                if unit == Some(i) {
                                    unit_in_capitals(expr(x))
                                } else {
                                    expr(x)
                                }
                            })
                            .collect(),
                    )
                }
            },
        },
        ExprKind::DateTime { kw, args } => ExprKind::DateTime {
            kw: *kw,
            args: args.iter().map(expr).collect(),
        },
        ExprKind::TypeOf { kw, name } => ExprKind::TypeOf {
            kw: *kw,
            name: primitive_in_capitals(name),
        },
        ExprKind::Lit(_)
        | ExprKind::Param(_)
        | ExprKind::Path(_)
        | ExprKind::Value { .. }
        | ExprKind::Dcs(_) => return e.clone(),
    };
    Expr { kind, span: e.span }
}

fn unit_pos(f: Func) -> Option<usize> {
    match f {
        Func::BeginOfPeriod | Func::EndOfPeriod | Func::DateAdd => Some(1),
        Func::DateDiff => Some(2),
        _ => None,
    }
}

fn keyword_of(w: &str, kws: &[Kw]) -> Option<&'static str> {
    let w = w.to_uppercase();
    kws.iter().find_map(|k| {
        if k.ru().to_uppercase() == w {
            Some(k.ru())
        } else if k.en().to_uppercase() == w {
            Some(k.en())
        } else {
            None
        }
    })
}

fn unit_in_capitals(mut e: Expr) -> Expr {
    const UNITS: [Kw; 10] = [
        Kw::Year,
        Kw::Quarter,
        Kw::Month,
        Kw::Week,
        Kw::Day,
        Kw::Hour,
        Kw::Minute,
        Kw::Second,
        Kw::TenDays,
        Kw::HalfYear,
    ];
    if let ExprKind::Path(p) = &mut e.kind
        && let [part] = p.parts.as_mut_slice()
        && let Some(k) = keyword_of(&part.text, &UNITS)
    {
        part.text = k.into();
    }
    e
}

fn primitive_in_capitals(p: &ast::Path) -> ast::Path {
    let mut p = p.clone();
    if let [part] = p.parts.as_mut_slice()
        && let Some(k) = keyword_of(&part.text, &[Kw::String, Kw::Number, Kw::Date, Kw::Boolean])
    {
        part.text = k.into();
    }
    p
}

fn chain(first: &Expr, rest: &[ChainOp]) -> Expr {
    let first = expr(first);
    let mut ops: Vec<ChainOp> = rest
        .iter()
        .map(|o| ChainOp {
            op: o.op,
            op_span: o.op_span,
            expr: expr(&o.expr),
        })
        .collect();
    let level = ops.first().map_or(Prec::Primary, |o| prec_of_op(o.op));
    let mergeable = matches!(&first.kind, ExprKind::Chain { rest: r2, .. }
        if r2.first().is_some_and(|o| prec_of_op(o.op) == level));
    let first = match first.kind {
        ExprKind::Chain {
            first: f2,
            rest: mut r2,
        } if mergeable => {
            r2.append(&mut ops);
            ops = r2;
            *f2
        }
        kind => Expr {
            kind,
            span: first.span,
        },
    };
    let min = chain_operand(level);
    let arith = matches!(level, Prec::Add | Prec::Mul);
    let operand = |e: Expr| {
        let e = at(e, min);
        if arith { cast_paren(e) } else { e }
    };
    Expr {
        kind: ExprKind::Chain {
            first: Box::new(operand(first)),
            rest: ops
                .into_iter()
                .map(|o| ChainOp {
                    op: o.op,
                    op_span: o.op_span,
                    expr: operand(o.expr),
                })
                .collect(),
        },
        span: SP,
    }
}

fn prec_of_op(op: BinOp) -> Prec {
    match op {
        BinOp::Or => Prec::Or,
        BinOp::And => Prec::And,
        BinOp::Add | BinOp::Sub => Prec::Add,
        BinOp::Mul | BinOp::Div => Prec::Mul,
    }
}

fn not(x: &Expr) -> Expr {
    let mut bare = x;
    let mut bracketed = false;
    while let ExprKind::Paren(inner) = &bare.kind {
        bare = inner;
        bracketed = true;
    }
    let inner = expr(bare);
    let operand = if prec(&inner) >= Prec::Unary {
        inner
    } else if bracketed {
        paren(inner)
    } else {
        at(inner, Prec::Not)
    };
    Expr {
        kind: ExprKind::Unary {
            op: UnOp::Not,
            op_span: SP,
            expr: Box::new(operand),
        },
        span: SP,
    }
}

fn element(e: &Expr) -> Expr {
    let c = expr(e);
    if prec(&c) == Prec::Or { paren(c) } else { c }
}

pub(crate) fn alias_of(s: &Source) -> Vec<String> {
    let implicit = match &s.table {
        Table::Named { path, .. } => path.parts.last().map(|p| p.text.to_string()),
        Table::Subquery(_) | Table::Param(_) => None,
    };
    s.alias
        .as_ref()
        .map(|a| a.name.text.to_string())
        .or(implicit)
        .map(|a| fold_name(&a))
        .into_iter()
        .collect()
}

fn leads(e: &Expr, aliases: &[String]) -> bool {
    matches!(&e.kind, ExprKind::Path(p)
        if p.parts.first().is_some_and(|f| aliases.contains(&fold_name(&f.text))))
}

fn is_link(e: &Expr, left_side: &[String], right_side: &[String]) -> bool {
    matches!(&e.kind, ExprKind::Compare { op, l, r, .. }
        if matches!(op, CmpOp::Eq | CmpOp::Ne | CmpOp::Lt | CmpOp::Gt | CmpOp::Le | CmpOp::Ge)
            && leads(l, left_side)
            && leads(r, right_side))
}

pub(crate) fn join_condition(on: &[JoinCond], left_side: &[String], right_side: &[String]) -> Expr {
    let conjuncts: Vec<Expr> = on
        .iter()
        .map(|c| {
            let e = expr(&c.expr);
            if !c.api && is_link(&e, left_side, right_side) {
                e
            } else {
                paren(e)
            }
        })
        .collect();
    and_chain(&conjuncts).unwrap_or_else(|| {
        paren(Expr {
            kind: ExprKind::Lit(ast::Lit::Bool(true)),
            span: SP,
        })
    })
}

fn items(items: &mut [ast::SelectItem]) {
    for it in items {
        match &mut it.kind {
            ItemKind::Expr(e) => *e = expr(e),
            ItemKind::Nested { fields, .. } => items_vec(fields),
            ItemKind::Star | ItemKind::PathStar(_) | ItemKind::EmptyTable { .. } => {}
        }
    }
}

fn items_vec(v: &mut Vec<ast::SelectItem>) {
    items(v.as_mut_slice());
}

pub(crate) fn select(sel: &mut ast::Select, op: &Operator) {
    items(&mut sel.items);
    let list = |cs: &[Expr]| and_chain(&cs.iter().map(element).collect::<Vec<_>>());
    sel.where_ = list(&op.filter).map(|expr| ast::Clause { kw: SP, expr });
    sel.having = list(&op.having).map(|expr| ast::Clause { kw: SP, expr });
    if let Some(g) = &mut sel.group_by {
        match &mut g.kind {
            GroupKind::Exprs(es) => {
                es.iter_mut().for_each(|e| *e = expr(e));
                es.retain(|e| !matches!(e.kind, ExprKind::Param(_)));
                if es.is_empty() {
                    sel.group_by = None;
                }
            }
            GroupKind::Sets(sets) => sets.iter_mut().flatten().for_each(|e| *e = expr(e)),
        }
    }
}

pub(crate) fn dcs(blocks: &[ast::DcsBlock]) -> Vec<ast::DcsBlock> {
    let mut out: Vec<ast::DcsBlock> = Vec::with_capacity(blocks.len());
    for b in blocks {
        let list = match &b.kind {
            ast::DcsKind::Select(l) | ast::DcsKind::Where(l) => Some(l),
            _ => None,
        };
        let same =
            |o: &ast::DcsBlock| std::mem::discriminant(&o.kind) == std::mem::discriminant(&b.kind);
        match (list, out.iter_mut().find(|o| same(o))) {
            (Some(l), Some(prev)) => {
                if let ast::DcsKind::Select(p) | ast::DcsKind::Where(p) = &mut prev.kind {
                    p.items.extend(l.items.iter().cloned());
                }
            }
            _ => out.push(b.clone()),
        }
    }
    for b in &mut out {
        if let ast::DcsKind::Where(l) = &mut b.kind {
            for it in &mut l.items {
                if it.alias.is_none()
                    && let ExprKind::Param(p) = &it.expr.kind
                {
                    it.alias = Some(ast::Alias {
                        name: p.name.clone(),
                        as_kw: Some(crate::to_ast::SP),
                    });
                }
            }
        }
    }
    out
}

pub(crate) fn order(items: &mut [ast::OrderItem]) {
    for it in items {
        it.expr = expr(&it.expr);
    }
}

pub(crate) fn totals(t: &mut ast::Totals) {
    for it in &mut t.items {
        it.expr = expr(&it.expr);
    }
    for p in &mut t.points {
        if let ControlPoint::Field {
            expr: e, periods, ..
        } = p
        {
            *e = expr(e);
            if let Some(per) = periods {
                per.begin = per.begin.as_ref().map(expr);
                per.end = per.end.as_ref().map(expr);
            }
        }
    }
}

pub(crate) fn totals_aliases(t: &mut ast::Totals, selected: &[ast::SelectItem]) {
    for it in &mut t.items {
        let redundant = it.alias.as_ref().is_some_and(|alias| {
            let ExprKind::Func {
                func,
                args: FuncArgs::List(args),
                ..
            } = &it.expr.kind
            else {
                return false;
            };
            let [
                Expr {
                    kind: ExprKind::Path(path),
                    ..
                },
            ] = args.as_slice()
            else {
                return false;
            };
            let [name] = path.parts.as_slice() else {
                return false;
            };
            let name = fold_name(&name.text);
            func.is_aggregate()
                && fold_name(&alias.name.text) == name
                && selected
                    .iter()
                    .filter(|item| {
                        item.alias
                            .as_ref()
                            .is_some_and(|alias| fold_name(&alias.name.text) == name)
                    })
                    .count()
                    == 1
        });
        if redundant {
            it.alias = None;
        }
    }
}

pub(crate) fn has_aggregate(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Func { func, args, .. } => {
            func.is_aggregate() || matches!(args, FuncArgs::List(a) if a.iter().any(has_aggregate))
        }
        ExprKind::Paren(x) | ExprKind::Unary { expr: x, .. } => has_aggregate(x),
        ExprKind::Chain { first, rest } => {
            has_aggregate(first) || rest.iter().any(|o| has_aggregate(&o.expr))
        }
        ExprKind::Compare { l, r, .. } => has_aggregate(l) || has_aggregate(r),
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            operand.as_deref().is_some_and(has_aggregate)
                || whens
                    .iter()
                    .any(|w| has_aggregate(&w.cond) || has_aggregate(&w.then))
                || else_.as_deref().is_some_and(has_aggregate)
        }
        ExprKind::Cast { expr: x, .. } | ExprKind::Deref { expr: x, .. } => has_aggregate(x),
        _ => false,
    }
}

fn has_field(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) => true,
        ExprKind::Func {
            func,
            args: FuncArgs::List(a),
            ..
        } => a
            .iter()
            .enumerate()
            .any(|(i, x)| unit_pos(*func) != Some(i) && has_field(x)),
        ExprKind::Paren(x)
        | ExprKind::Unary { expr: x, .. }
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::Deref { expr: x, .. } => has_field(x),
        ExprKind::Chain { first, rest } => {
            has_field(first) || rest.iter().any(|o| has_field(&o.expr))
        }
        ExprKind::Compare { l, r, .. } => has_field(l) || has_field(r),
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            operand.as_deref().is_some_and(has_field)
                || whens
                    .iter()
                    .any(|w| has_field(&w.cond) || has_field(&w.then))
                || else_.as_deref().is_some_and(has_field)
        }
        _ => false,
    }
}

pub(crate) fn complete_group(op: &mut Operator) {
    if op.group.is_some() {
        return;
    }
    let exprs: Vec<&Expr> = op
        .fields
        .iter()
        .filter_map(|it| match &it.kind {
            ItemKind::Expr(e) => Some(e),
            _ => None,
        })
        .collect();
    if !exprs.iter().any(|e| has_aggregate(e)) {
        return;
    }
    let group: Vec<Expr> = exprs
        .into_iter()
        .filter(|e| !has_aggregate(e) && has_field(e))
        .map(expr)
        .collect();
    if !group.is_empty() {
        op.group = Some(GroupKind::Exprs(group));
    }
}

fn leaves(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Path(_) => out.push(query_syntax::print::expr_sexpr(e)),
        ExprKind::Func {
            func,
            args: FuncArgs::List(a),
            ..
        } => a
            .iter()
            .enumerate()
            .filter(|(i, _)| unit_pos(*func) != Some(*i))
            .for_each(|(_, x)| leaves(x, out)),
        ExprKind::Paren(x)
        | ExprKind::Unary { expr: x, .. }
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::Deref { expr: x, .. } => leaves(x, out),
        ExprKind::Chain { first, rest } => {
            leaves(first, out);
            rest.iter().for_each(|o| leaves(&o.expr, out));
        }
        ExprKind::Compare { l, r, .. } => {
            leaves(l, out);
            leaves(r, out);
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            if let Some(o) = operand {
                leaves(o, out);
            }
            for w in whens {
                leaves(&w.cond, out);
                leaves(&w.then, out);
            }
            if let Some(x) = else_ {
                leaves(x, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn regroup(op: &mut Operator, dcs: bool) {
    let Some(GroupKind::Exprs(written)) = &op.group else {
        return;
    };
    let aggregate = op
        .fields
        .iter()
        .any(|it| matches!(&it.kind, ItemKind::Expr(e) if has_aggregate(e)))
        || op.having.iter().any(has_aggregate);
    let columns: Vec<Expr> = op
        .fields
        .iter()
        .filter_map(|it| match &it.kind {
            ItemKind::Expr(e) if has_field(e) && !has_aggregate(e) => Some(expr(e)),
            _ => None,
        })
        .collect();
    let group = if aggregate {
        let mut seen: HashSet<String> = written
            .iter()
            .map(|e| query_syntax::print::expr_sexpr(&expr(e)))
            .collect();
        let plain: HashSet<String> = written
            .iter()
            .filter(|e| matches!(e.kind, ExprKind::Path(_)))
            .map(|e| query_syntax::print::expr_sexpr(&expr(e)))
            .collect();
        let mut group = written.clone();
        for c in &columns {
            let mut read = Vec::new();
            leaves(c, &mut read);
            let covered = !read.is_empty() && read.iter().all(|p| plain.contains(p));
            if !covered && seen.insert(query_syntax::print::expr_sexpr(c)) {
                group.push(c.clone());
            }
        }
        if !dcs
            && group
                .iter()
                .any(|e| matches!(e.kind, ExprKind::Value { .. } | ExprKind::Lit(_)))
            && group.iter().any(has_field)
        {
            let selected: HashSet<String> = op
                .fields
                .iter()
                .filter_map(|it| match &it.kind {
                    ItemKind::Expr(e)
                        if matches!(e.kind, ExprKind::Value { .. } | ExprKind::Lit(_)) =>
                    {
                        Some(query_syntax::print::expr_sexpr(&expr(e)))
                    }
                    _ => None,
                })
                .collect();
            group.retain(|e| {
                !matches!(e.kind, ExprKind::Value { .. } | ExprKind::Lit(_))
                    || !selected.contains(&query_syntax::print::expr_sexpr(&expr(e)))
            });
        }
        if !dcs {
            let key = |e: &Expr| query_syntax::print::expr_sexpr(&expr(e));
            let mut remaining = std::collections::HashMap::<String, usize>::new();
            for e in group.iter().filter(|e| matches!(e.kind, ExprKind::Path(_))) {
                *remaining.entry(key(e)).or_default() += 1;
            }
            for c in columns
                .into_iter()
                .filter(|c| matches!(c.kind, ExprKind::Path(_)))
            {
                if let Some(count) = remaining.get_mut(&key(&c)) {
                    if *count == 0 {
                        group.push(c);
                    } else {
                        *count -= 1;
                    }
                }
            }
        }
        group
    } else if !dcs
        && op.having.is_empty()
        && written.iter().all(|e| matches!(e.kind, ExprKind::Path(_)))
        && op.fields.iter().all(|it| {
            matches!(
                &it.kind,
                ItemKind::Expr(Expr {
                    kind: ExprKind::Path(_)
                        | ExprKind::Lit(_)
                        | ExprKind::Param(_)
                        | ExprKind::Value { .. },
                    ..
                })
            )
        })
        && {
            let keys: HashSet<String> = written
                .iter()
                .map(|e| query_syntax::print::expr_sexpr(&expr(e)))
                .collect();
            columns
                .iter()
                .all(|e| keys.contains(&query_syntax::print::expr_sexpr(e)))
        }
    {
        written.clone()
    } else {
        let own: HashSet<String> = columns
            .iter()
            .map(query_syntax::print::expr_sexpr)
            .collect();
        let written: Vec<Expr> = written.iter().map(expr).collect();
        let key = |e: &Expr| query_syntax::print::expr_sexpr(e);
        let written_keys: HashSet<String> = written.iter().map(key).collect();
        let mut group: Vec<Expr> = written
            .iter()
            .filter(|g| own.contains(&key(g)))
            .cloned()
            .collect();
        let plain: HashSet<String> = written
            .iter()
            .filter(|e| matches!(e.kind, ExprKind::Path(_)))
            .map(key)
            .collect();
        let added: Vec<Expr> = columns
            .into_iter()
            .filter(|c| !written_keys.contains(&key(c)))
            .filter(|c| {
                let mut r = Vec::new();
                leaves(c, &mut r);
                !dcs || r.is_empty() || !r.iter().all(|p| plain.contains(p))
            })
            .collect();
        let mut read = Vec::new();
        added.iter().for_each(|c| leaves(c, &mut read));
        let read: HashSet<String> = read.into_iter().collect();
        if dcs {
            let mut kept: Vec<Expr> = Vec::new();
            for g in written {
                let mut paths = Vec::new();
                leaves(&g, &mut paths);
                let covered = !paths.is_empty() && paths.iter().all(|p| read.contains(p));
                if !covered || own.contains(&key(&g)) {
                    kept.push(g);
                }
            }
            kept.extend(added);
            group = kept;
            op.group = Some(GroupKind::Exprs(group));
            return;
        }
        group.extend(added);
        for g in written {
            let mut paths = Vec::new();
            leaves(&g, &mut paths);
            let covered = !paths.is_empty() && paths.iter().all(|p| read.contains(p));
            if !covered && !own.contains(&key(&g)) {
                group.push(g);
            }
        }
        group
    };
    if !group.is_empty() {
        op.group = Some(GroupKind::Exprs(group));
    }
}
