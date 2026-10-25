use super::{
    collation::{CollationError, ManagedCollation},
    key::try_key_of,
};
use memory_budget::MemoryBudget;
use query_ir::managed_value::ValueRef;
use query_syntax::ast::CmpOp;
use std::cmp::Ordering;

pub fn rank(value: ValueRef<'_>) -> u8 {
    match value {
        ValueRef::Null => 0,
        ValueRef::Undefined => 1,
        ValueRef::Bool(_) => 2,
        ValueRef::Number(_) => 3,
        ValueRef::Date(_) => 4,
        ValueRef::String(_) => 5,
        ValueRef::Uuid(_) => 6,
        ValueRef::Ref { .. } => 7,
        ValueRef::Type(_) => 8,
        ValueRef::SysEnum(..) => 9,
        ValueRef::Binary(_) => 10,
        ValueRef::Storage(_) => 11,
        ValueRef::Nested(_) => 12,
    }
}

pub fn try_cmp(
    left: ValueRef<'_>,
    right: ValueRef<'_>,
    collator: &ManagedCollation,
    budget: &MemoryBudget,
) -> Result<Ordering, CollationError> {
    collator.check_budget(budget)?;
    match (left, right) {
        (ValueRef::Bool(a), ValueRef::Bool(b)) => return Ok(a.cmp(&b)),
        (ValueRef::Number(a), ValueRef::Number(b)) => return Ok(a.cmp(b)),
        (ValueRef::Date(a), ValueRef::Date(b)) => return Ok(a.ticks().cmp(&b.ticks())),
        _ => {}
    }
    let left_rank = rank(left);
    let order = left_rank.cmp(&rank(right));
    if order != Ordering::Equal || left_rank <= 1 {
        return Ok(order);
    }
    let left_key = try_key_of(left, collator, budget)?;
    let right_key = try_key_of(right, collator, budget)?;
    Ok(left_key.cmp(&right_key))
}

pub fn try_eq3(
    left: ValueRef<'_>,
    right: ValueRef<'_>,
    collator: &ManagedCollation,
    budget: &MemoryBudget,
) -> Result<Option<bool>, CollationError> {
    try_compare(CmpOp::Eq, left, right, collator, budget)
}

pub fn try_compare(
    op: CmpOp,
    left: ValueRef<'_>,
    right: ValueRef<'_>,
    collator: &ManagedCollation,
    budget: &MemoryBudget,
) -> Result<Option<bool>, CollationError> {
    collator.check_budget(budget)?;
    if matches!(left, ValueRef::Null) || matches!(right, ValueRef::Null) {
        return Ok(None);
    }
    let order = try_cmp(left, right, collator, budget)?;
    Ok(Some(match op {
        CmpOp::Eq => order == Ordering::Equal,
        CmpOp::Ne => order != Ordering::Equal,
        CmpOp::Lt => order == Ordering::Less,
        CmpOp::Gt => order == Ordering::Greater,
        CmpOp::Le => order != Ordering::Greater,
        CmpOp::Ge => order != Ordering::Less,
    }))
}
