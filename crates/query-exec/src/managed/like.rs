use super::{
    collation::{CollationError, ManagedCollation},
    map::ManagedMap,
};
use bsl_prim::Strength;
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Reservation, Shared};
use query_ir::managed_value::{ValueRef, row::ScopedRow};
use std::{borrow::Borrow, collections::hash_map::RandomState, hash::BuildHasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LikeError {
    Memory(BudgetError),
    Icu(icu46_sys::IcuStatus),
    Runtime(icu46_sys::managed::runtime::RuntimeError),
    Column,
}
impl From<BudgetError> for LikeError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<CollationError> for LikeError {
    fn from(error: CollationError) -> Self {
        match error {
            CollationError::Budget(e) => Self::Memory(e),
            CollationError::Icu(e) => Self::Icu(e),
            CollationError::Runtime(e) => Self::Runtime(e),
        }
    }
}
impl std::fmt::Display for LikeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(f),
            Self::Icu(e) => e.fmt(f),
            Self::Runtime(e) => write!(f, "{e:?}"),
            Self::Column => f.write_str("invalid LIKE column"),
        }
    }
}
impl std::error::Error for LikeError {}

#[derive(Clone)]
struct Key(Shared<Vec<u8>>);
impl Borrow<[u8]> for Key {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_slice() == other.0.as_slice()
    }
}
impl Eq for Key {}

enum Item {
    Any,
    Lit(Key),
    Set(CharSet),
}
struct CharSet {
    negated: bool,
    singles: ManagedMap<Key, ()>,
    ranges: BudgetedVec<(Key, Key)>,
}
impl CharSet {
    fn contains(&self, key: &[u8], hash: u64) -> bool {
        let hit = self.singles.get(hash, key).is_some()
            || self
                .ranges
                .iter()
                .any(|(lo, hi)| lo.0.as_slice() <= key && key <= hi.0.as_slice());
        hit != self.negated
    }
}
struct Pattern {
    items: BudgetedVec<Item>,
    lits: ManagedMap<Key, BudgetedVec<usize>>,
    any: BudgetedVec<usize>,
    sets: BudgetedVec<usize>,
    star: BudgetedVec<u64>,
    words: usize,
    hash: RandomState,
    _descriptor: Reservation,
}
fn vec<T>(budget: &MemoryBudget) -> Result<BudgetedVec<T>, BudgetError> {
    BudgetedVec::new(budget, Category::Execution)
}
fn key(cp: u32, coll: &ManagedCollation, budget: &MemoryBudget) -> Result<Key, LikeError> {
    let mut units = [0u16; 2];
    let text = match char::from_u32(cp) {
        Some(ch) => ch.encode_utf16(&mut units),
        None => {
            units[0] = u16::try_from(cp).map_err(|_| BudgetError::Overflow)?;
            &mut units[..1]
        }
    };
    Ok(Key(coll
        .try_sort_key(Strength::Secondary, text, budget)?
        .try_into_shared()?))
}
fn special(cp: u32) -> bool {
    matches!(cp, 0x25 | 0x5f | 0x5b)
}

impl Pattern {
    fn compile(
        pat: &[u16],
        escape: Option<char>,
        coll: &ManagedCollation,
        budget: &MemoryBudget,
    ) -> Result<Self, LikeError> {
        let descriptor = budget
            .try_reserve(
                Category::Execution,
                u64::try_from(std::mem::size_of::<Self>()).map_err(|_| BudgetError::Overflow)?,
            )
            .map_err(BudgetError::from)?;
        let mut result = Self {
            items: vec(budget)?,
            lits: ManagedMap::new(budget)?,
            any: vec(budget)?,
            sets: vec(budget)?,
            star: vec(budget)?,
            words: 0,
            hash: RandomState::new(),
            _descriptor: descriptor,
        };
        let mut p = BudgetedVec::try_with_capacity(budget, Category::Execution, pat.len())?;
        for cp in crate::eval::code_points(pat) {
            p.try_push(cp)?;
        }
        let mut stars = vec(budget)?;
        let escape = escape.map(u32::from);
        let mut i = 0;
        while let Some(&cp) = p.get(i) {
            i += 1;
            if Some(cp) == escape
                && let Some(&next) = p.get(i)
                && (special(next) || Some(next) == escape)
            {
                result.push_lit(next, coll, budget)?;
                i += 1;
                continue;
            }
            match cp {
                0x25 => stars.try_push(result.items.len())?,
                0x5f => result.items.try_push(Item::Any)?,
                0x5b => {
                    let (set, used) = result.parse_set(p.get(i..).unwrap_or(&[]), coll, budget)?;
                    result.items.try_push(Item::Set(set))?;
                    i = i.checked_add(used).ok_or(BudgetError::Overflow)?;
                }
                _ => result.push_lit(cp, coll, budget)?,
            }
        }
        result.words = result
            .items
            .len()
            .checked_add(1)
            .ok_or(BudgetError::Overflow)?
            .div_ceil(64);
        result.star.try_resize(result.words, 0)?;
        for &position in stars.iter() {
            set_bit(&mut result.star, position);
        }
        for (position, item) in result.items.iter().enumerate() {
            match item {
                Item::Any => result.any.try_push(position)?,
                Item::Set(_) => result.sets.try_push(position)?,
                Item::Lit(key) => {
                    let hash = result.hash.hash_one(key.0.as_slice());
                    if let Some(positions) = result.lits.get_mut(hash, key.0.as_slice()) {
                        positions.try_push(position)?;
                    } else {
                        let mut positions = vec(budget)?;
                        positions.try_push(position)?;
                        result
                            .lits
                            .try_insert(hash, key.clone(), positions)
                            .map_err(|e| e.error)?;
                    }
                }
            }
        }
        Ok(result)
    }
    fn push_lit(
        &mut self,
        cp: u32,
        coll: &ManagedCollation,
        budget: &MemoryBudget,
    ) -> Result<(), LikeError> {
        let key = key(cp, coll, budget)?;
        if cp == 0xdf {
            self.items.try_push(Item::Lit(key.clone()))?;
        }
        self.items.try_push(Item::Lit(key))?;
        Ok(())
    }
    fn parse_set(
        &self,
        p: &[u32],
        coll: &ManagedCollation,
        budget: &MemoryBudget,
    ) -> Result<(CharSet, usize), LikeError> {
        let mut set = CharSet {
            negated: p.first() == Some(&0x5e),
            singles: ManagedMap::new(budget)?,
            ranges: vec(budget)?,
        };
        let mut j = usize::from(set.negated);
        while let Some(&cp) = p.get(j) {
            j += 1;
            if cp == 0x5d {
                break;
            }
            match (p.get(j), p.get(j + 1)) {
                (Some(&0x2d), Some(&hi)) if hi != 0x5d => {
                    let a = key(cp, coll, budget)?;
                    let b = key(hi, coll, budget)?;
                    set.ranges.try_push(if a.0.as_slice() <= b.0.as_slice() {
                        (a, b)
                    } else {
                        (b, a)
                    })?;
                    j += 2;
                }
                _ => {
                    let key = key(cp, coll, budget)?;
                    set.singles
                        .try_insert(self.hash.hash_one(key.0.as_slice()), key, ())
                        .map_err(|e| e.error)?;
                }
            }
        }
        Ok((set, j))
    }
    fn mask(
        &self,
        cp: u32,
        coll: &ManagedCollation,
        budget: &MemoryBudget,
    ) -> Result<BudgetedVec<u64>, LikeError> {
        let key = key(cp, coll, budget)?;
        let hash = self.hash.hash_one(key.0.as_slice());
        let mut mask = vec(budget)?;
        mask.try_resize(self.words, 0)?;
        for &j in self.any.iter() {
            set_bit(&mut mask, j);
        }
        if let Some(positions) = self.lits.get(hash, key.0.as_slice()) {
            for &j in positions.iter() {
                set_bit(&mut mask, j);
            }
        }
        for &j in self.sets.iter() {
            if let Some(Item::Set(set)) = self.items.get(j)
                && set.contains(&key.0, hash)
            {
                set_bit(&mut mask, j);
            }
        }
        Ok(mask)
    }
    fn matches(
        &self,
        text: &[u16],
        coll: &ManagedCollation,
        budget: &MemoryBudget,
    ) -> Result<bool, LikeError> {
        let mut cur = vec(budget)?;
        let mut next = vec(budget)?;
        cur.try_resize(self.words, 0u64)?;
        next.try_resize(self.words, 0u64)?;
        set_bit(&mut cur, 0);
        let mut local = ManagedMap::new(budget)?;
        for cp in crate::eval::code_points(text) {
            let hash = self.hash.hash_one(cp);
            if local.get(hash, &cp).is_none() {
                let mask = self.mask(cp, coll, budget)?;
                local.try_insert(hash, cp, mask).map_err(|e| e.error)?;
            }
            let mask = local.get(hash, &cp).ok_or(BudgetError::Overflow)?;
            for _ in 0..if cp == 0xdf { 2 } else { 1 } {
                let mut carry = 0u64;
                let mut live = 0u64;
                for (((n, &s), &mk), &star) in next
                    .iter_mut()
                    .zip(cur.iter())
                    .zip(mask.iter())
                    .zip(self.star.iter())
                {
                    crate::probe::hit();
                    let accepted = s & mk;
                    *n = (accepted << 1) | carry | (s & star);
                    carry = accepted >> 63;
                    live |= *n;
                }
                if live == 0 {
                    return Ok(false);
                }
                std::mem::swap(&mut cur, &mut next);
            }
        }
        let k = self.items.len();
        Ok(cur.get(k / 64).is_some_and(|w| w >> (k % 64) & 1 == 1))
    }
}
fn set_bit(words: &mut [u64], j: usize) {
    if let Some(word) = words.get_mut(j / 64) {
        *word |= 1 << (j % 64);
    }
}

pub fn try_like(
    input: &ScopedRow,
    text: usize,
    pattern: usize,
    escape: Option<char>,
    collator: &ManagedCollation,
) -> Result<Option<bool>, LikeError> {
    let text = input.values().get(text).ok_or(LikeError::Column)?.view();
    let pattern = input.values().get(pattern).ok_or(LikeError::Column)?.view();
    collator.check_budget(input.budget())?;
    match (text, pattern) {
        (ValueRef::String(text), ValueRef::String(pattern)) => {
            let pattern = Pattern::compile(pattern, escape, collator, input.budget())?;
            Ok(Some(pattern.matches(text, collator, input.budget())?))
        }
        _ => Ok(None),
    }
}
