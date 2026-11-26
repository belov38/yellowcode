use crate::schema::{FieldRole, QTableId, QuerySchema, StdRole, TableKind};
use crate::tir::{self, Col, From, Query, Select, Source, TExpr, TKind};
use crate::types::QType;
use query_syntax::Func;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnionPresent {
    Movement { present: u16, raw: u16 },
    Ref,
}

pub(crate) fn is_pure_ref_type(ty: &QType) -> bool {
    !ty.refs.is_empty() && ty.refs.len() == ty.atom_count()
}

pub(crate) fn union_presentation(query: &Query, schema: &QuerySchema) -> Option<UnionPresent> {
    if let Some((present, raw)) = super::uniform_movement_union_presentation(query) {
        return Some(UnionPresent::Movement { present, raw });
    }
    is_ref_union_presentation(query, schema).then_some(UnionPresent::Ref)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemPres {
    None,
    Ref,
    Unsupported,
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

fn find_source<'a>(sources: &'a [Source], remaining: &mut u16) -> Option<&'a Source> {
    for source in sources {
        if *remaining == 0 {
            return Some(source);
        }
        *remaining -= 1;
        for join in &source.joins {
            if let Some(found) = find_source(std::slice::from_ref(&join.right), remaining) {
                return Some(found);
            }
        }
    }
    None
}

fn source_table(schema: &QuerySchema, sources: &[Source], index: u16) -> Option<QTableId> {
    let mut remaining = index;
    let source = find_source(sources, &mut remaining)?;
    let From::Table { id, .. } = &source.from else {
        return None;
    };
    (!matches!(schema.table(*id).kind, TableKind::Virtual(_))).then_some(*id)
}

fn classify_item(schema: &QuerySchema, select: &Select, item: &tir::Item) -> ItemPres {
    match &item.expr.kind {
        TKind::Func {
            f: Func::Presentation | Func::RefPresentation,
            args,
        } => match args.as_slice() {
            [arg] if is_pure_ref_type(&arg.ty) => ItemPres::Ref,
            _ => ItemPres::Unsupported,
        },
        TKind::Field {
            level: 0,
            source,
            col,
            derefs,
        } => {
            if let Some((last, _)) = derefs.split_last() {
                if !last.targets.is_empty()
                    && last
                        .targets
                        .iter()
                        .all(|(t, f)| schema.is_presentation_field(*t, *f))
                {
                    let all_refs = last
                        .targets
                        .iter()
                        .all(|(t, _)| schema.table(*t).reference.is_some());
                    return if all_refs {
                        ItemPres::Ref
                    } else {
                        ItemPres::Unsupported
                    };
                }
                return ItemPres::None;
            }
            let Col::Field(field) = *col else {
                return ItemPres::None;
            };
            let Some(table) = source_table(schema, &select.sources, *source) else {
                return ItemPres::None;
            };
            if !schema.is_presentation_field(table, field) {
                return ItemPres::None;
            }
            let has_pure_ref = schema
                .table(table)
                .fields
                .iter()
                .find(|f| f.role == FieldRole::Std(StdRole::Ref))
                .is_some_and(|f| is_pure_ref_type(&f.ty));
            if has_pure_ref {
                ItemPres::Ref
            } else {
                ItemPres::Unsupported
            }
        }
        _ => ItemPres::None,
    }
}

fn is_ref_union_presentation(query: &Query, schema: &QuerySchema) -> bool {
    if query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !*all)
        || !query.order.is_empty()
        || query.autoorder
        || query.totals.is_some()
    {
        return false;
    }
    let Some(first) = query.parts.first() else {
        return false;
    };
    let width = first.items.len();
    for part in &query.parts {
        if part.distinct
            || part.group.is_some()
            || part.having.is_some()
            || part.items.len() != width
            || part
                .items
                .iter()
                .any(|item| item.nested.is_some() || has_aggregate(&item.expr))
        {
            return false;
        }
    }
    let mut has_presentation = false;
    for col in 0..width {
        let Some(first_item) = first.items.get(col) else {
            return false;
        };
        let expected = match classify_item(schema, first, first_item) {
            ItemPres::Unsupported => return false,
            ItemPres::Ref => {
                has_presentation = true;
                ItemPres::Ref
            }
            ItemPres::None => ItemPres::None,
        };
        for part in query.parts.iter().skip(1) {
            let Some(item) = part.items.get(col) else {
                return false;
            };
            if classify_item(schema, part, item) != expected {
                return false;
            }
        }
    }
    has_presentation
}
