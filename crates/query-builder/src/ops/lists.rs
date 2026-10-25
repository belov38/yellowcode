use super::{
    Direction, Op, OpError, PointKind, QueryRef, column_name, expr, is_null_filler, null_item,
    operator_mut, query_mut, unique,
};
use crate::model::*;
use crate::open::Ctx;
use crate::{canon, to_ast};
use bsl_prim::fold_name;
use query_syntax::ast::{self, ControlPoint, GroupKind, Hierarchy, IndexKind, IntoKind, ItemKind};

fn no_item(what: &'static str, index: usize) -> OpError {
    OpError::NoItem { what, index }
}

pub(super) fn apply(s: &mut Schema, op: &Op, _ctx: &Ctx<'_>) -> Result<(), OpError> {
    match op {
        Op::AddField {
            q,
            op,
            expr: text,
            alias,
        } => add_field(s, q, *op, text, alias.as_deref()),
        Op::SetField {
            q,
            op,
            column,
            expr: text,
        } => set_field(s, q, *op, *column, text),
        Op::RemoveField { q, op, column } => remove_field(s, q, *op, *column),
        Op::SetAlias { q, column, alias } => {
            let query = query_mut(s, q)?;
            pad_names(query);
            let n = query.names.len();
            let slot = query.names.get_mut(*column).ok_or(no_item("column", n))?;
            *slot = Some(alias.clone());
            if let Some(ast::SelectItem { alias: a, .. }) = query
                .operators
                .first_mut()
                .and_then(|o| o.fields.get_mut(*column))
            {
                *a = None;
            }
            Ok(())
        }
        Op::MoveColumn { q, column, to } => move_column(s, q, *column, *to),
        Op::AddCondition { q, op, expr: text } => {
            let mut e = expr(s, text)?;
            if let ast::ExprKind::Chain { rest, .. } = &mut e.kind
                && let Some(r) = rest.first_mut().filter(|r| r.op == ast::BinOp::And)
            {
                r.op_span = to_ast::UNSPLIT;
            }
            let o = operator_mut(s, q, *op)?;
            if canon::has_aggregate(&e) {
                o.having.push(e);
            } else {
                o.filter.push(e);
            }
            Ok(())
        }
        Op::SetCondition {
            q,
            op,
            index,
            expr: text,
        } => {
            let e = expr(s, text)?;
            let o = operator_mut(s, q, *op)?;
            let nf = o.filter.len();
            let slot = if *index < nf {
                o.filter.get_mut(*index)
            } else {
                o.having.get_mut(*index - nf)
            };
            *slot.ok_or(no_item("condition", *index))? = e;
            Ok(())
        }
        Op::RemoveCondition { q, op, index } => {
            let o = operator_mut(s, q, *op)?;
            let nf = o.filter.len();
            if *index < nf {
                o.filter.remove(*index);
            } else if *index - nf < o.having.len() {
                o.having.remove(*index - nf);
            } else {
                return Err(no_item("condition", *index));
            }
            Ok(())
        }
        Op::AddGroup { q, op, expr: text } => {
            let e = expr(s, text)?;
            let o = operator_mut(s, q, *op)?;
            match &mut o.group {
                Some(GroupKind::Exprs(v)) => {
                    let key = |x: &ast::Expr| query_syntax::print::expr_sexpr(&canon::expr(x));
                    let k = key(&e);
                    if !v.iter().any(|x| key(x) == k) {
                        v.push(e);
                    }
                }
                Some(GroupKind::Sets(_)) => return Err(OpError::Unsupported("grouping sets")),
                None => o.group = Some(GroupKind::Exprs(vec![e])),
            }
            Ok(())
        }
        Op::RemoveGroup { q, op, index } => {
            let o = operator_mut(s, q, *op)?;
            match &mut o.group {
                Some(GroupKind::Exprs(v)) => {
                    if *index >= v.len() {
                        return Err(no_item("group", *index));
                    }
                    v.remove(*index);
                    if v.is_empty() {
                        o.group = None;
                    }
                    Ok(())
                }
                Some(GroupKind::Sets(_)) => Err(OpError::Unsupported("grouping sets")),
                None => Err(no_item("group", *index)),
            }
        }
        Op::AddOrder { q, expr: text, dir } => {
            let e = expr(s, text)?;
            query_mut(s, q)?.order.push(order_item(e, *dir));
            Ok(())
        }
        Op::SetOrderDir { q, index, dir } => {
            let it = query_mut(s, q)?
                .order
                .get_mut(*index)
                .ok_or(no_item("order", *index))?;
            let e = it.expr.clone();
            *it = order_item(e, Some(*dir));
            Ok(())
        }
        Op::RemoveOrder { q, index } => {
            let query = query_mut(s, q)?;
            if *index >= query.order.len() {
                return Err(no_item("order", *index));
            }
            query.order.remove(*index);
            Ok(())
        }
        Op::SetAutoorder { q, on } => {
            query_mut(s, q)?.autoorder = *on;
            Ok(())
        }
        Op::AddTotal { q, expr: text } => {
            let mut e = expr(s, text)?;
            if !canon::has_aggregate(&e) {
                e = sum_of(s, text)?;
            }
            totals_mut(query_mut(s, q)?).items.push(ast::TotalsItem {
                expr: e,
                alias: None,
            });
            Ok(())
        }
        Op::RemoveTotal { q, index } => {
            let query = query_mut(s, q)?;
            let t = query.totals.as_mut().ok_or(no_item("total", *index))?;
            if *index >= t.items.len() {
                return Err(no_item("total", *index));
            }
            t.items.remove(*index);
            drop_empty_totals(query);
            Ok(())
        }
        Op::AddControlPoint {
            q,
            expr: text,
            kind,
        } => {
            let e = expr(s, text)?;
            let hierarchy = match kind.unwrap_or(PointKind::Items) {
                PointKind::Items => None,
                PointKind::Hierarchy => Some(Hierarchy::All(to_ast::SP)),
                PointKind::HierarchyOnly => Some(Hierarchy::Only(to_ast::SP)),
            };
            totals_mut(query_mut(s, q)?)
                .points
                .push(ControlPoint::Field {
                    expr: e,
                    hierarchy,
                    periods: None,
                    alias: None,
                    span: to_ast::SP,
                });
            Ok(())
        }
        Op::RemoveControlPoint { q, index } => {
            let query = query_mut(s, q)?;
            let t = query
                .totals
                .as_mut()
                .ok_or(no_item("control point", *index))?;
            let pos = t
                .points
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p, ControlPoint::Field { .. }))
                .nth(*index)
                .map(|(i, _)| i)
                .ok_or(no_item("control point", *index))?;
            t.points.remove(pos);
            drop_empty_totals(query);
            Ok(())
        }
        Op::SetOverall { q, on } => {
            let query = query_mut(s, q)?;
            let has = query.totals.as_ref().is_some_and(|t| {
                t.points
                    .iter()
                    .any(|p| matches!(p, ControlPoint::Overall(_)))
            });
            if *on && !has {
                totals_mut(query)
                    .points
                    .insert(0, ControlPoint::Overall(to_ast::SP));
            } else if !*on && has {
                if let Some(t) = &mut query.totals {
                    t.points.retain(|p| !matches!(p, ControlPoint::Overall(_)));
                }
                drop_empty_totals(query);
            }
            Ok(())
        }
        Op::SetPlacement { q, table } => {
            query_mut(s, q)?.into = table.as_ref().map(|t| Placement {
                kind: IntoKind::Put,
                table: t.clone(),
            });
            Ok(())
        }
        Op::AddIndex { q } => {
            query_mut(s, q)?.index = Some(IndexKind::Fields {
                fields: Vec::new(),
                unique: None,
            });
            Ok(())
        }
        Op::AddIndexField { q, expr: text } => {
            let e = expr(s, text)?;
            match &mut query_mut(s, q)?.index {
                Some(IndexKind::Fields { fields, .. }) => {
                    fields.push(e);
                    Ok(())
                }
                Some(IndexKind::Sets(_)) => Err(OpError::Unsupported("index sets")),
                None => Err(no_item("index", 0)),
            }
        }
        Op::SetIndexUnique { q, on } => match &mut query_mut(s, q)?.index {
            Some(IndexKind::Fields { unique, .. }) => {
                *unique = on.then_some(to_ast::SP);
                Ok(())
            }
            Some(IndexKind::Sets(_)) => Err(OpError::Unsupported("index sets")),
            None => Err(no_item("index", 0)),
        },
        Op::SetDistinct { q, op, on } => {
            operator_mut(s, q, *op)?.distinct = *on;
            Ok(())
        }
        Op::SetTop { q, op, n } => {
            operator_mut(s, q, *op)?.top = n.map(|n| n.to_string());
            Ok(())
        }
        Op::SetAllowed { q, on } => {
            query_mut(s, q)?.allowed = *on;
            Ok(())
        }
        Op::SetForUpdate { q, op, on } => {
            let o = operator_mut(s, q, *op)?;
            if !*on {
                o.for_update = None;
            } else if o.for_update.is_none() {
                o.for_update = Some(Vec::new());
            }
            Ok(())
        }
        Op::AddTableForUpdate { q, op, table } => {
            let path = ast::Path {
                parts: table.split('.').map(to_ast::ident).collect(),
            };
            operator_mut(s, q, *op)?
                .for_update
                .get_or_insert_with(Vec::new)
                .push(path);
            Ok(())
        }
        _ => Err(OpError::Unsupported("not an operation of the lists")),
    }
}

fn pad_names(query: &mut SelectQuery) {
    let width = query.operators.first().map_or(0, |o| o.fields.len());
    while query.names.len() < width {
        query.names.push(None);
    }
}

fn order_item(e: ast::Expr, dir: Option<Direction>) -> ast::OrderItem {
    let (dir, kw) = match dir.unwrap_or(Direction::Asc) {
        Direction::Asc => (ast::OrderDir::Asc, None),
        Direction::Desc => (ast::OrderDir::Desc, Some(to_ast::SP)),
        Direction::Hierarchy => (ast::OrderDir::Hierarchy, Some(to_ast::SP)),
        Direction::HierarchyDesc => (ast::OrderDir::HierarchyDesc, Some(to_ast::SP)),
    };
    ast::OrderItem {
        expr: e,
        dir,
        dir_kw: kw,
    }
}

fn totals_mut(q: &mut SelectQuery) -> &mut Totals {
    q.totals.get_or_insert_with(|| Totals {
        items: Vec::new(),
        points: Vec::new(),
    })
}

fn drop_empty_totals(q: &mut SelectQuery) {
    if q.totals
        .as_ref()
        .is_some_and(|t| t.items.is_empty() && t.points.is_empty())
    {
        q.totals = None;
    }
}

fn sum_of(s: &mut Schema, text: &str) -> Result<ast::Expr, OpError> {
    query_syntax::parse_expr(text, s.mode).map_err(OpError::Expr)?;
    expr(s, &format!("СУММА({text})"))
}

fn set_field(
    s: &mut Schema,
    q: &QueryRef,
    oi: usize,
    column: usize,
    text: &str,
) -> Result<(), OpError> {
    let e = expr(s, text)?;
    let query = query_mut(s, q)?;
    pad_names(query);
    let op = query.operators.get(oi).ok_or(OpError::NoOperator(oi))?;
    let old = op.fields.get(column).ok_or(no_item("field", column))?;
    if oi == 0 && query.names.get(column).is_some_and(Option::is_none) {
        let keep = old
            .alias
            .as_ref()
            .map(|a| a.name.text.to_string())
            .or_else(|| column_name(op, old));
        if let Some(n) = query.names.get_mut(column) {
            *n = keep;
        }
    }
    let item = ast::SelectItem {
        kind: ItemKind::Expr(e),
        alias: None,
        span: super::SP,
    };
    if let Some(f) = query
        .operators
        .get_mut(oi)
        .and_then(|o| o.fields.get_mut(column))
    {
        *f = item;
    }
    Ok(())
}

fn remove_field(s: &mut Schema, q: &QueryRef, oi: usize, column: usize) -> Result<(), OpError> {
    let query = query_mut(s, q)?;
    pad_names(query);
    let op = query.operators.get(oi).ok_or(OpError::NoOperator(oi))?;
    if column >= op.fields.len() {
        return Err(no_item("field", column));
    }
    for o in &mut query.operators {
        if column < o.fields.len() {
            o.fields.remove(column);
        }
    }
    if column < query.names.len() {
        query.names.remove(column);
    }
    Ok(())
}

fn move_column(s: &mut Schema, q: &QueryRef, column: usize, to: usize) -> Result<(), OpError> {
    let query = query_mut(s, q)?;
    pad_names(query);
    let width = query.operators.first().map_or(0, |o| o.fields.len());
    if column >= width {
        return Err(no_item("column", column));
    }
    if to >= width {
        return Err(no_item("column", to));
    }
    for o in &mut query.operators {
        if column < o.fields.len() && to < o.fields.len() {
            let f = o.fields.remove(column);
            o.fields.insert(to, f);
        }
    }
    let n = query.names.remove(column);
    query.names.insert(to, n);
    Ok(())
}

fn add_field(
    s: &mut Schema,
    q: &super::QueryRef,
    oi: usize,
    text: &str,
    alias: Option<&str>,
) -> Result<(), OpError> {
    let e = expr(s, text)?;
    let item = ast::SelectItem {
        kind: ItemKind::Expr(e),
        alias: None,
        span: super::SP,
    };
    let query = query_mut(s, q)?;
    let width = query.operators.first().map_or(0, |o| o.fields.len());
    while query.names.len() < width {
        query.names.push(None);
    }
    let op = query.operators.get(oi).ok_or(OpError::NoOperator(oi))?;
    if let Some(col) = op.fields.iter().position(is_null_filler) {
        if let Some(f) = query
            .operators
            .get_mut(oi)
            .and_then(|o| o.fields.get_mut(col))
        {
            *f = item;
        }
        if let (Some(a), Some(n)) = (alias, query.names.get_mut(col)) {
            *n = Some(a.to_string());
        }
        return Ok(());
    }
    let name = match (alias, oi) {
        (Some(a), _) => Some(a.to_string()),
        (None, 0) => None,
        (None, _) => column_name(op, &item).map(|n| {
            unique(&n, &|c| {
                query
                    .names
                    .iter()
                    .flatten()
                    .any(|x| fold_name(x) == fold_name(c))
            })
        }),
    };
    for (k, o) in query.operators.iter_mut().enumerate() {
        o.fields
            .push(if k == oi { item.clone() } else { null_item() });
    }
    query.names.push(name);
    Ok(())
}
