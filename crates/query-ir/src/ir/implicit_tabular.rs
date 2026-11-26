use crate::ir::{ColSel, SrcId};
use crate::schema::{FieldRole, QFieldId, QTableId, QuerySchema, StdRole};
use crate::tir::{self, TExpr, TKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImplicitSectionKey {
    pub(crate) owner_src: SrcId,
    pub(crate) owner_col: ColSel,
    pub(crate) tab_table: QTableId,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImplicitSectionAccess {
    pub(crate) key: ImplicitSectionKey,
    pub(crate) tab_field: QFieldId,
    pub(crate) rest_start: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum ImplicitWork<'t> {
    Expr(&'t TExpr, u16),
    Source(&'t tir::Source, u16),
    Query(&'t tir::Query, u16),
}

fn ref_field(schema: &QuerySchema, table: QTableId) -> Option<QFieldId> {
    schema
        .table(table)
        .fields
        .iter()
        .position(|f| f.role == FieldRole::Std(StdRole::Ref))
        .and_then(|i| u16::try_from(i).ok())
        .map(QFieldId)
}

fn single_nested_target(schema: &QuerySchema, step: &tir::Deref) -> Option<QTableId> {
    let &[(table, field)] = step.targets.as_slice() else {
        return None;
    };
    schema.field(table, field)?.nested
}

fn single_table_field(step: &tir::Deref, expected_table: QTableId) -> Option<QFieldId> {
    let &(_, field) = step.targets.first()?;
    step.targets
        .iter()
        .all(|&(table, _)| table == expected_table)
        .then_some(field)
}

pub(crate) fn match_implicit_section(
    schema: &QuerySchema,
    owner_src: SrcId,
    owner_table: Option<QTableId>,
    col: ColSel,
    derefs: &[tir::Deref],
) -> Option<ImplicitSectionAccess> {
    let first = derefs.first()?;
    if let (Some(qt), ColSel::Field(field)) = (owner_table, col)
        && let Some(tab_table) = schema.field(qt, field).and_then(|f| f.nested)
    {
        let owner_ref = ref_field(schema, qt)?;
        ref_field(schema, tab_table)?;
        let tab_field = single_table_field(first, tab_table)?;
        return Some(ImplicitSectionAccess {
            key: ImplicitSectionKey {
                owner_src,
                owner_col: ColSel::Field(owner_ref),
                tab_table,
            },
            tab_field,
            rest_start: 1,
        });
    }
    let second = derefs.get(1)?;
    let tab_table = single_nested_target(schema, first)?;
    ref_field(schema, tab_table)?;
    let tab_field = single_table_field(second, tab_table)?;
    Some(ImplicitSectionAccess {
        key: ImplicitSectionKey {
            owner_src,
            owner_col: col,
            tab_table,
        },
        tab_field,
        rest_start: 2,
    })
}

pub(crate) fn push_select_work<'t, E>(
    sel: &'t tir::Select,
    order: Option<&'t [tir::OrderKey]>,
    extra: &[&'t TExpr],
    depth: u16,
    push: &mut impl FnMut(ImplicitWork<'t>) -> Result<(), E>,
) -> Result<(), E> {
    for &expr in extra.iter().rev() {
        push(ImplicitWork::Expr(expr, depth))?;
    }
    if let Some(order) = order {
        for key in order.iter().rev() {
            if key.col.is_none() {
                push(ImplicitWork::Expr(&key.expr, depth))?;
            }
        }
    }
    if let Some(having) = &sel.having {
        push(ImplicitWork::Expr(having, depth))?;
    }
    if let Some(group) = &sel.group {
        for set in group.iter().rev() {
            for expr in set.iter().rev() {
                push(ImplicitWork::Expr(expr, depth))?;
            }
        }
    }
    for item in sel.items.iter().rev() {
        if item.nested.is_none() {
            push(ImplicitWork::Expr(&item.expr, depth))?;
        }
    }
    if let Some(where_) = &sel.where_ {
        push(ImplicitWork::Expr(where_, depth))?;
    }
    for source in sel.sources.iter().rev() {
        push(ImplicitWork::Source(source, depth))?;
    }
    Ok(())
}

pub(crate) fn step_work<'t, E>(
    item: ImplicitWork<'t>,
    push: &mut impl FnMut(ImplicitWork<'t>) -> Result<(), E>,
    on_field: &mut impl FnMut(u16, tir::Col, &'t [tir::Deref]) -> Result<(), E>,
) -> Result<(), E> {
    match item {
        ImplicitWork::Source(source, depth) => {
            for join in source.joins.iter().rev() {
                push(ImplicitWork::Expr(&join.on, depth))?;
                push(ImplicitWork::Source(&join.right, depth))?;
            }
            if depth > 0
                && let tir::From::Subquery(sub) = &source.from
            {
                push(ImplicitWork::Query(sub, depth))?;
            }
        }
        ImplicitWork::Query(query, depth) => {
            let single_order = (query.parts.len() == 1).then_some(query.order.as_slice());
            for part in query.parts.iter().rev() {
                push_select_work(part, single_order, &[], depth, push)?;
            }
        }
        ImplicitWork::Expr(expr, depth) => match &expr.kind {
            TKind::Agg { nested: true, .. } => {}
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                if *level == depth {
                    on_field(*source, *col, derefs)?;
                }
            }
            TKind::InQuery { l, q, .. } => {
                if let Some(next_depth) = depth.checked_add(1) {
                    push(ImplicitWork::Query(q, next_depth))?;
                }
                for lhs in l.iter().rev() {
                    push(ImplicitWork::Expr(lhs, depth))?;
                }
            }
            TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => {}
            TKind::Neg(x)
            | TKind::Not(x)
            | TKind::IsNull { l: x, .. }
            | TKind::Refs { l: x, .. }
            | TKind::Cast { e: x, .. } => {
                push(ImplicitWork::Expr(x, depth))?;
            }
            TKind::Arith { first, steps } => {
                for step in steps.iter().rev() {
                    push(ImplicitWork::Expr(&step.rhs, depth))?;
                }
                push(ImplicitWork::Expr(first, depth))?;
            }
            TKind::And(v) | TKind::Or(v) | TKind::Func { args: v, .. } => {
                for child in v.iter().rev() {
                    push(ImplicitWork::Expr(child, depth))?;
                }
            }
            TKind::Compare(_, a, b) => {
                push(ImplicitWork::Expr(b, depth))?;
                push(ImplicitWork::Expr(a, depth))?;
            }
            TKind::InList { l, rhs, .. } => {
                for child in rhs.iter().rev() {
                    push(ImplicitWork::Expr(child, depth))?;
                }
                for child in l.iter().rev() {
                    push(ImplicitWork::Expr(child, depth))?;
                }
            }
            TKind::Between { l, lo, hi, .. } => {
                push(ImplicitWork::Expr(hi, depth))?;
                push(ImplicitWork::Expr(lo, depth))?;
                push(ImplicitWork::Expr(l, depth))?;
            }
            TKind::Like { l, pattern, .. } => {
                push(ImplicitWork::Expr(pattern, depth))?;
                push(ImplicitWork::Expr(l, depth))?;
            }
            TKind::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(e) = else_ {
                    push(ImplicitWork::Expr(e, depth))?;
                }
                for (cond, then) in whens.iter().rev() {
                    push(ImplicitWork::Expr(then, depth))?;
                    push(ImplicitWork::Expr(cond, depth))?;
                }
                if let Some(op) = operand {
                    push(ImplicitWork::Expr(op, depth))?;
                }
            }
            TKind::Agg {
                nested: false, arg, ..
            } => {
                if let Some(a) = arg {
                    push(ImplicitWork::Expr(a, depth))?;
                }
            }
        },
    }
    Ok(())
}
