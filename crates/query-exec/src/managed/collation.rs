use bsl_prim::Strength;
use icu46_sys::{Collator, IcuStatus, SORT_KEY_SLACK, SortKeyWrite};
use memory_budget::{
    BudgetError, BudgetedVec, Category, LimitReason, MemoryBudget, MemoryLimit, Reservation,
};
use std::ffi::CStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollationError {
    Budget(BudgetError),
    Icu(IcuStatus),
}
impl From<BudgetError> for CollationError {
    fn from(error: BudgetError) -> Self {
        Self::Budget(error)
    }
}
impl From<IcuStatus> for CollationError {
    fn from(error: IcuStatus) -> Self {
        Self::Icu(error)
    }
}
impl std::fmt::Display for CollationError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(error) => error.fmt(output),
            Self::Icu(error) => error.fmt(output),
        }
    }
}
impl std::error::Error for CollationError {}

pub struct ManagedCollation {
    secondary: Collator,
    tertiary: Collator,
    charge: Reservation,
}
impl ManagedCollation {
    pub fn try_new(locale: &CStr, budget: &MemoryBudget) -> Result<Self, CollationError> {
        let charge = budget
            .try_reserve(Category::Execution, std::mem::size_of::<Self>() as u64)
            .map_err(BudgetError::from)?;
        let secondary = Collator::try_new_cstr(locale, icu46_sys::Strength::Secondary)?;
        let tertiary = Collator::try_new_cstr(locale, icu46_sys::Strength::Tertiary)?;
        Ok(Self {
            secondary,
            tertiary,
            charge,
        })
    }

    pub fn try_sort_key(
        &self,
        strength: Strength,
        text: &[u16],
        budget: &MemoryBudget,
    ) -> Result<BudgetedVec<u8>, CollationError> {
        self.check_budget(budget)?;
        i32::try_from(text.len()).map_err(|_| BudgetError::Overflow)?;
        let padded_len = text.len().checked_add(1).ok_or(BudgetError::Overflow)?;
        let mut source = BudgetedVec::try_with_capacity(budget, Category::Execution, padded_len)?;
        source.try_extend_from_slice(text)?;
        source.try_push(0)?;
        let collator = match strength {
            Strength::Secondary => &self.secondary,
            Strength::Tertiary => &self.tertiary,
        };
        let mut probe = [0; SORT_KEY_SLACK + 1];
        let needed = match collator.try_sort_key_into(&source, &mut probe)? {
            SortKeyWrite::Complete(length) | SortKeyWrite::Required(length) => length,
        };
        let capacity = needed
            .checked_add(SORT_KEY_SLACK)
            .ok_or(BudgetError::Overflow)?;
        let mut output = BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)?;
        output.try_resize(capacity, 0)?;
        match collator.try_sort_key_into(&source, &mut output)? {
            SortKeyWrite::Complete(length) => {
                output.truncate(length);
                Ok(output)
            }
            SortKeyWrite::Required(_) => Err(BudgetError::Overflow.into()),
        }
    }

    pub(super) fn check_budget(&self, budget: &MemoryBudget) -> Result<(), CollationError> {
        if self.charge.budget().same_instance(budget) {
            Ok(())
        } else {
            Err(BudgetError::Limit(MemoryLimit {
                category: Category::Execution,
                requested: 0,
                reason: LimitReason::InvalidSize,
            })
            .into())
        }
    }

    pub fn try_sorting_key(
        &self,
        text: &[u16],
        budget: &MemoryBudget,
    ) -> Result<BudgetedVec<u8>, CollationError> {
        let end = text
            .iter()
            .rposition(|&unit| unit != 0x20)
            .map_or(0, |index| index + 1);
        self.try_sort_key(Strength::Secondary, text.get(..end).unwrap_or(&[]), budget)
    }
}
