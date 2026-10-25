use super::{CompileArena, NativeOwned};
use crate::{sema::R, tir};
use memory_budget::BudgetError;
pub(super) fn checked_depth(scope: Option<&CompileArena>, depth: u16) -> R<u16> {
    if scope.is_some() && depth > 128 {
        return super::allocation_error(scope, BudgetError::Overflow);
    }
    Ok(depth)
}
fn invalid<T>(scope: Option<&CompileArena>) -> R<T> {
    super::allocation_error(scope, BudgetError::Overflow)
}

pub(crate) struct SelectOptions {
    pub(crate) allowed: bool,
    pub(crate) distinct: bool,
    pub(crate) top: Option<u64>,
}
pub(crate) fn select<'a>(
    scope: Option<&'a CompileArena>,
    options: SelectOptions,
    items: NativeOwned<'a, Vec<tir::Item>>,
    where_: NativeOwned<'a, Option<tir::TExpr>>,
    group: NativeOwned<'a, Option<Vec<Vec<tir::TExpr>>>>,
    having: NativeOwned<'a, Option<tir::TExpr>>,
    for_update: NativeOwned<'a, Option<Vec<crate::schema::QTableId>>>,
) -> R<NativeOwned<'a, tir::Select>> {
    if !items.scope_matches(scope)
        || !where_.scope_matches(scope)
        || !group.scope_matches(scope)
        || !having.scope_matches(scope)
        || !for_update.scope_matches(scope)
    {
        return invalid(scope);
    }
    let value = items
        .zip(where_)
        .and_then(|v| v.zip(group))
        .and_then(|v| v.zip(having))
        .and_then(|v| v.zip(for_update));
    Ok(super::allocation_result(scope, value)?.map(
        |((((items, where_), group), having), for_update)| tir::Select {
            allowed: options.allowed,
            distinct: options.distinct,
            top: options.top,
            items,
            sources: Vec::new(),
            where_,
            group,
            having,
            for_update,
            index_by: Vec::new(),
            index_unique: Vec::new(),
        },
    ))
}
pub(crate) fn query<'a>(
    scope: Option<&'a CompileArena>,
    parts: NativeOwned<'a, Vec<tir::Select>>,
    unions: NativeOwned<'a, Vec<bool>>,
    order: NativeOwned<'a, Vec<tir::OrderKey>>,
    autoorder: bool,
    totals: NativeOwned<'a, Option<tir::Totals>>,
) -> R<NativeOwned<'a, tir::Query>> {
    if !parts.scope_matches(scope)
        || !unions.scope_matches(scope)
        || !order.scope_matches(scope)
        || !totals.scope_matches(scope)
    {
        return invalid(scope);
    }
    let value = parts
        .zip(unions)
        .and_then(|v| v.zip(order))
        .and_then(|v| v.zip(totals));
    Ok(
        super::allocation_result(scope, value)?.map(|(((parts, unions), order), totals)| {
            tir::Query {
                parts,
                unions,
                order,
                autoorder,
                totals,
            }
        }),
    )
}
pub(crate) fn source<'a>(
    scope: Option<&'a CompileArena>,
    alias: NativeOwned<'a, String>,
    from: NativeOwned<'a, tir::From>,
    joins: NativeOwned<'a, Vec<tir::Join>>,
) -> R<NativeOwned<'a, tir::Source>> {
    if !alias.scope_matches(scope) || !from.scope_matches(scope) || !joins.scope_matches(scope) {
        return invalid(scope);
    }
    Ok(
        super::allocation_result(scope, alias.zip(from).and_then(|v| v.zip(joins)))?
            .map(|((alias, from), joins)| tir::Source { alias, from, joins }),
    )
}
pub(crate) fn join<'a>(
    scope: Option<&'a CompileArena>,
    kind: query_syntax::ast::JoinKind,
    right: NativeOwned<'a, tir::Source>,
    on: NativeOwned<'a, tir::TExpr>,
) -> R<NativeOwned<'a, tir::Join>> {
    if !right.scope_matches(scope) || !on.scope_matches(scope) {
        return invalid(scope);
    }
    let depth = checked_depth(scope, right.depth().saturating_add(1).max(on.depth()))?;
    Ok(super::allocation_result(scope, right.zip(on))?
        .map(|(right, on)| tir::Join { kind, right, on })
        .set_depth(depth))
}

pub(crate) fn subquery<'a>(
    scope: Option<&'a CompileArena>,
    value: NativeOwned<'a, tir::Query>,
) -> R<NativeOwned<'a, tir::From>> {
    if !value.scope_matches(scope) {
        return invalid(scope);
    }
    let depth = checked_depth(scope, value.depth().saturating_add(1))?;
    Ok(super::allocation_result(scope, value.boxed())?
        .map(tir::From::Subquery)
        .set_depth(depth))
}
