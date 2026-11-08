use super::{ExecEnv, ExecError, LedgerWork, MemBudget, prepaid_vec, reserve_backing};
use std::ops::{Deref, DerefMut};

pub(super) struct BuilderVec<'m, 'a, T> {
    values: Vec<T>,
    mem: &'m MemBudget<'a>,
    bytes: u64,
    #[cfg(test)]
    owner: usize,
    #[cfg(test)]
    generation: usize,
}
impl<'m, 'a, T> BuilderVec<'m, 'a, T> {
    pub(super) fn new(
        capacity: usize,
        mem: &'m MemBudget<'a>,
        env: &ExecEnv<'_>,
    ) -> Result<Self, ExecError> {
        let bytes = crate::model::size_vec(capacity, std::mem::size_of::<T>())?;
        let values = prepaid_vec(capacity, mem, env)?;
        let result = Self {
            values,
            mem,
            bytes,
            #[cfg(test)]
            owner,
            #[cfg(test)]
            generation: 1,
        };
        #[cfg(test)]
        super::builder_tests::built(owner, 1, &result.values, result.values.capacity());
        Ok(result)
    }
    pub(super) fn reserve(
        &mut self,
        additional: usize,
        env: &ExecEnv<'_>,
    ) -> Result<(), ExecError> {
        let required = self
            .values
            .len()
            .checked_add(additional)
            .ok_or_else(crate::model::invalid_size)?;
        if required <= self.values.capacity() {
            return Ok(());
        }
        reserve_backing(&mut self.values, additional, self.mem, env)?;
        self.bytes = crate::model::vec_block(self.values.capacity(), std::mem::size_of::<T>());
        #[cfg(test)]
        {
            self.generation = next_generation;
            super::builder_tests::built(
                self.owner,
                self.generation,
                &self.values,
                self.values.capacity(),
            );
        }
        Ok(())
    }
    pub(super) fn push(&mut self, value: T, env: &ExecEnv<'_>) -> Result<(), ExecError> {
        self.reserve(1, env)?;
        self.values.push(value);
        Ok(())
    }
    pub(super) fn copy_from(
        values: &[T],
        mem: &'m MemBudget<'a>,
        env: &ExecEnv<'_>,
    ) -> Result<Self, ExecError>
    where
        T: Copy,
    {
        let mut out = Self::new(values.len(), mem, env)?;
        out.extend_copy(values, env)?;
        Ok(out)
    }
    pub(super) fn extend_copy(&mut self, values: &[T], env: &ExecEnv<'_>) -> Result<(), ExecError>
    where
        T: Copy,
    {
        self.reserve(values.len(), env)?;
        let mut work = LedgerWork::new(env);
        for &value in values {
            work.step()?;
            self.values.push(value);
        }
        work.check()
    }
    pub(super) fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }
    pub(super) fn clear(&mut self) {
        self.values.clear();
    }
    pub(super) fn as_slice(&self) -> &[T] {
        &self.values
    }
    fn destroy(&mut self) {
        drop(std::mem::take(&mut self.values));
        self.mem.shrink(std::mem::take(&mut self.bytes));
    }
}
impl<T> Deref for BuilderVec<'_, '_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.values
    }
}
impl<T> DerefMut for BuilderVec<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}
impl<T> Drop for BuilderVec<'_, '_, T> {
    fn drop(&mut self) {
        self.destroy();
    }
}
impl<'s, T> IntoIterator for &'s BuilderVec<'_, '_, T> {
    type Item = &'s T;
    type IntoIter = std::slice::Iter<'s, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}
pub(super) struct OwnedIter<'m, 'a, T> {
    values: Option<std::vec::IntoIter<T>>,
    mem: &'m MemBudget<'a>,
    bytes: u64,
    #[cfg(test)]
    owner: usize,
    #[cfg(test)]
    generation: usize,
}
impl<'m, 'a, T> IntoIterator for BuilderVec<'m, 'a, T> {
    type Item = T;
    type IntoIter = OwnedIter<'m, 'a, T>;
    fn into_iter(mut self) -> Self::IntoIter {
        let values = std::mem::take(&mut self.values).into_iter();
        let bytes = std::mem::take(&mut self.bytes);
        OwnedIter {
            values: Some(values),
            mem: self.mem,
            bytes,
            #[cfg(test)]
            owner: self.owner,
            #[cfg(test)]
            generation: self.generation,
        }
    }
}
impl<T> Iterator for OwnedIter<'_, '_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.as_mut().and_then(Iterator::next)
    }
}
impl<T> Drop for OwnedIter<'_, '_, T> {
    fn drop(&mut self) {
        drop(self.values.take());
        self.mem.shrink(std::mem::take(&mut self.bytes));
    }
}

pub(super) struct BuilderKeyMap<'m, 'a, V> {
    values: std::collections::HashMap<crate::key::Key, V>,
    model: crate::ops::hier::HierarchyHash<(crate::key::Key, V)>,
    mem: &'m MemBudget<'a>,
    balance: u64,
    #[cfg(test)]
    pub(super) owner: usize,
}
impl<'m, 'a, V> BuilderKeyMap<'m, 'a, V> {
    pub(super) fn new(mem: &'m MemBudget<'a>) -> Self {
        #[cfg(test)]
        {
            let _scope = super::builder_tests::scope(owner, 1);
            super::builder_tests::hash_new::<(crate::key::Key, V)>();
        }
        Self {
            values: std::collections::HashMap::new(),
            model: crate::ops::hier::HierarchyHash::new(),
            mem,
            balance: 0,
            #[cfg(test)]
            owner,
        }
    }
    pub(super) fn get(&self, key: &crate::key::Key) -> Option<&V> {
        self.values.get(key)
    }
    pub(super) fn insert(
        &mut self,
        key: crate::ops::hier::TotalsKey<'m, 'a>,
        value: V,
    ) -> Result<(), ExecError> {
        let prepared = self.model.prepare(self.mem, &mut self.balance)?;
        #[cfg(test)]
        super::builder_tests::hash_prepared(prepared.0);
        let next = crate::model::size_add(self.balance, key.bytes())?;
        let key = key.transfer(&mut self.balance, next);
        self.values.insert(key, value);
        #[cfg(test)]
        super::builder_tests::key_adopted(observed.0, observed.1);
        self.model.inserted(
            prepared,
            self.values.capacity(),
            self.mem,
            &mut self.balance,
        )
    }
}
impl<V> Drop for BuilderKeyMap<'_, '_, V> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.values));
        self.mem.shrink(std::mem::take(&mut self.balance));
    }
}

pub(super) struct BuilderOrderKeys<'m, 'a> {
    values: BuilderVec<'m, 'a, crate::key::Key>,
    mem: &'m MemBudget<'a>,
    key_bytes: u64,
    #[cfg(test)]
    owner: usize,
}
impl<'m, 'a> BuilderOrderKeys<'m, 'a> {
    pub(super) fn new(
        capacity: usize,
        mem: &'m MemBudget<'a>,
        env: &ExecEnv<'_>,
    ) -> Result<Self, ExecError> {
        Ok(Self {
            values: BuilderVec::new(capacity, mem, env)?,
            mem,
            key_bytes: 0,
            #[cfg(test)]
            owner: super::builder_tests::new_owner(),
        })
    }
    pub(super) fn push(
        &mut self,
        value: &query_ir::QValue,
        coll: crate::coll::ExecCollation<'_>,
        env: &ExecEnv<'_>,
    ) -> Result<(), ExecError> {
        let mut key = crate::ops::hier::TotalsKey::new(value, self.mem, coll, Some(env))?;
        key.retain()?;
        self.values.reserve(1, env)?;
        let next = crate::model::size_add(self.key_bytes, key.bytes())?;
        let key = key.transfer(&mut self.key_bytes, next);
        self.values.values.push(key);
        #[cfg(test)]
        super::builder_tests::key_adopted(observed.0, observed.1);
        Ok(())
    }
}
impl Deref for BuilderOrderKeys<'_, '_> {
    type Target = [crate::key::Key];
    fn deref(&self) -> &Self::Target {
        self.values.as_slice()
    }
}
impl Drop for BuilderOrderKeys<'_, '_> {
    fn drop(&mut self) {
        self.values.destroy();
        self.mem.shrink(std::mem::take(&mut self.key_bytes));
    }
}

pub(super) fn stable_sort_scratch<T>(len: usize) -> Result<u64, ExecError> {
    let size = std::mem::size_of::<T>();
    if size == 0 || len < 2 {
        return Ok(0);
    }
    let drift = if len <= 20 {
        0
    } else {
        (len - len / 2).max(len.min(8_000_000 / size)).max(48)
    };
    let capacity = drift.max(len / 2);
    if capacity <= 4096 / size {
        return Ok(0);
    }
    crate::model::size_vec(capacity, size)
}

pub(super) fn stable_sort_paid<T>(
    values: &mut [T],
    mem: &MemBudget<'_>,
    env: &ExecEnv<'_>,
    mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) -> Result<(), ExecError> {
    let bytes = stable_sort_scratch::<T>(values.len())?;
    LedgerWork::new(env).check()?;
    mem.grow(bytes)?;
    let charge = super::ScratchCharge { mem, bytes };
    values.sort_by(&mut compare);
    drop(charge);
    LedgerWork::new(env).check()
}

pub(super) struct BuilderHash<'m, 'a, K, V> {
    values: std::collections::HashMap<K, V>,
    model: crate::ops::hier::HierarchyHash<(K, V)>,
    mem: &'m MemBudget<'a>,
    balance: u64,
    #[cfg(test)]
    pub(super) owner: usize,
}
impl<'m, 'a, K: Eq + std::hash::Hash, V> BuilderHash<'m, 'a, K, V> {
    pub(super) fn new(mem: &'m MemBudget<'a>) -> Self {
        #[cfg(test)]
        {
            let _scope = super::builder_tests::scope(owner, 1);
            super::builder_tests::hash_new::<(K, V)>();
        }
        Self {
            values: std::collections::HashMap::new(),
            model: crate::ops::hier::HierarchyHash::new(),
            mem,
            balance: 0,
            #[cfg(test)]
            owner,
        }
    }
    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.values.contains_key(key)
    }
    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.values.get(key)
    }
    pub(super) fn insert(&mut self, key: K, value: V) -> Result<(), ExecError> {
        if self.values.contains_key(&key) {
            return Err(crate::model::invalid_size());
        }
        let prepared = self.model.prepare(self.mem, &mut self.balance)?;
        #[cfg(test)]
        super::builder_tests::hash_prepared(prepared.0);
        self.values.insert(key, value);
        self.model.inserted(
            prepared,
            self.values.capacity(),
            self.mem,
            &mut self.balance,
        )
    }
}
impl<K, V> Drop for BuilderHash<'_, '_, K, V> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.values));
        self.mem.shrink(std::mem::take(&mut self.balance));
    }
}

pub(super) struct BuilderKeySet<'m, 'a> {
    values: BuilderKeyMap<'m, 'a, ()>,
}
impl<'m, 'a> BuilderKeySet<'m, 'a> {
    pub(super) fn new(mem: &'m MemBudget<'a>) -> Self {
        Self {
            values: BuilderKeyMap::new(mem),
        }
    }
    pub(super) fn insert(
        &mut self,
        value: &query_ir::QValue,
        coll: crate::coll::ExecCollation<'_>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<bool, ExecError> {
        let mut key = crate::ops::hier::TotalsKey::new(value, self.values.mem, coll, env)?;
        if self.values.get(key.as_key()).is_some() {
            return Ok(false);
        }
        key.retain()?;
        self.values.insert(key, ())?;
        Ok(true)
    }
    pub(super) fn contains(
        &self,
        value: &query_ir::QValue,
        coll: crate::coll::ExecCollation<'_>,
        env: Option<&ExecEnv<'_>>,
    ) -> Result<bool, ExecError> {
        let key = crate::ops::hier::TotalsKey::new(value, self.values.mem, coll, env)?;
        Ok(self.values.get(key.as_key()).is_some())
    }
}
