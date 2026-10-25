use crate::error::{OpenError, Shape};
use crate::model::*;
use query_syntax::ast::DcsKind;

pub(crate) fn check(schema: &Schema) -> Result<(), OpenError> {
    for (i, st) in schema.batch.iter().enumerate() {
        if let Stmt::Select(q) = st
            && let Some(what) = query(q)
        {
            return Err(OpenError::Shape { stmt: i, what });
        }
    }
    Ok(())
}

fn query(q: &SelectQuery) -> Option<Shape> {
    if has_order(&q.dcs) {
        return Some(Shape::DcsOrder);
    }
    for op in &q.operators {
        if has_order(&op.dcs) {
            return Some(Shape::DcsOrder);
        }
        for s in &op.sources {
            if let Some(w) = source(s) {
                return Some(w);
            }
        }
    }
    None
}

fn has_order(blocks: &[query_syntax::ast::DcsBlock]) -> bool {
    blocks.iter().any(|b| matches!(b.kind, DcsKind::OrderBy(_)))
}

fn source(s: &Source) -> Option<Shape> {
    match &s.table {
        Table::Subquery(_) | Table::Param(_) if s.alias.is_none() => {
            return Some(Shape::SubqueryAlias);
        }
        Table::Subquery(q) => {
            if let Some(w) = query(q) {
                return Some(w);
            }
        }
        _ => {}
    }
    s.joins.iter().find_map(|j| source(&j.right))
}
