use super::*;

#[derive(Clone, Copy)]
pub(crate) struct UnionAutoorder {
    pub(crate) reference_desc: bool,
    pub(crate) scalar_desc: Option<bool>,
}

pub(crate) fn uniform_union_autoorder(
    query: &Query,
    schema: &QuerySchema,
) -> Option<UnionAutoorder> {
    if query.order.len() == 2 {
        two_keys(query, schema)
    } else {
        single_key(query, schema).map(|reference_desc| UnionAutoorder {
            reference_desc,
            scalar_desc: None,
        })
    }
}

fn direction(order: &tir::OrderKey, column: usize) -> Option<bool> {
    if order.col != Some(column) {
        return None;
    }
    match order.dir {
        OrderDir::Asc => Some(false),
        OrderDir::Desc => Some(true),
        _ => None,
    }
}

fn same_type(left: &QType, right: &QType) -> bool {
    left.boolean == right.boolean
        && left.number == right.number
        && left.string == right.string
        && left.date == right.date
        && left.refs.as_slice() == right.refs.as_slice()
        && left.others.as_slice() == right.others.as_slice()
}

fn scalar_type(ty: &QType) -> bool {
    ty.atom_count() == 1
        && ty.refs.is_empty()
        && ty.others.is_empty()
        && ty.string.is_none_or(|qualifier| qualifier.len > 0)
}

fn direct_item(item: &tir::Item, schema: &QuerySchema, table: QTableId) -> bool {
    if item.star || item.unknown || item.nested.is_some() {
        return false;
    }
    let TKind::Field {
        level: 0,
        source: 0,
        col: Col::Field(field),
        derefs,
    } = &item.expr.kind
    else {
        return false;
    };
    derefs.is_empty()
        && schema.field(table, *field).is_some_and(|field| {
            field.nested.is_none()
                && field.role != FieldRole::Std(StdRole::Presentation)
                && same_type(&field.ty, &item.expr.ty)
        })
}

fn two_keys(query: &Query, schema: &QuerySchema) -> Option<UnionAutoorder> {
    if !query.autoorder
        || query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !*all)
        || query.parts.iter().skip(1).any(|arm| arm.allowed)
        || query.totals.is_some()
    {
        return None;
    }
    let [reference_order, scalar_order] = query.order.as_slice() else {
        return None;
    };
    let reference_desc = direction(reference_order, 0)?;
    let scalar_desc = direction(scalar_order, 1)?;
    let [reference, scalar] = query.parts.first()?.items.as_slice() else {
        return None;
    };
    let [atom] = reference.expr.ty.refs.as_slice() else {
        return None;
    };
    let info = schema.ref_info(*atom);
    let required = match info.kind {
        mdschema::MdKind::Document => StdRole::Date,
        mdschema::MdKind::Enum => StdRole::Order,
        _ => return None,
    };
    if !schema
        .table(info.table)
        .fields
        .iter()
        .any(|field| field.role == FieldRole::Std(required))
        || !scalar_type(&scalar.expr.ty)
    {
        return None;
    }
    for arm in &query.parts {
        let [reference, suffix] = arm.items.as_slice() else {
            return None;
        };
        let [source] = arm.sources.as_slice() else {
            return None;
        };
        if arm.distinct
            || arm.top.is_some()
            || arm.group.is_some()
            || arm.having.is_some()
            || arm.for_update.is_some()
            || !arm.index_by.is_empty()
            || !arm.index_unique.is_empty()
            || source.all_fields
            || !source.joins.is_empty()
            || reference.expr.ty.atom_count() != 1
            || reference.expr.ty.refs.as_slice() != [*atom]
            || !scalar_type(&suffix.expr.ty)
            || !same_type(&scalar.expr.ty, &suffix.expr.ty)
        {
            return None;
        }
        let From::Table {
            id,
            args,
            records,
            accounting,
            drcr,
            complement,
            breakdown,
            ..
        } = &source.from
        else {
            return None;
        };
        if !matches!(schema.table(*id).kind, TableKind::Object(_))
            || !args.is_empty()
            || records.is_some()
            || accounting.is_some()
            || drcr.is_some()
            || complement.is_some()
            || !breakdown.is_empty()
            || !direct_item(reference, schema, *id)
            || !direct_item(suffix, schema, *id)
            || arm
                .where_
                .as_ref()
                .is_some_and(|expr| !union_autoorder_filter(expr, schema, *id))
        {
            return None;
        }
    }
    Some(UnionAutoorder {
        reference_desc,
        scalar_desc: Some(scalar_desc),
    })
}

fn single_key(query: &Query, schema: &QuerySchema) -> Option<bool> {
    if !query.autoorder
        || query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !*all)
        || query.parts.iter().skip(1).any(|arm| arm.allowed)
        || query.totals.is_some()
    {
        return None;
    }
    let [order] = query.order.as_slice() else {
        return None;
    };
    if order.col != Some(0) {
        return None;
    }
    let desc = match order.dir {
        OrderDir::Asc => false,
        OrderDir::Desc => true,
        _ => return None,
    };
    let first = query.parts.first()?;
    let [item] = first.items.as_slice() else {
        return None;
    };
    let [atom] = item.expr.ty.refs.as_slice() else {
        return None;
    };
    let info = schema.ref_info(*atom);
    let required = match info.kind {
        mdschema::MdKind::Catalog => StdRole::Description,
        mdschema::MdKind::Document => StdRole::Date,
        mdschema::MdKind::Enum => StdRole::Order,
        _ => return None,
    };
    if !schema
        .table(info.table)
        .fields
        .iter()
        .any(|f| f.role == FieldRole::Std(required))
    {
        return None;
    }
    for arm in &query.parts {
        let [item] = arm.items.as_slice() else {
            return None;
        };
        let [source] = arm.sources.as_slice() else {
            return None;
        };
        if arm.distinct
            || arm.top.is_some()
            || arm.group.is_some()
            || arm.having.is_some()
            || arm.for_update.is_some()
            || !arm.index_by.is_empty()
            || !arm.index_unique.is_empty()
            || item.star
            || item.unknown
            || item.nested.is_some()
            || item.expr.ty.atom_count() != 1
            || item.expr.ty.refs.as_slice() != [*atom]
            || source.all_fields
            || !source.joins.is_empty()
        {
            return None;
        }
        let TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(_),
            derefs,
        } = &item.expr.kind
        else {
            return None;
        };
        if !derefs.is_empty() {
            return None;
        }
        let From::Table {
            id,
            args,
            records,
            drcr,
            complement,
            breakdown,
            ..
        } = &source.from
        else {
            return None;
        };
        if !matches!(schema.table(*id).kind, TableKind::Object(_))
            || !args.is_empty()
            || records.is_some()
            || drcr.is_some()
            || complement.is_some()
            || !breakdown.is_empty()
            || arm
                .where_
                .as_ref()
                .is_some_and(|e| !union_autoorder_filter(e, schema, *id))
        {
            return None;
        }
    }
    Some(desc)
}

fn union_autoorder_filter(expr: &TExpr, schema: &QuerySchema, table: QTableId) -> bool {
    match &expr.kind {
        TKind::InQuery { .. }
        | TKind::Agg { .. }
        | TKind::InList {
            hierarchy: true, ..
        } => return false,
        TKind::Func {
            f: Func::Presentation | Func::RefPresentation,
            ..
        } => return false,
        TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(field),
            derefs,
        } if derefs.is_empty() => {
            if schema.field(table, *field).is_none_or(|field| {
                field.nested.is_some() || field.role == FieldRole::Std(StdRole::Presentation)
            }) {
                return false;
            }
        }
        TKind::Field { .. } => return false,
        TKind::Cast { derefs, .. } if !derefs.is_empty() => return false,
        _ => {}
    }
    let mut scalar = true;
    crate::sema::select::each_child(expr, &mut |child| {
        scalar = scalar && union_autoorder_filter(child, schema, table);
    });
    scalar
}

impl Builder<'_> {
    pub(super) fn union_autoorder_part(
        &mut self,
        part: &mut Rel,
        sort: &mut Vec<SortKey>,
        reference_desc: bool,
        scalar_desc: Option<bool>,
    ) -> R<()> {
        let Rel::Project { exprs, .. } = part else {
            return internal("an autoordered union arm without a projection");
        };
        let visible = 1 + usize::from(scalar_desc.is_some());
        if exprs.len() != visible {
            return internal(if scalar_desc.is_some() {
                "an autoordered union arm without its visible keys"
            } else {
                "an autoordered union arm without one raw reference"
            });
        }
        let Some(base) = exprs.first() else {
            return internal("an autoordered union arm without one raw reference");
        };
        let automatic = self.auto_keys(base)?;
        if sort.is_empty() {
            for (index, key) in automatic.iter().enumerate() {
                sort.push(SortKey {
                    expr: out(index + visible, key.ty.clone())?,
                    desc: reference_desc,
                });
            }
            if let Some(desc) = scalar_desc {
                let Some(scalar) = exprs.get(1) else {
                    return internal("an autoordered union arm without its visible keys");
                };
                sort.push(SortKey {
                    expr: out(1, scalar.ty.clone())?,
                    desc,
                });
            }
        } else if sort.len().checked_sub(usize::from(scalar_desc.is_some()))
            != Some(automatic.len())
        {
            return internal("nonuniform automatic union key layout");
        }
        exprs.extend(automatic);
        Ok(())
    }
}
