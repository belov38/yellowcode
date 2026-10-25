use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use std::ops::{Deref, DerefMut};

#[derive(Clone, Copy)]
pub enum Allocation<'a> {
    Native,
    Managed(&'a MemoryBudget),
}
#[derive(Debug)]
pub enum Owned<T> {
    Native(T),
    Managed(Charged<T>),
}
impl<T> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Native(value) => value,
            Self::Managed(value) => value,
        }
    }
}
impl<T> Owned<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Owned<U> {
        match self {
            Self::Native(value) => Owned::Native(f(value)),
            Self::Managed(value) => Owned::Managed(value.map(f)),
        }
    }
    pub fn zip<U>(self, other: Owned<U>) -> Result<Owned<(T, U)>, BudgetError> {
        match (self, other) {
            (Self::Native(first), Owned::Native(second)) => Ok(Owned::Native((first, second))),
            (Self::Managed(first), Owned::Managed(second)) => {
                Ok(Owned::Managed(first.try_zip(second)?))
            }
            _ => Err(BudgetError::Overflow),
        }
    }
    pub fn native(self) -> Result<T, BudgetError> {
        match self {
            Self::Native(value) => Ok(value),
            Self::Managed(_) => Err(BudgetError::Overflow),
        }
    }
    pub fn managed(self) -> Result<Charged<T>, BudgetError> {
        match self {
            Self::Managed(value) => Ok(value),
            Self::Native(_) => Err(BudgetError::Overflow),
        }
    }
}
impl<T> Owned<Vec<T>> {
    pub fn push(self, value: Owned<T>) -> Result<Self, BudgetError> {
        if self.len() == self.capacity() {
            return Err(BudgetError::Overflow);
        }
        Ok(self.zip(value)?.map(|(mut values, value)| {
            values.push(value);
            values
        }))
    }
}

pub enum Vector<T> {
    Native(Vec<T>),
    Managed(BudgetedVec<T>),
}
impl<T> Deref for Vector<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Native(value) => value,
            Self::Managed(value) => value,
        }
    }
}
impl<T> DerefMut for Vector<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Native(value) => value,
            Self::Managed(value) => value,
        }
    }
}
impl<T> Vector<T> {
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetError> {
        match self {
            Self::Native(value) => value
                .try_reserve(additional)
                .map_err(|_| BudgetError::AllocationFailed),
            Self::Managed(value) => value.try_reserve(additional),
        }
    }
    pub fn push(&mut self, value: T) -> Result<(), BudgetError> {
        self.reserve(1)?;
        match self {
            Self::Native(values) => values.push(value),
            Self::Managed(values) => return values.try_push(value),
        }
        Ok(())
    }
    pub fn insert(&mut self, index: usize, value: T) -> Result<(), BudgetError> {
        if index > self.len() {
            return Err(BudgetError::Overflow);
        }
        self.push(value)?;
        self.get_mut(index..)
            .ok_or(BudgetError::Overflow)?
            .rotate_right(1);
        Ok(())
    }
    pub fn pop(&mut self) -> Option<T> {
        match self {
            Self::Native(values) => values.pop(),
            Self::Managed(values) => values.pop(),
        }
    }
    pub fn truncate(&mut self, len: usize) {
        match self {
            Self::Native(values) => values.truncate(len),
            Self::Managed(values) => values.truncate(len),
        }
    }
    pub fn finish(self) -> Owned<Vec<T>> {
        match self {
            Self::Native(value) => Owned::Native(value),
            Self::Managed(value) => Owned::Managed(value.into_charged_vec()),
        }
    }
}
impl<T: Copy> Vector<T> {
    pub fn extend(&mut self, values: &[T]) -> Result<(), BudgetError> {
        self.reserve(values.len())?;
        match self {
            Self::Native(output) => output.extend_from_slice(values),
            Self::Managed(output) => output.try_extend_from_slice(values)?,
        }
        Ok(())
    }
}
impl Allocation<'_> {
    pub fn vector<T>(self, capacity: usize) -> Result<Vector<T>, BudgetError> {
        match self {
            Self::Native => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(capacity)
                    .map_err(|_| BudgetError::AllocationFailed)?;
                Ok(Vector::Native(values))
            }
            Self::Managed(budget) => Ok(Vector::Managed(BudgetedVec::try_with_capacity(
                budget,
                Category::Metadata,
                capacity,
            )?)),
        }
    }
    pub fn bytes(self, bytes: &[u8]) -> Result<Owned<Vec<u8>>, BudgetError> {
        let mut out = self.vector(bytes.len())?;
        out.extend(bytes)?;
        Ok(out.finish())
    }
    pub fn replace_text(
        self,
        input: &str,
        from: &str,
        to: &str,
    ) -> Result<Owned<String>, BudgetError> {
        if from.is_empty() {
            return Err(BudgetError::Overflow);
        }
        let count = input.matches(from).count();
        let removed = count.checked_mul(from.len()).ok_or(BudgetError::Overflow)?;
        let added = count.checked_mul(to.len()).ok_or(BudgetError::Overflow)?;
        let len = input
            .len()
            .checked_sub(removed)
            .and_then(|len| len.checked_add(added))
            .ok_or(BudgetError::Overflow)?;
        let mut bytes = self.vector(len)?;
        let mut start = 0;
        for (at, _) in input.match_indices(from) {
            bytes.extend(
                input
                    .as_bytes()
                    .get(start..at)
                    .ok_or(BudgetError::Overflow)?,
            )?;
            bytes.extend(to.as_bytes())?;
            start = at.checked_add(from.len()).ok_or(BudgetError::Overflow)?;
        }
        bytes.extend(input.as_bytes().get(start..).ok_or(BudgetError::Overflow)?)?;
        bytes.finish().map(String::from_utf8).try_string()
    }
    pub fn text(self, parts: &[&str]) -> Result<Owned<String>, BudgetError> {
        let len = parts.iter().try_fold(0usize, |len, text| {
            len.checked_add(text.len()).ok_or(BudgetError::Overflow)
        })?;
        let mut out = self.vector(len)?;
        for part in parts {
            out.extend(part.as_bytes())?;
        }
        out.finish().map(String::from_utf8).try_string()
    }
}
impl Owned<Result<String, std::string::FromUtf8Error>> {
    pub fn try_string(self) -> Result<Owned<String>, BudgetError> {
        match self {
            Self::Native(value) => value.map(Owned::Native).map_err(|_| BudgetError::Overflow),
            Self::Managed(value) => value
                .try_map(|value| value)
                .map(Owned::Managed)
                .map_err(|_| BudgetError::Overflow),
        }
    }
}
