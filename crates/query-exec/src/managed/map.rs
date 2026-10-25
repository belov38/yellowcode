use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Reservation};
use std::borrow::Borrow;

#[derive(Debug)]
struct Entry<K, V> {
    hash: u64,
    key: K,
    value: V,
}

#[derive(Debug)]
pub struct InsertError<K, V> {
    pub error: BudgetError,
    pub key: K,
    pub value: V,
}

#[derive(Debug)]
pub struct ManagedMap<K, V> {
    slots: BudgetedVec<Option<Entry<K, V>>>,
    len: usize,
    descriptor: Reservation,
}

impl<K, V> ManagedMap<K, V> {
    pub fn new(budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let bytes = std::mem::size_of::<Self>()
            .checked_sub(std::mem::size_of::<BudgetedVec<Option<Entry<K, V>>>>())
            .ok_or(BudgetError::Overflow)?;
        let descriptor = budget.try_reserve(
            Category::Execution,
            u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?,
        )?;
        let slots = BudgetedVec::new(budget, Category::Execution)?;
        Ok(Self {
            slots,
            len: 0,
            descriptor,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get<Q: Eq + ?Sized>(&self, hash: u64, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let index = self.find(hash, key)?;
        self.slots.get(index)?.as_ref().map(|entry| &entry.value)
    }

    pub fn get_mut<Q: Eq + ?Sized>(&mut self, hash: u64, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let index = self.find(hash, key)?;
        self.slots
            .get_mut(index)?
            .as_mut()
            .map(|entry| &mut entry.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref().map(|entry| (&entry.key, &entry.value)))
    }

    pub fn clear(&mut self) {
        self.len = 0;
        let mut cleanup = ClearSlots {
            remaining: &mut self.slots,
        };
        cleanup.finish();
    }

    fn find<Q: Eq + ?Sized>(&self, hash: u64, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut index = hash as usize & mask;
        loop {
            match self.slots.get(index)?.as_ref() {
                None => return None,
                Some(entry) if entry.hash == hash && entry.key.borrow() == key => {
                    return Some(index);
                }
                Some(_) => index = (index + 1) & mask,
            }
        }
    }

    fn try_grow(&mut self) -> Result<(), BudgetError> {
        let capacity = if self.slots.is_empty() {
            8
        } else {
            self.slots
                .len()
                .checked_mul(2)
                .ok_or(BudgetError::Overflow)?
        };
        let mut slots = BudgetedVec::try_with_capacity(
            self.descriptor.budget(),
            Category::Execution,
            capacity,
        )?;
        for _ in 0..capacity {
            slots.try_push(None)?;
        }
        for old in self.slots.iter_mut() {
            if let Some(entry) = old.take() {
                let index = empty_slot(&slots, entry.hash);
                #[allow(
                    clippy::indexing_slicing,
                    reason = "empty_slot returns a masked index within this nonempty power-of-two table"
                )]
                let slot = &mut slots[index];
                *slot = Some(entry);
            }
        }
        let old = std::mem::replace(&mut self.slots, slots);
        drop(old);
        Ok(())
    }
}

impl<K: Eq, V> ManagedMap<K, V> {
    pub fn try_insert(
        &mut self,
        hash: u64,
        key: K,
        value: V,
    ) -> Result<Option<V>, InsertError<K, V>> {
        if let Some(index) = self.find(hash, &key)
            && let Some(Some(entry)) = self.slots.get_mut(index)
        {
            return Ok(Some(std::mem::replace(&mut entry.value, value)));
        }
        let capacity = self.slots.len();
        if self.len >= capacity - capacity / 4
            && let Err(error) = self.try_grow()
        {
            return Err(InsertError { error, key, value });
        }
        let index = empty_slot(&self.slots, hash);
        #[allow(
            clippy::indexing_slicing,
            reason = "empty_slot returns a masked index within this nonempty power-of-two table"
        )]
        let slot = &mut self.slots[index];
        *slot = Some(Entry { hash, key, value });
        self.len += 1;
        Ok(None)
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "private callers supply a nonempty power-of-two table; every index is masked by len minus one"
)]
fn empty_slot<K, V>(slots: &[Option<Entry<K, V>>], hash: u64) -> usize {
    let mask = slots.len() - 1;
    let mut index = hash as usize & mask;
    while slots[index].is_some() {
        index = (index + 1) & mask;
    }
    index
}

struct ClearSlots<'a, K, V> {
    remaining: &'a mut [Option<Entry<K, V>>],
}
impl<K, V> ClearSlots<'_, K, V> {
    fn finish(&mut self) {
        while let Some((slot, rest)) = std::mem::take(&mut self.remaining).split_first_mut() {
            self.remaining = rest;
            drop(slot.take());
        }
    }
}
impl<K, V> Drop for ClearSlots<'_, K, V> {
    fn drop(&mut self) {
        self.finish();
    }
}
