use crate::{Category, LimitReason, MemoryBudget, MemoryLimit, Reservation};
use std::alloc::{Layout, alloc};
use std::fmt;
use std::ops::{Deref, DerefMut};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetError {
    Limit(MemoryLimit),
    AllocationFailed,
    Overflow,
}
impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(f),
            Self::AllocationFailed => f.write_str("MEMORY_ALLOCATION_FAILED"),
            Self::Overflow => f.write_str("MEMORY_SIZE_OVERFLOW"),
        }
    }
}
impl std::error::Error for BudgetError {}
impl From<MemoryLimit> for BudgetError {
    fn from(error: MemoryLimit) -> Self {
        Self::Limit(error)
    }
}

#[derive(Debug)]
pub struct BudgetedVec<T> {
    values: Vec<T>,
    charge: Reservation,
}

impl<T> BudgetedVec<T> {
    pub fn new(budget: &MemoryBudget, category: Category) -> Result<Self, BudgetError> {
        Self::try_with_capacity(budget, category, 0)
    }
    pub fn try_with_capacity(
        budget: &MemoryBudget,
        category: Category,
        capacity: usize,
    ) -> Result<Self, BudgetError> {
        let bytes = capacity_bytes::<T>(capacity)?;
        let total = bytes
            .checked_add(std::mem::size_of::<Self>() as u64)
            .ok_or(BudgetError::Overflow)?;
        let charge = budget.try_reserve(category, total)?;
        let values = unsafe { allocate_vec(capacity, alloc) }?;
        Ok(Self { values, charge })
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }
    pub fn as_slice(&self) -> &[T] {
        &self.values
    }
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.values
    }
    pub fn clear(&mut self) {
        self.values.clear();
    }
    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }
    pub fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), BudgetError> {
        let required = self
            .len()
            .checked_add(additional)
            .ok_or(BudgetError::Overflow)?;
        if required <= self.capacity() {
            return Ok(());
        }
        let preferred = self
            .capacity()
            .checked_mul(2)
            .unwrap_or(required)
            .max(required);
        match self.reallocate(preferred) {
            Err(BudgetError::Limit(_)) if preferred != required => self.reallocate(required),
            result => result,
        }
    }
    pub fn reservation(&self) -> &Reservation {
        &self.charge
    }
    pub fn into_charged_vec(self) -> crate::Charged<Vec<T>> {
        crate::Charged {
            value: self.values,
            reservation: self.charge,
        }
    }
    pub fn try_into_shared(self) -> Result<crate::Shared<Vec<T>>, BudgetError> {
        let Self { values, charge } = self;
        crate::Shared::try_new(values, charge)
    }
    pub fn try_resize(&mut self, len: usize, value: T) -> Result<(), BudgetError>
    where
        T: Copy,
    {
        if len > self.len() {
            self.try_reserve(len - self.len())?;
        }
        self.values.resize(len, value);
        Ok(())
    }
    pub fn try_push(&mut self, value: T) -> Result<(), BudgetError> {
        self.try_reserve(1)?;
        self.values.push(value);
        Ok(())
    }
    pub fn try_extend_from_slice(&mut self, values: &[T]) -> Result<(), BudgetError>
    where
        T: Copy,
    {
        self.try_reserve(values.len())?;
        self.values.extend_from_slice(values);
        Ok(())
    }
    fn reallocate(&mut self, capacity: usize) -> Result<(), BudgetError> {
        unsafe { self.reallocate_with(capacity, alloc) }
    }
    unsafe fn reallocate_with(
        &mut self,
        capacity: usize,
        allocate: unsafe fn(Layout) -> *mut u8,
    ) -> Result<(), BudgetError> {
        let bytes = capacity_bytes::<T>(capacity)?;
        let backing = self
            .charge
            .budget()
            .try_reserve(self.charge.category(), bytes)?;
        let mut values = unsafe { allocate_vec(capacity, allocate) }?;
        values.append(&mut self.values);
        let old = std::mem::replace(&mut self.values, values);
        let old_bytes = capacity_bytes::<T>(old.capacity())?;
        drop(old);
        self.charge.shrink(old_bytes)?;
        self.charge.merge_same(backing);
        Ok(())
    }
}
impl<T> Deref for BudgetedVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}
impl<T> DerefMut for BudgetedVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

pub(crate) fn capacity_bytes<T>(capacity: usize) -> Result<u64, BudgetError> {
    let layout = Layout::array::<T>(capacity).map_err(|_| BudgetError::Overflow)?;
    u64::try_from(layout.size()).map_err(|_| BudgetError::Overflow)
}

pub(crate) unsafe fn allocate_vec<T>(
    capacity: usize,
    allocate: unsafe fn(Layout) -> *mut u8,
) -> Result<Vec<T>, BudgetError> {
    let layout = Layout::array::<T>(capacity).map_err(|_| BudgetError::Overflow)?;
    if layout.size() == 0 {
        return Ok(Vec::new());
    }
    let pointer = unsafe { allocate(layout) }.cast::<T>();
    if pointer.is_null() {
        return Err(BudgetError::AllocationFailed);
    }
    Ok(unsafe { Vec::from_raw_parts(pointer, 0, capacity) })
}

#[derive(Debug)]
pub struct BudgetedText {
    buffer: BudgetedVec<u8>,
    limit: u64,
    error: Option<BudgetError>,
}
impl BudgetedText {
    pub fn new(budget: &MemoryBudget, category: Category, limit: u64) -> Result<Self, BudgetError> {
        Self::try_with_capacity(budget, category, limit, 0)
    }
    pub fn try_with_capacity(
        budget: &MemoryBudget,
        category: Category,
        limit: u64,
        capacity: usize,
    ) -> Result<Self, BudgetError> {
        let requested = u64::try_from(capacity).map_err(|_| BudgetError::Overflow)?;
        if requested > limit {
            return Err(BudgetError::Limit(MemoryLimit {
                category,
                requested,
                reason: LimitReason::Category,
            }));
        }
        let buffer = BudgetedVec::try_with_capacity(budget, category, capacity)?;
        buffer
            .charge
            .try_grow((std::mem::size_of::<Self>() - std::mem::size_of_val(&buffer)) as u64)?;
        Ok(Self {
            buffer,
            limit,
            error: None,
        })
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        self.buffer.reservation().budget()
    }
    pub fn as_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(self.buffer.as_slice()) }
    }
    pub fn error(&self) -> Option<&BudgetError> {
        self.error.as_ref()
    }
    pub fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = (|| {
            let len = self
                .buffer
                .len()
                .checked_add(text.len())
                .ok_or(BudgetError::Overflow)?;
            let len = u64::try_from(len).map_err(|_| BudgetError::Overflow)?;
            if len > self.limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category: self.buffer.charge.category(),
                    requested: len,
                    reason: LimitReason::Category,
                }));
            }
            self.buffer.try_extend_from_slice(text.as_bytes())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }
    pub fn try_push(&mut self, value: char) -> Result<(), BudgetError> {
        self.try_push_str(value.encode_utf8(&mut [0; 4]))
    }
    pub fn into_string(self) -> Result<crate::Charged<String>, BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.buffer.charge.shrink(
            (std::mem::size_of::<Self>() - std::mem::size_of::<crate::Charged<String>>()) as u64,
        )?;
        Ok(self
            .buffer
            .into_charged_vec()
            .map(|bytes| unsafe { String::from_utf8_unchecked(bytes) }))
    }
}
impl fmt::Write for BudgetedText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.try_push_str(text).map_err(|_| fmt::Error)
    }
    fn write_char(&mut self, value: char) -> fmt::Result {
        self.try_push(value).map_err(|_| fmt::Error)
    }
}

#[derive(Debug)]
pub struct BudgetedWriter {
    buffer: BudgetedVec<u8>,
    wire_limit: u64,
    error: Option<BudgetError>,
}
impl BudgetedWriter {
    pub fn new(budget: &MemoryBudget, wire_limit: u64) -> Result<Self, BudgetError> {
        let buffer = BudgetedVec::new(budget, Category::Response)?;
        buffer
            .charge
            .try_grow((std::mem::size_of::<Self>() - std::mem::size_of_val(&buffer)) as u64)?;
        Ok(Self {
            buffer,
            wire_limit,
            error: None,
        })
    }
    pub fn serialization_sink(&mut self) -> SerializationSink<'_> {
        SerializationSink(self)
    }
    pub fn as_slice(&self) -> &[u8] {
        self.buffer.as_slice()
    }
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
    pub fn error(&self) -> Option<&BudgetError> {
        self.error.as_ref()
    }
    pub fn into_buffer(self) -> BudgetedVec<u8> {
        self.buffer
    }
    pub fn try_write(&mut self, bytes: &[u8]) -> Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = (|| {
            let len = self
                .len()
                .checked_add(bytes.len())
                .ok_or(BudgetError::Overflow)?;
            let len = u64::try_from(len).map_err(|_| BudgetError::Overflow)?;
            if len > self.wire_limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category: Category::Response,
                    requested: len,
                    reason: LimitReason::Category,
                }));
            }
            self.buffer.try_extend_from_slice(bytes)
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }
}
pub struct SerializationSink<'a>(&'a mut BudgetedWriter);
impl std::io::Write for SerializationSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.try_write(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Write for BudgetedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.try_write(bytes)
            .map(|()| bytes.len())
            .map_err(|_| std::io::ErrorKind::OutOfMemory.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
