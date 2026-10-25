use crate::ast::{self, ExprKind, FuncArgs, InRhs, ItemKind};
use crate::with_deep_stack;

pub fn unresolved_dcs_tails(batch: &ast::Batch) -> Vec<ast::DcsUnresolvedTail> {
    with_deep_stack(batch.max_depth, || {
        let mut out = Vec::new();
        each_tail(batch, &mut |tail| out.push(tail.clone()));
        out.sort_by_key(|tail| match tail {
            ast::DcsUnresolvedTail::AdjacentParam(param) => param.span.start,
        });
        out
    })
}

pub fn first_unresolved_dcs_tail_at(batch: &ast::Batch) -> Option<u32> {
    let mut first: Option<u32> = None;
    each_tail(batch, &mut |tail| {
        let ast::DcsUnresolvedTail::AdjacentParam(param) = tail;
        let at = param.span.start;
        if first.is_none_or(|f| at < f) {
            first = Some(at);
        }
    });
    first
}

fn each_tail(batch: &ast::Batch, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    for statement in &batch.stmts {
        if let ast::Stmt::Query(statement) = statement {
            query(&statement.query, out);
        }
    }
}

fn query(query: &ast::Query, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    for part in &query.parts {
        for field in &part.items {
            item(field, out);
        }
        if let Some(from) = &part.from {
            for item in &from.sources {
                source(item, out);
            }
        }
        for clause in part.where_.iter().chain(part.having.iter()) {
            expr(&clause.expr, out);
        }
        if let Some(group) = &part.group_by {
            match &group.kind {
                ast::GroupKind::Exprs(values) => values.iter().for_each(|value| expr(value, out)),
                ast::GroupKind::Sets(sets) => {
                    sets.iter().flatten().for_each(|value| expr(value, out))
                }
            }
        }
        if let Some(index) = &part.index_by {
            match &index.kind {
                ast::IndexKind::Fields { fields, .. } => {
                    fields.iter().for_each(|value| expr(value, out))
                }
                ast::IndexKind::Sets(sets) => sets
                    .iter()
                    .flat_map(|set| &set.fields)
                    .for_each(|value| expr(value, out)),
            }
        }
        for block in &part.dcs {
            dcs(block, out);
        }
    }
    if let Some(order) = &query.order_by {
        for item in &order.items {
            expr(&item.expr, out);
        }
    }
    if let Some(totals) = &query.totals {
        for item in &totals.items {
            expr(&item.expr, out);
        }
        for point in &totals.points {
            if let ast::ControlPoint::Field {
                expr: value,
                periods,
                ..
            } = point
            {
                expr(value, out);
                if let Some(periods) = periods {
                    for value in periods.begin.iter().chain(periods.end.iter()) {
                        expr(value, out);
                    }
                }
            }
        }
    }
    for block in &query.dcs {
        dcs(block, out);
    }
}

fn item(item: &ast::SelectItem, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    match &item.kind {
        ItemKind::Expr(value) => expr(value, out),
        ItemKind::Nested { table, fields } => {
            expr(table, out);
            for field in fields {
                self::item(field, out);
            }
        }
        ItemKind::Star | ItemKind::PathStar(_) | ItemKind::EmptyTable { .. } => {}
    }
}

fn source(source: &ast::Source, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    match &source.table {
        ast::TableRef::Subquery { query: nested, .. } => query(nested, out),
        ast::TableRef::Named {
            args: Some(args), ..
        } => {
            for arg in args.args.iter().flatten() {
                expr(&arg.expr, out);
                for group in &arg.dcs {
                    list(group, out);
                }
            }
        }
        ast::TableRef::Named { args: None, .. } | ast::TableRef::Param(_) => {}
    }
    for join in &source.joins {
        self::source(&join.right, out);
        expr(&join.on, out);
    }
}

fn dcs(block: &ast::DcsBlock, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    match &block.kind {
        ast::DcsKind::Select(items)
        | ast::DcsKind::Where(items)
        | ast::DcsKind::GroupBy(items)
        | ast::DcsKind::OrderBy(items)
        | ast::DcsKind::Totals(items) => list(items, out),
        ast::DcsKind::Characteristics(characteristics) => {
            for source in [&characteristics.kinds, &characteristics.values] {
                if let ast::CharSource::Query(nested) = source {
                    query(nested, out);
                }
            }
        }
    }
}

fn list(list: &ast::DcsList, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    if let Some(tail) = &list.unresolved_tail {
        out(tail);
    }
    for item in &list.items {
        expr(&item.expr, out);
        if let Some(nested) = &item.nested {
            self::list(nested, out);
        }
        for group in &item.dcs {
            self::list(group, out);
        }
    }
}

fn expr(value: &ast::Expr, out: &mut dyn FnMut(&ast::DcsUnresolvedTail)) {
    match &value.kind {
        ExprKind::Lit(_)
        | ExprKind::Param(_)
        | ExprKind::Path(_)
        | ExprKind::TypeOf { .. }
        | ExprKind::Value { .. } => {}
        ExprKind::Deref { expr: inner, .. }
        | ExprKind::Unary { expr: inner, .. }
        | ExprKind::IsNull { l: inner, .. }
        | ExprKind::Refs { l: inner, .. }
        | ExprKind::Cast { expr: inner, .. }
        | ExprKind::Paren(inner) => expr(inner, out),
        ExprKind::Chain { first, rest } => {
            expr(first, out);
            for op in rest {
                expr(&op.expr, out);
            }
        }
        ExprKind::Compare { l, r, .. } => {
            expr(l, out);
            expr(r, out);
        }
        ExprKind::In { l, rhs, .. } => {
            expr(l, out);
            match rhs {
                InRhs::List(values) => values.iter().for_each(|value| expr(value, out)),
                InRhs::Query(nested) => query(nested, out),
            }
        }
        ExprKind::Between { l, lo, hi, .. } => {
            expr(l, out);
            expr(lo, out);
            expr(hi, out);
        }
        ExprKind::Like { l, pattern, .. } => {
            expr(l, out);
            expr(pattern, out);
        }
        ExprKind::Tuple(values) | ExprKind::DateTime { args: values, .. } => {
            values.iter().for_each(|value| expr(value, out))
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            for value in operand.iter().chain(else_.iter()) {
                expr(value, out);
            }
            for when in whens {
                expr(&when.cond, out);
                expr(&when.then, out);
            }
        }
        ExprKind::Func {
            args: FuncArgs::List(args),
            ..
        } => {
            args.iter().for_each(|arg| expr(arg, out));
        }
        ExprKind::Func {
            args: FuncArgs::Star(_),
            ..
        } => {}
        ExprKind::Dcs(items) => list(items, out),
    }
}
