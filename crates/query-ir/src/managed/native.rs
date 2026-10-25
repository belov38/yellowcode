use super::{AResult, CompileArena, ManagedClone};
use memory_budget::BudgetError;
use std::mem::ManuallyDrop;

pub(crate) struct NativeOwned<'a, T> {
    value: ManuallyDrop<T>,
    scope: Option<&'a CompileArena>,
    bytes: u64,
    depth: u16,
}
impl<'a> NativeOwned<'a, crate::tir::TExpr> {
    pub(super) fn into_expr_parts(self) -> (crate::tir::TExpr, Option<&'a CompileArena>, u64, u16) {
        let scope = self.scope;
        let bytes = self.bytes;
        let depth = self.depth;
        (self.into_native(), scope, bytes, depth)
    }
}
impl<T> NativeOwned<'_, T> {
    pub(super) fn native_bytes(&self) -> u64 {
        self.bytes
    }
}
impl<T> NativeOwned<'_, T> {
    pub(crate) fn into_native(self) -> T {
        let mut owner = ManuallyDrop::new(self);
        unsafe { ManuallyDrop::take(&mut owner.value) }
    }
}
impl<T> NativeOwned<'_, T> {
    pub(crate) fn trusted_borrow(&self) -> &T {
        &self.value
    }
}

pub(crate) struct NativeVec<'a, T> {
    owner: NativeOwned<'a, Vec<T>>,
}
impl<'a, T> NativeVec<'a, T> {
    pub(crate) fn new(scope: Option<&'a CompileArena>, capacity: usize) -> crate::sema::R<Self> {
        let value = super::vector(scope, capacity)?;
        let bytes = if scope.is_some() {
            (value.capacity() * std::mem::size_of::<T>()) as u64
        } else {
            0
        };
        Ok(Self {
            owner: NativeOwned {
                value: ManuallyDrop::new(value),
                scope,
                bytes,
                depth: 0,
            },
        })
    }
    fn ensure_one(&mut self) -> crate::sema::R<()> {
        let scope = self.owner.scope;
        let old_capacity = self.owner.value.capacity();
        let old_bytes = old_capacity * std::mem::size_of::<T>();
        super::reserve_additional(scope, &mut self.owner.value, 1)?;
        if scope.is_some() {
            let new_bytes = self.owner.value.capacity() * std::mem::size_of::<T>();
            self.owner.bytes = self.owner.bytes - old_bytes as u64 + new_bytes as u64;
        }
        Ok(())
    }
    pub(crate) fn push_owned(&mut self, value: NativeOwned<'a, T>) -> crate::sema::R<()> {
        let same_domain = match (self.owner.scope, value.scope) {
            (Some(left), Some(right)) => std::ptr::eq(left, right),
            (None, None) => true,
            _ => false,
        };
        if !same_domain {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        self.ensure_one()?;
        let Some(bytes) = self.owner.bytes.checked_add(value.bytes) else {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        };
        self.owner.bytes = bytes;
        self.owner.depth = self.owner.depth.max(value.depth);
        self.owner.value.push(value.into_native());
        Ok(())
    }
    pub(crate) fn finish(self) -> NativeOwned<'a, Vec<T>> {
        self.owner
    }
}
impl<'a, T> NativeVec<'a, T> {
    pub(super) fn clone_empty(scope: &'a CompileArena, capacity: usize) -> AResult<Self> {
        let value = scope.vector::<T>(capacity)?;
        let bytes = (value.capacity() * std::mem::size_of::<T>()) as u64;
        Ok(Self {
            owner: NativeOwned::leaf(value, Some(scope), bytes),
        })
    }
    pub(super) fn clone_push(&mut self, value: NativeOwned<'a, T>) -> AResult<()> {
        if !matches!((self.owner.scope, value.scope), (Some(a), Some(b)) if std::ptr::eq(a,b)) {
            return Err(BudgetError::Overflow);
        }
        let scope = self.owner.scope.ok_or(BudgetError::Overflow)?;
        let old = (self.owner.value.capacity() * std::mem::size_of::<T>()) as u64;
        let required = self
            .owner
            .value
            .len()
            .checked_add(1)
            .ok_or(BudgetError::Overflow)?;
        scope.ensure_capacity(&mut self.owner.value, required)?;
        let new = (self.owner.value.capacity() * std::mem::size_of::<T>()) as u64;
        self.owner.bytes = self.owner.bytes - old + new;
        self.owner.bytes = self
            .owner
            .bytes
            .checked_add(value.bytes)
            .ok_or(BudgetError::Overflow)?;
        self.owner.depth = self.owner.depth.max(value.depth);
        self.owner.value.push(value.into_native());
        Ok(())
    }
}
impl<T: Copy> NativeVec<'_, T> {
    pub(crate) fn push_copy(&mut self, value: T) -> crate::sema::R<()> {
        self.ensure_one()?;
        self.owner.value.push(value);
        Ok(())
    }
}
struct Release<'a> {
    scope: Option<&'a CompileArena>,
    bytes: u64,
}
impl Drop for Release<'_> {
    fn drop(&mut self) {
        if let Some(scope) = self.scope {
            let _ = scope.charge.shrink(self.bytes);
        }
    }
}
impl<T> Drop for NativeOwned<'_, T> {
    fn drop(&mut self) {
        let release = Release {
            scope: self.scope,
            bytes: self.bytes,
        };
        unsafe { ManuallyDrop::drop(&mut self.value) };
        drop(release);
    }
}

pub(crate) fn owned_copy<'a, T: ManagedClone + Clone>(
    scope: Option<&'a CompileArena>,
    value: &T,
) -> crate::sema::R<NativeOwned<'a, T>> {
    match scope {
        Some(scope) => {
            let depth = scope.compile(value.graph_depth(128))?;
            scope
                .compile(value.clone_owned(scope))
                .map(|value| value.set_depth(depth))
        }
        None => Ok(NativeOwned::leaf(value.clone(), None, 0)),
    }
}

impl<'a, T> NativeOwned<'a, T> {
    pub(super) fn leaf(value: T, scope: Option<&'a CompileArena>, bytes: u64) -> Self {
        Self {
            value: ManuallyDrop::new(value),
            scope,
            bytes,
            depth: 0,
        }
    }
    pub(crate) fn map<U>(self, convert: impl FnOnce(T) -> U) -> NativeOwned<'a, U> {
        let release = Release {
            scope: self.scope,
            bytes: self.bytes,
        };
        let scope = self.scope;
        let bytes = self.bytes;
        let depth = self.depth;
        let value = convert(self.into_native());
        let mut output = NativeOwned::leaf(value, scope, bytes);
        output.depth = depth;
        std::mem::forget(release);
        output
    }
    pub(super) fn try_map<U>(
        self,
        convert: impl FnOnce(T) -> AResult<U>,
    ) -> AResult<NativeOwned<'a, U>> {
        let release = Release {
            scope: self.scope,
            bytes: self.bytes,
        };
        let scope = self.scope;
        let bytes = self.bytes;
        let depth = self.depth;
        let value = convert(self.into_native())?;
        let mut output = NativeOwned::leaf(value, scope, bytes);
        output.depth = depth;
        std::mem::forget(release);
        Ok(output)
    }
    pub(crate) fn zip<U>(self, other: NativeOwned<'a, U>) -> AResult<NativeOwned<'a, (T, U)>> {
        let same = match (self.scope, other.scope) {
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return Err(BudgetError::Overflow);
        }
        let bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or(BudgetError::Overflow)?;
        let scope = self.scope;
        let depth = self.depth.max(other.depth);
        let mut value = NativeOwned::leaf((self.into_native(), other.into_native()), scope, bytes);
        value.depth = depth;
        Ok(value)
    }
    pub(crate) fn boxed(self) -> AResult<NativeOwned<'a, Box<T>>> {
        let scope = self.scope;
        let bytes = self
            .bytes
            .checked_add(if scope.is_some() {
                std::mem::size_of::<T>() as u64
            } else {
                0
            })
            .ok_or(BudgetError::Overflow)?;
        let mut result = self.try_map(|value| match scope {
            Some(scope) => scope.boxed(value),
            None => Ok(Box::new(value)),
        })?;
        result.bytes = bytes;
        Ok(result)
    }
    pub(super) fn into_durable(self) -> AResult<DurableOwned<T>> {
        let scope = self.scope.ok_or(BudgetError::Overflow)?;
        let charge = scope.charge.split(self.bytes)?;
        Ok(DurableOwned {
            value: ManuallyDrop::new(self.into_native()),
            charge: ManuallyDrop::new(charge),
            thread_confined: std::marker::PhantomData,
        })
    }
}

pub(crate) trait SharedPayload {}
impl SharedPayload for crate::temp::TempSchema {}
impl SharedPayload for crate::NestedResult {}

#[allow(dead_code)]
pub(super) struct DurableOwned<T> {
    value: ManuallyDrop<T>,
    charge: ManuallyDrop<memory_budget::Reservation>,
    thread_confined: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[allow(dead_code)]
impl<T> DurableOwned<T> {
    pub(super) fn trusted_borrow(&self) -> &T {
        &self.value
    }
    pub(super) fn into_shared(self) -> AResult<crate::QShared<T>>
    where
        T: SharedPayload,
    {
        let mut owner = ManuallyDrop::new(self);
        let value = unsafe { ManuallyDrop::take(&mut owner.value) };
        let charge = unsafe { ManuallyDrop::take(&mut owner.charge) };
        memory_budget::Shared::try_new(value, charge).map(crate::QShared::from_managed)
    }
}
impl<T> Drop for DurableOwned<T> {
    fn drop(&mut self) {
        struct Release<'a>(&'a mut ManuallyDrop<memory_budget::Reservation>);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                unsafe { ManuallyDrop::drop(self.0) };
            }
        }
        let release = Release(&mut self.charge);
        unsafe { ManuallyDrop::drop(&mut self.value) };
        drop(release);
    }
}

impl<'a, T> NativeOwned<'a, T> {
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(super) fn set_depth(mut self, depth: u16) -> Self {
        self.depth = depth;
        self
    }
    pub(crate) fn scope_matches(&self, scope: Option<&CompileArena>) -> bool {
        match (self.scope, scope) {
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
    pub(crate) fn into_some(self) -> NativeOwned<'a, Option<T>> {
        self.map(Some)
    }
}
impl<'a, T: Copy> NativeOwned<'a, T> {
    pub(crate) fn inline(scope: Option<&'a CompileArena>, value: T) -> Self {
        Self::leaf(value, scope, 0)
    }
}
impl<'a, T> NativeOwned<'a, Option<T>> {
    pub(crate) fn none(scope: Option<&'a CompileArena>) -> Self {
        Self::leaf(None, scope, 0)
    }
}
impl<T> NativeVec<'_, T> {
    pub(crate) fn items(&self) -> &[T] {
        self.owner.trusted_borrow()
    }
}
impl<'a> NativeVec<'a, crate::tir::Select> {
    pub(crate) fn install_sources(
        &mut self,
        index: usize,
        sources: NativeOwned<'a, Vec<crate::tir::Source>>,
    ) -> crate::sema::R<()> {
        if !sources.scope_matches(self.owner.scope) {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let Some(slot) = self.owner.value.get_mut(index) else {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        };
        if slot.sources.capacity() != 0 {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let bytes = super::allocation_result(
            self.owner.scope,
            self.owner
                .bytes
                .checked_add(sources.bytes)
                .ok_or(BudgetError::Overflow),
        )?;
        self.owner.depth = self.owner.depth.max(sources.depth);
        slot.sources = sources.into_native();
        self.owner.bytes = bytes;
        Ok(())
    }
    pub(crate) fn install_indexes(
        &mut self,
        indexes: NativeOwned<'a, Vec<Vec<usize>>>,
    ) -> crate::sema::R<()> {
        if !indexes.scope_matches(self.owner.scope) {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let Some(slot) = self.owner.value.last_mut() else {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        };
        if slot.index_by.capacity() != 0 {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let bytes = super::allocation_result(
            self.owner.scope,
            self.owner
                .bytes
                .checked_add(indexes.bytes)
                .ok_or(BudgetError::Overflow),
        )?;
        slot.index_by = indexes.into_native();
        self.owner.bytes = bytes;
        Ok(())
    }
    pub(crate) fn install_index_unique(
        &mut self,
        unique: NativeOwned<'a, Vec<bool>>,
    ) -> crate::sema::R<()> {
        if !unique.scope_matches(self.owner.scope) {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let Some(slot) = self.owner.value.last_mut() else {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        };
        if slot.index_unique.capacity() != 0 {
            return super::allocation_error(self.owner.scope, BudgetError::Overflow);
        }
        let bytes = super::allocation_result(
            self.owner.scope,
            self.owner
                .bytes
                .checked_add(unique.bytes)
                .ok_or(BudgetError::Overflow),
        )?;
        slot.index_unique = unique.into_native();
        self.owner.bytes = bytes;
        Ok(())
    }
}
