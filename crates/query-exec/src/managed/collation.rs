use bsl_prim::Strength;
use icu46_sys::managed::runtime::{RuntimeError, quarantined};
use icu46_sys::{Collator, IcuStatus, SORT_KEY_SLACK, SortKeyWrite};
use memory_budget::{
    BudgetError, BudgetedVec, Category, LimitReason, MemoryBudget, MemoryLimit, Reservation,
};
use std::ffi::CStr;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollationError {
    Budget(BudgetError),
    Icu(IcuStatus),
    Runtime(RuntimeError),
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
            Self::Runtime(error) => write!(output, "{error:?}"),
        }
    }
}
impl std::error::Error for CollationError {}

pub struct ManagedCollation {
    secondary: Collator,
    tertiary: Collator,
    runtime: bool,
    charge: Reservation,
}
static NEVER: AtomicBool = AtomicBool::new(false);
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
            runtime: false,
            charge,
        })
    }

    pub fn try_open(
        locale: &CStr,
        budget: &MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<Self, CollationError> {
        not_quarantined()?;
        let charge = prepared(
            budget
                .try_reserve(Category::Execution, std::mem::size_of::<Self>() as u64)
                .map_err(BudgetError::from),
        )?;
        let open = |strength| {
            Collator::open_managed_cancellable(locale, strength, budget, cancel)
                .map_err(CollationError::Runtime)
        };
        let secondary = open(icu46_sys::Strength::Secondary)?;
        let tertiary = open(icu46_sys::Strength::Tertiary)?;
        Ok(Self {
            secondary,
            tertiary,
            runtime: true,
            charge,
        })
    }

    fn collator(&self, strength: Strength) -> &Collator {
        match strength {
            Strength::Secondary => &self.secondary,
            Strength::Tertiary => &self.tertiary,
        }
    }

    fn write(
        &self,
        collator: &Collator,
        source: &[u16],
        output: &mut [u8],
        budget: &MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<SortKeyWrite, CollationError> {
        if self.runtime {
            collator
                .sort_key_managed_cancellable(source, output, budget, cancel)
                .map_err(CollationError::Runtime)
        } else {
            Ok(collator.try_sort_key_into(source, output)?)
        }
    }

    fn source(text: &[u16], budget: &MemoryBudget) -> Result<BudgetedVec<u16>, BudgetError> {
        i32::try_from(text.len()).map_err(|_| BudgetError::Overflow)?;
        let padded_len = text.len().checked_add(1).ok_or(BudgetError::Overflow)?;
        let mut source = BudgetedVec::try_with_capacity(budget, Category::Execution, padded_len)?;
        source.try_extend_from_slice(text)?;
        source.try_push(0)?;
        Ok(source)
    }

    fn admit(&self) -> Result<(), CollationError> {
        if self.runtime {
            not_quarantined()
        } else {
            Ok(())
        }
    }

    fn prepared<T>(&self, result: Result<T, BudgetError>) -> Result<T, CollationError> {
        if self.runtime {
            prepared(result)
        } else {
            Ok(result?)
        }
    }

    pub fn try_sort_key_len(
        &self,
        strength: Strength,
        text: &[u16],
        budget: &MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<usize, CollationError> {
        self.check_budget(budget)?;
        self.admit()?;
        let source = self.prepared(Self::source(text, budget))?;
        let collator = self.collator(strength);
        if self.runtime {
            return collator
                .sort_key_len_managed(&source, budget, cancel)
                .map_err(CollationError::Runtime);
        }
        let mut probe = [0; SORT_KEY_SLACK + 1];
        match collator.try_sort_key_into(&source, &mut probe)? {
            SortKeyWrite::Complete(length) | SortKeyWrite::Required(length) => Ok(length),
        }
    }

    pub fn try_sort_key(
        &self,
        strength: Strength,
        text: &[u16],
        budget: &MemoryBudget,
    ) -> Result<BudgetedVec<u8>, CollationError> {
        self.try_sort_key_cancellable(strength, text, budget, &NEVER)
    }

    pub fn try_sort_key_cancellable(
        &self,
        strength: Strength,
        text: &[u16],
        budget: &MemoryBudget,
        cancel: &AtomicBool,
    ) -> Result<BudgetedVec<u8>, CollationError> {
        self.check_budget(budget)?;
        self.admit()?;
        let source = self.prepared(Self::source(text, budget))?;
        let collator = self.collator(strength);
        let mut probe = [0; SORT_KEY_SLACK + 1];
        let needed = match self.write(collator, &source, &mut probe, budget, cancel)? {
            SortKeyWrite::Complete(length) | SortKeyWrite::Required(length) => length,
        };
        let capacity = needed
            .checked_add(SORT_KEY_SLACK)
            .ok_or(BudgetError::Overflow)?;
        let mut output = self.prepared(
            BudgetedVec::try_with_capacity(budget, Category::Execution, capacity)
                .and_then(|mut output| output.try_resize(capacity, 0).map(|()| output)),
        )?;
        match self.write(collator, &source, &mut output, budget, cancel)? {
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

fn not_quarantined() -> Result<(), CollationError> {
    match quarantined() {
        Some(first) => Err(CollationError::Runtime(RuntimeError::RestartRequired(
            first,
        ))),
        None => Ok(()),
    }
}

fn prepared<T>(result: Result<T, BudgetError>) -> Result<T, CollationError> {
    result.map_err(|error| match quarantined() {
        Some(first) => CollationError::Runtime(RuntimeError::RestartRequired(first)),
        None => CollationError::Budget(error),
    })
}
