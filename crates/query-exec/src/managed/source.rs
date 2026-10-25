use memory_budget::{BudgetError, Category, LimitReason, MemoryBudget, MemoryLimit, Reservation};
use query_ir::managed_value::{ManagedRow, row::ScopedRow};

#[derive(Debug)]
pub struct ScopedRows<I> {
    input: Option<I>,
    charge: Reservation,
}
impl<I> ScopedRows<I> {
    pub fn try_new(input: I, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        if budget.scope_snapshot().is_some() {
            return Err(invalid());
        }
        let charge = budget.try_reserve(
            Category::Execution,
            u64::try_from(std::mem::size_of::<Self>()).map_err(|_| BudgetError::Overflow)?,
        )?;
        Ok(Self {
            input: Some(input),
            charge,
        })
    }
}
impl<I, E> Iterator for ScopedRows<I>
where
    I: Iterator<Item = Result<(u64, ManagedRow), E>>,
    E: From<BudgetError>,
{
    type Item = Result<(u64, ScopedRow), E>;
    fn next(&mut self) -> Option<Self::Item> {
        let next = self.input.as_mut()?.next();
        match next {
            None => {
                drop(self.input.take());
                None
            }
            Some(Err(error)) => {
                drop(self.input.take());
                Some(Err(error))
            }
            Some(Ok((id, row))) => {
                let result = if row
                    .reservation()
                    .budget()
                    .same_instance(self.charge.budget())
                {
                    ScopedRow::try_from_owned(row)
                } else {
                    Err(invalid())
                };
                match result {
                    Ok(row) => Some(Ok((id, row))),
                    Err(error) => {
                        drop(self.input.take());
                        Some(Err(error.into()))
                    }
                }
            }
        }
    }
}
impl<I, E> std::iter::FusedIterator for ScopedRows<I>
where
    I: Iterator<Item = Result<(u64, ManagedRow), E>>,
    E: From<BudgetError>,
{
}
fn invalid() -> BudgetError {
    BudgetError::Limit(MemoryLimit {
        category: Category::Execution,
        requested: 0,
        reason: LimitReason::InvalidSize,
    })
}
