use query_exec::TempTables;
use query_syntax::ast::{Batch, Expr, ExprKind, ItemKind, Stmt, TableRef};

pub(super) fn scalar_batch(
    batch: &Batch,
    temp: &TempTables,
    schema: &query_ir::QuerySchema,
) -> bool {
    if temp.names().iter().filter_map(|n| temp.table(n)).any(|t| {
        t.schema.columns.iter().any(|c| {
            c.unknown
                || c.ty
                    .as_ref()
                    .is_none_or(|ty| !ty.refs.is_empty() || !ty.others.is_empty())
        })
    }) {
        return false;
    }
    for stmt in &batch.stmts {
        let Stmt::Query(stmt) = stmt else {
            continue;
        };
        let q = &stmt.query;
        if q.order_by.is_some() || q.totals.is_some() || !q.dcs.is_empty() {
            return false;
        }
        for s in &q.parts {
            if s.where_.is_some()
                || s.group_by.is_some()
                || s.having.is_some()
                || s.for_update.is_some()
                || !s.dcs.is_empty()
            {
                return false;
            }
            if let Some(from) = &s.from {
                for source in &from.sources {
                    if !source.joins.is_empty()
                        || !matches!(&source.table,
                        TableRef::Named { path, args: None } if path.parts.len() == 1
                        && path.parts.first().is_some_and(|p| schema.find_table(&[p.text.as_ref()]).is_none()))
                    {
                        return false;
                    }
                }
            }
            for item in &s.items {
                match &item.kind {
                    ItemKind::Star => {}
                    ItemKind::PathStar(p) if p.parts.len() == 1 => {}
                    ItemKind::Expr(e) if scalar_expr(e) => {}
                    _ => return false,
                }
            }
        }
    }
    true
}
fn scalar_expr(e: &Expr) -> bool {
    let mut pending = vec![e];
    while let Some(e) = pending.pop() {
        match &e.kind {
            ExprKind::Lit(_) => {}
            ExprKind::Path(p) if p.parts.len() <= 2 => {}
            ExprKind::Chain { first, rest } => {
                pending.push(first);
                pending.extend(rest.iter().map(|r| &r.expr));
            }
            ExprKind::Paren(e) | ExprKind::Unary { expr: e, .. } => pending.push(e),
            _ => return false,
        }
    }
    true
}
