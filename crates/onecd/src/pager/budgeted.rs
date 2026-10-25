use super::PageBuf;
use crate::error::Result;
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Reservation};

const EMPTY: usize = 0;
const DELETED: usize = usize::MAX;
struct Slot {
    no: u32,
    page: Option<PageBuf>,
    prev: Option<usize>,
    next: Option<usize>,
}

pub(super) struct CleanCache {
    slots: BudgetedVec<Slot>,
    table: BudgetedVec<usize>,
    _structure: Reservation,
    free: Option<usize>,
    head: Option<usize>,
    tail: Option<usize>,
    tombstones: usize,
    evictions: u64,
    pages: u64,
    payload_bytes: u64,
    pin_bound: Option<usize>,
}
impl CleanCache {
    pub fn new(budget: &MemoryBudget, bytes: usize, page_size: usize) -> Result<Option<Self>> {
        let per = page_size
            .checked_add(std::mem::size_of::<Slot>())
            .and_then(|v| v.checked_add(4 * std::mem::size_of::<usize>() + 512))
            .ok_or(BudgetError::Overflow)?;
        let count = bytes / per;
        if count == 0 {
            return Ok(None);
        }
        let table_count = count
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(BudgetError::Overflow)?;
        let structure = budget
            .try_reserve(Category::Cache, std::mem::size_of::<Self>() as u64)
            .map_err(BudgetError::from)?;
        let mut slots = BudgetedVec::try_with_capacity(budget, Category::Cache, count)?;
        let mut table = BudgetedVec::try_with_capacity(budget, Category::Cache, table_count)?;
        for i in 0..count {
            slots.try_push(Slot {
                no: 0,
                page: None,
                prev: None,
                next: (i + 1 < count).then_some(i + 1),
            })?;
        }
        table.try_resize(table_count, EMPTY)?;
        Ok(Some(Self {
            slots,
            table,
            _structure: structure,
            free: Some(0),
            head: None,
            tail: None,
            tombstones: 0,
            evictions: 0,
            pages: 0,
            payload_bytes: 0,
            pin_bound: None,
        }))
    }
    pub fn stats(&mut self) -> super::ReaderStats {
        let mut stats = super::ReaderStats {
            evictions: self.evictions,
            cache_pages: self.pages,
            cache_payload_bytes: self.payload_bytes,
            ..Default::default()
        };
        let mut deepest = None;
        let mut at = self.pin_bound.and(self.head);
        while let Some(index) = at {
            let Some(slot) = self.slots.get(index) else {
                break;
            };
            if let Some(page) = &slot.page
                && page.is_pinned()
            {
                stats.pinned_cache_pages += 1;
                stats.pinned_cache_payload_bytes += page.len() as u64;
                deepest = Some(index);
            }
            if Some(index) == self.pin_bound {
                break;
            }
            at = slot.next;
        }
        self.pin_bound = deepest;
        stats
    }
    fn touch(&mut self, index: usize) {
        if self.pin_bound == Some(index) {
            self.pin_bound = self.slots.get(index).and_then(|s| s.prev).or(Some(index));
        }
        self.unlink(index);
        self.prepend(index);
        if self.pin_bound.is_none() {
            self.pin_bound = Some(index);
        }
    }
    fn hash(&self, no: u32) -> usize {
        (no.wrapping_mul(0x9e37_79b9) as usize) & (self.table.len() - 1)
    }
    fn locate(&self, no: u32) -> Option<(usize, usize)> {
        let start = self.hash(no);
        for step in 0..self.table.len() {
            let bucket = (start + step) & (self.table.len() - 1);
            let index = *self.table.get(bucket)?;
            if index == EMPTY {
                return None;
            }
            if index != DELETED
                && self
                    .slots
                    .get(index - 1)
                    .is_some_and(|s| s.no == no && s.page.is_some())
            {
                return Some((bucket, index - 1));
            }
        }
        None
    }
    fn index_insert(&mut self, no: u32, index: usize) {
        let start = self.hash(no);
        for step in 0..self.table.len() {
            let bucket = (start + step) & (self.table.len() - 1);
            if let Some(value) = self.table.get_mut(bucket)
                && (*value == EMPTY || *value == DELETED)
            {
                if *value == DELETED {
                    self.tombstones -= 1;
                }
                *value = index + 1;
                return;
            }
        }
    }
    fn unlink(&mut self, index: usize) {
        let Some(slot) = self.slots.get(index) else {
            return;
        };
        let (prev, next) = (slot.prev, slot.next);
        if let Some(p) = prev {
            if let Some(slot) = self.slots.get_mut(p) {
                slot.next = next;
            }
        } else {
            self.head = next;
        }
        if let Some(n) = next {
            if let Some(slot) = self.slots.get_mut(n) {
                slot.prev = prev;
            }
        } else {
            self.tail = prev;
        }
    }
    fn prepend(&mut self, index: usize) {
        if let Some(head) = self.head
            && let Some(slot) = self.slots.get_mut(head)
        {
            slot.prev = Some(index);
        }
        if let Some(slot) = self.slots.get_mut(index) {
            slot.prev = None;
            slot.next = self.head;
        }
        self.head = Some(index);
        if self.tail.is_none() {
            self.tail = Some(index);
        }
    }
    pub fn get(&mut self, no: u32) -> Option<PageBuf> {
        let (_, index) = self.locate(no)?;
        self.touch(index);
        self.slots.get(index)?.page.clone()
    }
    pub fn evict_one(&mut self) -> Result<bool> {
        let Some(index) = self.tail else {
            return Ok(false);
        };
        let Some(slot) = self.slots.get(index) else {
            return Ok(false);
        };
        let no = slot.no;
        let prev = slot.prev;
        if let Some(page) = &slot.page {
            page.transfer(Category::ReaderWorking)?;
            self.pages -= 1;
            self.payload_bytes -= page.len() as u64;
        }
        if self.pin_bound == Some(index) {
            self.pin_bound = prev;
        }
        self.unlink(index);
        self.evictions = self.evictions.saturating_add(1);
        if let Some((bucket, _)) = self.locate(no)
            && let Some(value) = self.table.get_mut(bucket)
        {
            *value = DELETED;
            self.tombstones += 1;
        }
        if let Some(slot) = self.slots.get_mut(index) {
            slot.page = None;
            slot.next = self.free;
            slot.prev = None;
        }
        self.free = Some(index);
        if self.tombstones > self.slots.len() / 2 {
            self.table.fill(EMPTY);
            self.tombstones = 0;
            for index in 0..self.slots.len() {
                if let Some(slot) = self.slots.get(index)
                    && slot.page.is_some()
                {
                    self.index_insert(slot.no, index);
                }
            }
        }
        Ok(true)
    }
    pub fn insert(&mut self, no: u32, page: &PageBuf) -> Result<()> {
        if self.locate(no).is_some() {
            return Ok(());
        }
        if self.free.is_none() {
            self.evict_one()?;
        }
        loop {
            match page.transfer(Category::Cache) {
                Ok(()) => break,
                Err(crate::error::Error::Memory(BudgetError::Limit(_))) => {
                    if !self.evict_one()? {
                        return Ok(());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        let Some(index) = self.free else {
            page.transfer(Category::ReaderWorking)?;
            return Ok(());
        };
        if let Some(slot) = self.slots.get_mut(index) {
            self.free = slot.next;
            slot.no = no;
            slot.page = Some(page.clone());
            self.pages += 1;
            self.payload_bytes += page.len() as u64;
        }
        self.index_insert(no, index);
        self.prepend(index);
        if self.pin_bound.is_none() {
            self.pin_bound = Some(index);
        }
        Ok(())
    }
}
impl Drop for CleanCache {
    fn drop(&mut self) {
        for slot in self.slots.iter() {
            if let Some(page) = &slot.page {
                let _result = page.transfer(Category::ReaderWorking);
            }
        }
    }
}
