use super::{
    collation::ManagedCollation,
    compare::rank,
    expr::{self, ExprError},
    key::{ManagedKey, try_key_of},
    map::ManagedMap,
    stream::{StreamControl, StreamError},
};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Reservation};
use query_ir::{PExpr, managed_value::row::ScopedRow};
use std::{
    cmp::Ordering,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::atomic::Ordering as AtomicOrdering,
    time::Instant,
};

fn checkpoint<E>(control: StreamControl<'_>, budget: &MemoryBudget) -> Result<(), StreamError<E>> {
    if control.cancel.load(AtomicOrdering::Relaxed) {
        return Err(StreamError::Cancelled);
    }
    if control.stop.is_some_and(|stop| Instant::now() >= stop) {
        return Err(StreamError::Timeout);
    }
    budget
        .check_admission(Category::Execution)
        .map_err(BudgetError::from)?;
    Ok(())
}
fn descriptor<T>(
    budget: &MemoryBudget,
    nested: usize,
    coll: &ManagedCollation,
) -> Result<Reservation, ExprError> {
    if budget.scope_snapshot().is_some() {
        return Err(BudgetError::Overflow.into());
    }
    coll.check_budget(budget)?;
    let size = std::mem::size_of::<T>()
        .checked_sub(nested)
        .ok_or(BudgetError::Overflow)?;
    Ok(budget
        .try_reserve(
            Category::Execution,
            u64::try_from(size).map_err(|_| BudgetError::Overflow)?,
        )
        .map_err(BudgetError::from)?)
}
#[derive(Debug)]
struct TupleKey(BudgetedVec<ManagedKey>);
impl PartialEq for TupleKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_slice() == other.0.as_slice()
    }
}
impl Eq for TupleKey {}
impl Hash for TupleKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_slice().hash(state);
    }
}

pub struct DistinctRows<'a, I> {
    input: Option<I>,
    seen: Option<ManagedMap<TupleKey, ()>>,
    hash: RandomState,
    coll: &'a ManagedCollation,
    control: StreamControl<'a>,
    charge: Reservation,
}
impl<'a, I> DistinctRows<'a, I> {
    pub fn try_new(
        input: I,
        budget: &MemoryBudget,
        coll: &'a ManagedCollation,
        control: StreamControl<'a>,
    ) -> Result<Self, ExprError> {
        let charge = descriptor::<Self>(
            budget,
            std::mem::size_of::<ManagedMap<TupleKey, ()>>(),
            coll,
        )?;
        let seen = ManagedMap::new(budget)?;
        Ok(Self {
            input: Some(input),
            seen: Some(seen),
            hash: RandomState::new(),
            coll,
            control,
            charge,
        })
    }
    fn finish(&mut self) {
        drop(self.input.take());
        drop(self.seen.take());
    }
    fn retain<E>(&mut self, row: &ScopedRow) -> Result<bool, StreamError<E>> {
        if !row.budget().same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        checkpoint(self.control, self.charge.budget())?;
        let mut keys =
            BudgetedVec::try_with_capacity(row.budget(), Category::Execution, row.values().len())?;
        for value in row.values() {
            checkpoint(self.control, self.charge.budget())?;
            keys.try_push(
                try_key_of(value.view(), self.coll, row.budget()).map_err(ExprError::from)?,
            )?;
        }
        let key = TupleKey(keys);
        let hash = self.hash.hash_one(&key);
        let seen = self.seen.as_mut().ok_or(BudgetError::Overflow)?;
        if seen.get(hash, &key).is_some() {
            return Ok(false);
        }
        seen.try_insert(hash, key, ()).map_err(|e| e.error)?;
        checkpoint(self.control, self.charge.budget())?;
        Ok(true)
    }
}
impl<I, E> Iterator for DistinctRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    type Item = Result<(u64, ScopedRow), StreamError<E>>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.input.as_ref()?;
            let next = checkpoint(self.control, self.charge.budget())
                .and_then(|()| self.input.as_mut().and_then(Iterator::next).transpose());
            match next {
                Ok(Some((id, row))) => match self.retain(&row) {
                    Ok(true) => return Some(Ok((id, row))),
                    Ok(false) => continue,
                    Err(error) => {
                        self.finish();
                        return Some(Err(error));
                    }
                },
                Ok(None) => {
                    self.finish();
                    return None;
                }
                Err(error) => {
                    self.finish();
                    return Some(Err(error));
                }
            }
        }
    }
}
impl<I, E> std::iter::FusedIterator for DistinctRows<'_, I> where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>
{
}

struct SortEntry {
    keys: BudgetedVec<(u8, ManagedKey)>,
    row: ScopedRow,
    tag: u64,
    ordinal: u64,
}
fn compare(a: &SortEntry, b: &SortEntry, keys: &[(PExpr, bool)]) -> Ordering {
    for ((left, right), (_, descending)) in a.keys.iter().zip(b.keys.iter()).zip(keys) {
        let order = left.0.cmp(&right.0).then_with(|| {
            if left.0 <= 1 {
                Ordering::Equal
            } else {
                left.1.cmp(&right.1)
            }
        });
        if order != Ordering::Equal {
            return if *descending { order.reverse() } else { order };
        }
    }
    a.ordinal.cmp(&b.ordinal)
}

#[allow(
    clippy::indexing_slicing,
    reason = "heap indices are bounded before access"
)]
fn sift_up(entries: &mut [SortEntry], keys: &[(PExpr, bool)]) {
    let mut index = entries.len() - 1;
    while index > 0 {
        let parent = (index - 1) / 2;
        if compare(&entries[parent], &entries[index], keys) != Ordering::Less {
            break;
        }
        entries.swap(parent, index);
        index = parent;
    }
}
#[allow(
    clippy::indexing_slicing,
    reason = "heap indices are bounded before access"
)]
fn sift_down(entries: &mut [SortEntry], keys: &[(PExpr, bool)]) {
    let mut index = 0usize;
    while let Some(child) = index
        .checked_mul(2)
        .and_then(|i| i.checked_add(1))
        .filter(|&i| i < entries.len())
    {
        let child = if child + 1 < entries.len()
            && compare(&entries[child], &entries[child + 1], keys) == Ordering::Less
        {
            child + 1
        } else {
            child
        };
        if compare(&entries[index], &entries[child], keys) != Ordering::Less {
            break;
        }
        entries.swap(index, child);
        index = child;
    }
}

pub struct SortRows<'a, I> {
    input: Option<I>,
    buffer: Option<BudgetedVec<SortEntry>>,
    keys: &'a [(PExpr, bool)],
    limit: Option<u64>,
    coll: &'a ManagedCollation,
    control: StreamControl<'a>,
    ready: bool,
    charge: Reservation,
}
impl<'a, I> SortRows<'a, I> {
    pub fn try_new(
        input: I,
        keys: &'a [(PExpr, bool)],
        limit: Option<u64>,
        budget: &MemoryBudget,
        coll: &'a ManagedCollation,
        control: StreamControl<'a>,
    ) -> Result<Self, ExprError> {
        let charge =
            descriptor::<Self>(budget, std::mem::size_of::<BudgetedVec<SortEntry>>(), coll)?;
        let buffer = BudgetedVec::new(budget, Category::Execution)?;
        Ok(Self {
            input: Some(input),
            buffer: Some(buffer),
            keys,
            limit,
            coll,
            control,
            ready: false,
            charge,
        })
    }
    fn finish(&mut self) {
        drop(self.input.take());
        drop(self.buffer.take());
    }
    fn entry<E>(
        &self,
        tag: u64,
        row: ScopedRow,
        ordinal: u64,
    ) -> Result<SortEntry, StreamError<E>> {
        if !row.budget().same_instance(self.charge.budget()) {
            return Err(BudgetError::Overflow.into());
        }
        checkpoint(self.control, self.charge.budget())?;
        let mut keys =
            BudgetedVec::try_with_capacity(row.budget(), Category::Execution, self.keys.len())?;
        for (expression, _) in self.keys {
            checkpoint(self.control, self.charge.budget())?;
            let value = expr::try_eval(expression, &row, self.coll, self.control.max_number_limbs)?;
            let value = value.values().first().ok_or(ExprError::Column(0))?.view();
            keys.try_push((
                rank(value),
                try_key_of(value, self.coll, row.budget()).map_err(ExprError::from)?,
            ))?;
        }
        checkpoint(self.control, self.charge.budget())?;
        Ok(SortEntry {
            keys,
            row,
            tag,
            ordinal,
        })
    }
}
impl<I, E> SortRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    fn load(&mut self) -> Result<(), StreamError<E>> {
        let mut ordinal = 0u64;
        if self.limit != Some(0) {
            loop {
                checkpoint(self.control, self.charge.budget())?;
                let Some((tag, row)) = self.input.as_mut().and_then(Iterator::next).transpose()?
                else {
                    break;
                };
                let entry = self.entry(tag, row, ordinal)?;
                ordinal = ordinal.checked_add(1).ok_or(BudgetError::Overflow)?;
                let buffer = self.buffer.as_mut().ok_or(BudgetError::Overflow)?;
                if self
                    .limit
                    .is_some_and(|n| u64::try_from(buffer.len()).is_ok_and(|len| len >= n))
                {
                    if buffer
                        .first()
                        .is_some_and(|worst| compare(&entry, worst, self.keys) == Ordering::Less)
                    {
                        let worst = buffer.first_mut().ok_or(BudgetError::Overflow)?;
                        *worst = entry;
                        sift_down(buffer, self.keys);
                    }
                } else {
                    buffer.try_push(entry)?;
                    if self.limit.is_some() {
                        sift_up(buffer, self.keys);
                    }
                }
            }
        }
        drop(self.input.take());
        checkpoint(self.control, self.charge.budget())?;
        self.buffer
            .as_mut()
            .ok_or(BudgetError::Overflow)?
            .sort_unstable_by(|a, b| compare(b, a, self.keys));
        self.ready = true;
        Ok(())
    }
}
impl<I, E> Iterator for SortRows<'_, I>
where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>,
{
    type Item = Result<(u64, ScopedRow), StreamError<E>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.buffer.as_ref()?;
        let state = if self.ready { Ok(()) } else { self.load() }
            .and_then(|()| checkpoint(self.control, self.charge.budget()));
        if let Err(error) = state {
            self.finish();
            return Some(Err(error));
        }
        match self.buffer.as_mut().and_then(BudgetedVec::pop) {
            Some(SortEntry { keys, row, tag, .. }) => {
                drop(keys);
                if self.buffer.as_ref().is_some_and(|buffer| buffer.is_empty()) {
                    self.finish();
                }
                Some(Ok((tag, row)))
            }
            None => {
                self.finish();
                None
            }
        }
    }
}
impl<I, E> std::iter::FusedIterator for SortRows<'_, I> where
    I: Iterator<Item = Result<(u64, ScopedRow), StreamError<E>>>
{
}
