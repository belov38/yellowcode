use crate::schema::{QFieldId, QTableId, QuerySchema, TableKind};
use crate::tir::{self, Col, TKind};
use query_syntax::ast::OrderDir;

pub(crate) fn supported(schema: &QuerySchema, query: &tir::Query) -> bool {
    if query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !all)
        || query.autoorder
        || query.totals.is_some()
    {
        return false;
    }
    let Some(first) = query.parts.first() else {
        return false;
    };
    let Some((column, section, item)) = projection(schema, first) else {
        return false;
    };
    let Some(fields) = item.nested.as_deref() else {
        return false;
    };
    query.parts.iter().skip(1).all(|part| {
        let Some((other_column, other_section, other_item)) = projection(schema, part) else {
            return false;
        };
        let Some(other_fields) = other_item.nested.as_deref() else {
            return false;
        };
        column == other_column
            && section == other_section
            && first.items.len() == part.items.len()
            && item.star == other_item.star
            && item.expr.ty == other_item.expr.ty
            && fields.len() == other_fields.len()
            && fields.iter().zip(other_fields).all(|(field, other)| {
                field.star == other.star
                    && field.name == other.name
                    && field.expr.ty == other.expr.ty
                    && direct_field(&field.expr) == direct_field(&other.expr)
            })
    })
}

pub(crate) fn validate_order(query: &tir::Query) -> Result<(), &'static str> {
    for key in &query.order {
        if !matches!(key.dir, OrderDir::Asc | OrderDir::Desc) {
            return Err("ИЕРАРХИЯ в объединении");
        }
        let Some(item) = key.col.and_then(|col| query.parts.first()?.items.get(col)) else {
            return Err("вложенная таблица в объединении");
        };
        if item.nested.is_some() {
            return Err("сортировка по вложенной таблице");
        }
    }
    Ok(())
}

fn projection<'a>(
    schema: &QuerySchema,
    select: &'a tir::Select,
) -> Option<(usize, QTableId, &'a tir::Item)> {
    if select.distinct || select.group.is_some() || select.having.is_some() {
        return None;
    }
    let [source] = select.sources.as_slice() else {
        return None;
    };
    if !source.joins.is_empty() {
        return None;
    }
    let tir::From::Table { id, .. } = &source.from else {
        return None;
    };
    if !matches!(schema.table(*id).kind, TableKind::Object(_)) {
        return None;
    }
    let mut nested = select
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.nested.is_some());
    let (column, item) = nested.next()?;
    if nested.next().is_some() || item.unknown {
        return None;
    }
    let section = schema.field(*id, direct_field(&item.expr)?)?.nested?;
    let fields = item.nested.as_deref()?;
    if fields.is_empty()
        || !fields.iter().all(|field| {
            !field.unknown
                && field.nested.is_none()
                && direct_field(&field.expr)
                    .and_then(|id| schema.field(section, id))
                    .is_some_and(|field| field.nested.is_none())
        })
    {
        return None;
    }
    Some((column, section, item))
}

fn direct_field(expr: &tir::TExpr) -> Option<QFieldId> {
    match &expr.kind {
        TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(field),
            derefs,
        } if derefs.is_empty() => Some(*field),
        _ => None,
    }
}
