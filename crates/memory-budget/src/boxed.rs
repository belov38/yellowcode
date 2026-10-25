use crate::{BudgetError, Category, MemoryBudget, Reservation};
use std::alloc::{Layout, alloc};
use std::ops::{Deref, DerefMut};

#[derive(Debug)]
pub struct BudgetedBox<T: ?Sized> {
    value: Box<T>,
    charge: Reservation,
}
impl<T> BudgetedBox<T> {
    pub fn try_new(
        budget: &MemoryBudget,
        category: Category,
        value: T,
    ) -> Result<Self, BudgetError> {
        let layout = Layout::new::<T>();
        let bytes = layout
            .size()
            .checked_add(std::mem::size_of::<Self>())
            .ok_or(BudgetError::Overflow)?;
        let charge = budget.try_reserve(
            category,
            u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
        )?;
        if layout.size() == 0 {
            return Ok(Self {
                value: Box::new(value),
                charge,
            });
        }
        let pointer = unsafe { alloc(layout) }.cast::<T>();
        if pointer.is_null() {
            drop(value);
            drop(charge);
            return Err(BudgetError::AllocationFailed);
        }
        let value = unsafe {
            pointer.write(value);
            Box::from_raw(pointer)
        };
        Ok(Self { value, charge })
    }
}
impl<T: ?Sized> BudgetedBox<T> {
    pub fn try_map_box<U: ?Sized>(
        self,
        convert: impl FnOnce(Box<T>) -> Box<U>,
    ) -> Result<BudgetedBox<U>, BudgetError> {
        let old = std::mem::size_of::<Self>();
        let new = std::mem::size_of::<BudgetedBox<U>>();
        if new > old {
            self.charge.try_grow((new - old) as u64)?;
        }
        let Self { value, charge } = self;
        let value = convert(value);
        if old > new {
            charge.shrink((old - new) as u64)?;
        }
        Ok(BudgetedBox { value, charge })
    }
    pub fn reservation(&self) -> &Reservation {
        &self.charge
    }
}
impl<T: ?Sized> Deref for BudgetedBox<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T: ?Sized> DerefMut for BudgetedBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
