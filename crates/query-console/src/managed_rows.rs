use crate::managed::ManagedCell;
use memory_budget::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryBudget, MemoryLimit, TextCount,
};
use query_ir::managed_value::{ManagedValue, ValueRef, row::ScopedRow};
use query_syntax::Lang;
use query_testhost::{TestBase, measure_managed_repr};
use std::fmt::Write;

fn checked_add(first: u64, second: u64) -> Result<u64, BudgetError> {
    first.checked_add(second).ok_or(BudgetError::Overflow)
}
fn retained_refusal(error: BudgetError) -> Result<Option<BudgetedVec<ManagedCell>>, BudgetError> {
    match error {
        BudgetError::Limit(limit)
            if limit.category == Category::Result && limit.reason == LimitReason::Category =>
        {
            Ok(None)
        }
        error => Err(error),
    }
}

pub fn try_retain_scoped_row(
    row: &ScopedRow,
    visible_columns: usize,
    base: &TestBase,
    lang: Lang,
    canonical_limit: u64,
) -> Result<Option<BudgetedVec<ManagedCell>>, BudgetError> {
    let count = row.values().len().min(visible_columns);
    let values = row.values().get(..count).ok_or(BudgetError::Overflow)?;
    try_retain_row(values, base, lang, row.budget(), canonical_limit)
}

pub fn try_retain_row(
    values: &[ManagedValue],
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    canonical_limit: u64,
) -> Result<Option<BudgetedVec<ManagedCell>>, BudgetError> {
    let descriptors = values
        .len()
        .checked_mul(std::mem::size_of::<ManagedCell>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<BudgetedVec<ManagedCell>>()))
        .ok_or(BudgetError::Overflow)?;
    let mut required = u64::try_from(descriptors).map_err(|_| BudgetError::Overflow)?;
    for value in values {
        let len = measure_managed_repr(value, base, lang, budget, canonical_limit)?;
        required = checked_add(required, len)?;
        required = checked_add(required, std::mem::size_of::<Charged<String>>() as u64)?;
        if let ValueRef::Ref { table, .. } = value.view() {
            let mut kind = TextCount::new(Category::Row, canonical_limit);
            if let Some(name) = base.ref_xml_name(table) {
                kind.try_push_str(name)?;
            } else {
                write!(&mut kind, "Ref#{table}")
                    .map_err(|_| kind.error().copied().unwrap_or(BudgetError::Overflow))?;
            }
            required = checked_add(required, kind.len())?;
            required = checked_add(required, 36)?;
            required = checked_add(required, 2 * std::mem::size_of::<Charged<String>>() as u64)?;
        }
        check_row_limit(required, canonical_limit)?;
    }
    check_row_limit(required, canonical_limit)?;
    match budget.try_reserve(Category::Result, required) {
        Ok(probe) => drop(probe),
        Err(error) => return retained_refusal(error.into()),
    }
    let create = || {
        let mut row = BudgetedVec::try_with_capacity(budget, Category::Result, values.len())?;
        for value in values {
            row.try_push(ManagedCell::try_new(
                value,
                base,
                lang,
                budget,
                canonical_limit,
            )?)?;
        }
        Ok(row)
    };
    match create() {
        Ok(row) => Ok(Some(row)),
        Err(error) => retained_refusal(error),
    }
}

fn check_row_limit(requested: u64, limit: u64) -> Result<(), BudgetError> {
    if requested > limit {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Row,
            requested,
            reason: LimitReason::Category,
        }));
    }
    Ok(())
}
