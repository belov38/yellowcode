use crate::buffer::{allocate_vec, capacity_bytes};
use crate::{
    BudgetError, BudgetedVec, Category, Charged, LimitReason, MemoryBudget, MemoryLimit,
    Reservation,
};
use std::alloc::alloc;
use std::marker::PhantomData;

#[derive(Debug)]
pub struct Grant {
    category: Category,
    granted: u64,
    issued: u64,
    remaining: u64,
    not_send: PhantomData<*mut ()>,
}

impl Grant {
    fn new(category: Category, granted: u64) -> Self {
        Self {
            category,
            granted,
            issued: 0,
            remaining: granted,
            not_send: PhantomData,
        }
    }
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
    pub fn try_vec_unowned<T>(&mut self, capacity: usize) -> Result<Vec<T>, BudgetError> {
        let bytes = capacity_bytes::<T>(capacity)?
            .checked_add(std::mem::size_of::<BudgetedVec<T>>() as u64)
            .ok_or(BudgetError::Overflow)?;
        if bytes > self.remaining {
            return Err(BudgetError::Limit(MemoryLimit {
                category: self.category,
                requested: bytes,
                reason: LimitReason::InvalidSize,
            }));
        }
        let values = unsafe { allocate_vec(capacity, alloc) }?;
        self.remaining -= bytes;
        self.issued += bytes;
        Ok(values)
    }
    fn check(&self) {
        debug_assert_eq!(self.issued + self.remaining, self.granted);
    }
}

pub fn extend_within<T: Copy>(vec: &mut Vec<T>, values: &[T]) -> Result<(), BudgetError> {
    if vec.capacity() - vec.len() < values.len() {
        return Err(BudgetError::Overflow);
    }
    vec.extend_from_slice(values);
    Ok(())
}

pub fn resize_within<T: Copy>(vec: &mut Vec<T>, len: usize, value: T) -> Result<(), BudgetError> {
    if len > vec.capacity() {
        return Err(BudgetError::Overflow);
    }
    vec.resize(len, value);
    Ok(())
}

impl MemoryBudget {
    pub fn try_prepaid<T, E: From<BudgetError>>(
        &self,
        category: Category,
        bytes: u64,
        build: impl FnOnce(&mut Grant) -> Result<T, E>,
    ) -> Result<Charged<T>, E> {
        let reservation = self
            .try_reserve(category, bytes)
            .map_err(|e| E::from(BudgetError::Limit(e)))?;
        let mut grant = Grant::new(category, bytes);
        let value = build(&mut grant)?;
        grant.check();
        if grant.remaining != 0 {
            reservation
                .shrink(grant.remaining)
                .map_err(|e| E::from(BudgetError::Limit(e)))?;
        }
        Ok(Charged { value, reservation })
    }
}

#[derive(Debug)]
pub struct Scratch<T> {
    value: Option<T>,
    filled: u64,
    lease: Reservation,
}

impl<T> Scratch<T> {
    pub fn try_new(budget: &MemoryBudget, category: Category) -> Result<Self, MemoryLimit> {
        Ok(Self {
            value: None,
            filled: 0,
            lease: budget.try_reserve(category, 0)?,
        })
    }
    pub fn refill<E: From<BudgetError>>(
        &mut self,
        bytes: u64,
        build: impl FnOnce(&mut Grant) -> Result<T, E>,
    ) -> Result<&T, E> {
        self.value = None;
        self.filled = 0;
        let held = self.lease.bytes();
        if bytes > held {
            self.lease
                .try_grow(bytes - held)
                .map_err(|e| E::from(BudgetError::Limit(e)))?;
        }
        let mut grant = Grant::new(self.lease.category(), bytes);
        let value = build(&mut grant)?;
        grant.check();
        self.filled = bytes;
        Ok(self.value.insert(value))
    }
    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }
    pub fn clear(&mut self) {
        self.value = None;
        self.filled = 0;
    }
    pub fn trim(&self) {
        let extra = self.lease.bytes().saturating_sub(self.filled);
        if extra > 0 {
            let _ = self.lease.shrink(extra);
        }
    }
    pub fn held(&self) -> u64 {
        self.lease.bytes()
    }
}
