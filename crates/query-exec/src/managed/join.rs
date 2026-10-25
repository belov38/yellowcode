use memory_budget::{BudgetError, Category, RequestMemory, Shared};
use query_ir::managed_value::{
    ManagedRow,
    row::{ScopedInput, ScopedRow},
};

pub fn try_concat(
    left: Shared<ScopedRow>,
    right: Shared<ScopedRow>,
    request: &RequestMemory,
) -> Result<ScopedRow, BudgetError> {
    if request.budget.scope_snapshot().is_some() {
        return Err(BudgetError::Overflow);
    }
    let budget = request.budget.try_scope(Category::Row, request.row_limit)?;
    let left = ScopedInput::try_new(left, &budget)?;
    let right = ScopedInput::try_new(right, &budget)?;
    let columns = left
        .values()
        .len()
        .checked_add(right.values().len())
        .ok_or(BudgetError::Overflow)?;
    let mut output = ManagedRow::try_with_capacity(&budget, Category::Execution, columns)?;
    for value in left.values().iter().chain(right.values()) {
        output.try_push(value.clone())?;
    }
    ScopedRow::try_from_owned(output)
}

#[path = "join_rows.rs"]
mod rows;
pub use rows::{JoinMode, JoinRows, JoinSpec};
