use super::{Builder, R, internal, out};
use crate::QuerySchema;
use crate::ir::{ColSel, Expr, ExprKind, Rel, SortKey, SrcId, SrcKind};
use crate::schema::{FieldRole, QTableId, StdRole, TableKind};
use crate::tir::{self, Col, From, TExpr, TKind};
use mdschema::MdKind;
use query_syntax::ast::OrderDir;

#[derive(Clone, Copy)]
pub(crate) struct HierarchyUnion {
    pub(crate) column: usize,
    pub(crate) visible: usize,
    pub(crate) table: QTableId,
    pub(crate) desc: bool,
}

fn has_aggregate(expr: &TExpr) -> bool {
    if matches!(expr.kind, TKind::Agg { .. }) {
        return true;
    }
    let mut found = false;
    crate::sema::select::each_child(expr, &mut |child| {
        found = found || has_aggregate(child);
    });
    found
}

pub(crate) fn hierarchy_union_shape(
    query: &tir::Query,
    schema: &QuerySchema,
) -> Option<HierarchyUnion> {
    if query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !all)
        || query.autoorder
        || query.totals.is_some()
    {
        return None;
    }
    let [order] = query.order.as_slice() else {
        return None;
    };
    let desc = match order.dir {
        OrderDir::Hierarchy => false,
        OrderDir::HierarchyDesc => true,
        _ => return None,
    };
    let column = order.col?;
    let first = query.parts.first()?;
    let visible = first.items.len();
    let [atom] = first.items.get(column)?.expr.ty.refs.as_slice() else {
        return None;
    };
    let table = schema.ref_info(*atom).table;
    if schema.table(table).kind != TableKind::Object(MdKind::Catalog)
        || schema.hierarchical_table(*atom) != Some(table)
    {
        return None;
    }
    for arm in &query.parts {
        let [source] = arm.sources.as_slice() else {
            return None;
        };
        if arm.distinct
            || arm.group.is_some()
            || arm.having.is_some()
            || arm.items.len() != visible
            || !source.joins.is_empty()
            || arm
                .items
                .iter()
                .any(|item| item.nested.is_some() || item.unknown || has_aggregate(&item.expr))
        {
            return None;
        }
        let From::Table { id, .. } = &source.from else {
            return None;
        };
        if *id != table {
            return None;
        }
        let key = &arm.items.get(column)?.expr;
        let TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(field),
            derefs,
        } = &key.kind
        else {
            return None;
        };
        if !derefs.is_empty()
            || key.ty.atom_count() != 1
            || key.ty.refs.as_slice() != [*atom]
            || schema.field(table, *field)?.role != FieldRole::Std(StdRole::Ref)
            || !schema
                .table(table)
                .fields
                .iter()
                .any(|field| field.role == FieldRole::Std(StdRole::Parent))
        {
            return None;
        }
    }
    Some(HierarchyUnion {
        column,
        visible,
        table,
        desc,
    })
}

impl HierarchyUnion {
    pub(crate) fn source(self, schema: &QuerySchema, expr: &Expr, kind: &SrcKind) -> Option<SrcId> {
        let ExprKind::Col {
            src,
            col: ColSel::Field(field),
        } = expr.kind
        else {
            return None;
        };
        let SrcKind::Table(table) = kind else {
            return None;
        };
        let [atom] = expr.ty.refs.as_slice() else {
            return None;
        };
        (*table == self.table
            && schema.table(*table).kind == TableKind::Object(MdKind::Catalog)
            && expr.ty.atom_count() == 1
            && schema.ref_info(*atom).table == *table
            && schema.hierarchical_table(*atom) == Some(*table)
            && schema.field(*table, field)?.role == FieldRole::Std(StdRole::Ref))
        .then_some(src)
    }

    pub(crate) fn slots(self) -> Option<(u16, u16)> {
        Some((
            u16::try_from(self.visible).ok()?,
            u16::try_from(self.visible.checked_add(1)?).ok()?,
        ))
    }
}

fn projection(part: &Rel) -> Option<&[Expr]> {
    match part {
        Rel::Project { exprs, .. } => Some(exprs),
        Rel::Top { input, .. } => match input.as_ref() {
            Rel::Project { exprs, .. } => Some(exprs),
            _ => None,
        },
        _ => None,
    }
}

impl Builder<'_> {
    pub(super) fn hierarchy_union(
        &self,
        query: &tir::Query,
        mut rel: Rel,
        types: &[crate::QType],
    ) -> R<(Rel, bool)> {
        if self.query_depth != 1 || self.temp_out {
            return Ok((rel, false));
        }
        let Some(shape) = hierarchy_union_shape(query, self.schema) else {
            return Ok((rel, false));
        };
        let Rel::Union { parts, .. } = &mut rel else {
            return internal("a hierarchy union without union input");
        };
        for part in parts.iter() {
            let Some(exprs) = projection(part).filter(|exprs| exprs.len() == shape.visible) else {
                return Ok((rel, false));
            };
            let Some(base) = exprs.get(shape.column) else {
                return Ok((rel, false));
            };
            let ExprKind::Col { src, .. } = base.kind else {
                return Ok((rel, false));
            };
            let Some(source) = self.sources.get(src.0 as usize) else {
                return Ok((rel, false));
            };
            if shape.source(self.schema, base, &source.kind).is_none()
                || self.hierarchy_of(src).is_none()
            {
                return Ok((rel, false));
            }
        }
        let Some((id, parent)) = shape.slots() else {
            return internal("too many columns");
        };
        for part in parts {
            let project = match part {
                Rel::Top { input, .. } => input.as_mut(),
                other => other,
            };
            let Rel::Project { exprs, .. } = project else {
                return internal("a qualified hierarchy union arm without projection");
            };
            let Some(base) = exprs.get(shape.column) else {
                return internal("a qualified hierarchy union key without source");
            };
            let ExprKind::Col { src, .. } = base.kind else {
                return internal("a qualified hierarchy union key without source");
            };
            let Some((reference, ancestor)) = self.hierarchy_of(src) else {
                return internal("a qualified hierarchy union source without parent");
            };
            exprs.push(reference);
            exprs.push(ancestor);
        }
        let Some(ty) = types.get(shape.column) else {
            return internal("a hierarchy union key outside visible types");
        };
        let keys = vec![SortKey {
            expr: out(shape.column, ty.clone())?,
            desc: shape.desc,
        }];
        Ok((
            Rel::Hierarchy {
                input: Box::new(rel),
                id,
                parent,
                keys,
            },
            true,
        ))
    }
}
