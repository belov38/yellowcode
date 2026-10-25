use query_syntax::ast::{self, ExprKind, FuncArgs, InRhs, ItemKind};
use query_syntax::{Func, Kw};

pub(crate) fn batch(batch: &mut ast::Batch, text: &str) {
    for statement in &mut batch.stmts {
        if let ast::Stmt::Query(statement) = statement {
            query(&mut statement.query, text);
        }
    }
}

fn query(query: &mut ast::Query, text: &str) {
    for part in &mut query.parts {
        for field in &mut part.items {
            item(field, text);
        }
        if let Some(from) = &mut part.from {
            for item in &mut from.sources {
                source(item, text);
            }
        }
        for clause in part.where_.iter_mut().chain(part.having.iter_mut()) {
            expr(&mut clause.expr, text);
        }
        if let Some(group) = &mut part.group_by {
            match &mut group.kind {
                ast::GroupKind::Exprs(values) => {
                    values.iter_mut().for_each(|value| expr(value, text))
                }
                ast::GroupKind::Sets(sets) => sets
                    .iter_mut()
                    .flatten()
                    .for_each(|value| expr(value, text)),
            }
        }
        if let Some(index) = &mut part.index_by {
            match &mut index.kind {
                ast::IndexKind::Fields { fields, .. } => {
                    fields.iter_mut().for_each(|value| expr(value, text))
                }
                ast::IndexKind::Sets(sets) => sets
                    .iter_mut()
                    .flat_map(|set| &mut set.fields)
                    .for_each(|value| expr(value, text)),
            }
        }
        for block in &mut part.dcs {
            dcs(block, text);
        }
    }
    if let Some(order) = &mut query.order_by {
        for item in &mut order.items {
            expr(&mut item.expr, text);
        }
    }
    if let Some(totals) = &mut query.totals {
        for item in &mut totals.items {
            expr(&mut item.expr, text);
        }
        for point in &mut totals.points {
            if let ast::ControlPoint::Field {
                expr: value,
                periods,
                ..
            } = point
            {
                expr(value, text);
                if let Some(periods) = periods {
                    for value in periods.begin.iter_mut().chain(periods.end.iter_mut()) {
                        expr(value, text);
                    }
                }
            }
        }
    }
    for block in &mut query.dcs {
        dcs(block, text);
    }
}

fn item(item: &mut ast::SelectItem, text: &str) {
    match &mut item.kind {
        ItemKind::Expr(value) => expr(value, text),
        ItemKind::Nested { table, fields } => {
            expr(table, text);
            for field in fields {
                self::item(field, text);
            }
        }
        ItemKind::Star | ItemKind::PathStar(_) | ItemKind::EmptyTable { .. } => {}
    }
}

fn source(source: &mut ast::Source, text: &str) {
    match &mut source.table {
        ast::TableRef::Subquery { query: nested, .. } => query(nested, text),
        ast::TableRef::Named {
            args: Some(args), ..
        } => {
            for arg in args.args.iter_mut().flatten() {
                expr(&mut arg.expr, text);
                for group in &mut arg.dcs {
                    list(group, text);
                }
            }
        }
        ast::TableRef::Named { args: None, .. } | ast::TableRef::Param(_) => {}
    }
    for join in &mut source.joins {
        self::source(&mut join.right, text);
        expr(&mut join.on, text);
    }
}

fn dcs(block: &mut ast::DcsBlock, text: &str) {
    match &mut block.kind {
        ast::DcsKind::Select(items)
        | ast::DcsKind::Where(items)
        | ast::DcsKind::GroupBy(items)
        | ast::DcsKind::OrderBy(items)
        | ast::DcsKind::Totals(items) => list(items, text),
        ast::DcsKind::Characteristics(characteristics) => {
            for source in [&mut characteristics.kinds, &mut characteristics.values] {
                if let ast::CharSource::Query(nested) = source {
                    query(nested, text);
                }
            }
        }
    }
}

fn list(list: &mut ast::DcsList, text: &str) {
    for item in &mut list.items {
        expr(&mut item.expr, text);
        if let Some(nested) = &mut item.nested {
            self::list(nested, text);
        }
        for group in &mut item.dcs {
            self::list(group, text);
        }
    }
}

fn expr(value: &mut ast::Expr, text: &str) {
    match &mut value.kind {
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
        | ExprKind::Paren(inner) => expr(inner, text),
        ExprKind::Chain { first, rest } => {
            expr(first, text);
            for op in rest {
                expr(&mut op.expr, text);
            }
        }
        ExprKind::Compare { l, r, .. } => {
            expr(l, text);
            expr(r, text);
        }
        ExprKind::In { l, rhs, .. } => {
            expr(l, text);
            match rhs {
                InRhs::List(values) => values.iter_mut().for_each(|value| expr(value, text)),
                InRhs::Query(nested) => query(nested, text),
            }
        }
        ExprKind::Between { l, lo, hi, .. } => {
            expr(l, text);
            expr(lo, text);
            expr(hi, text);
        }
        ExprKind::Like { l, pattern, .. } => {
            expr(l, text);
            expr(pattern, text);
        }
        ExprKind::Tuple(values) | ExprKind::DateTime { args: values, .. } => {
            values.iter_mut().for_each(|value| expr(value, text))
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            for value in operand.iter_mut().chain(else_.iter_mut()) {
                expr(value, text);
            }
            for when in whens {
                expr(&mut when.cond, text);
                expr(&mut when.then, text);
            }
        }
        ExprKind::Func {
            func,
            name_span,
            args: FuncArgs::List(args),
            ..
        } => {
            for arg in &mut *args {
                expr(arg, text);
            }
            if matches!(func, Func::BeginOfPeriod | Func::EndOfPeriod) && args.len() == 1 {
                let day = if name_span.text(text).eq_ignore_ascii_case(func.en()) {
                    Kw::Day.en()
                } else {
                    Kw::Day.ru()
                };
                args.push(ast::Expr {
                    span: *name_span,
                    kind: ExprKind::Path(ast::Path {
                        parts: vec![ast::Ident {
                            text: day.into(),
                            span: *name_span,
                        }],
                    }),
                });
            }
        }
        ExprKind::Func {
            args: FuncArgs::Star(_),
            ..
        } => {}
        ExprKind::Dcs(items) => list(items, text),
    }
}
