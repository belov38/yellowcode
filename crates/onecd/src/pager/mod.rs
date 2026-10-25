use std::sync::Arc;

use crate::error::Result;

mod budgeted;
pub(crate) mod files;
pub mod storage;
pub(crate) mod txn;
pub(crate) mod versioned;

#[cfg(any(test, feature = "testing"))]
pub use storage::FailingStorage;
pub use storage::{FileStorage, MemStorage, Storage};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub cache_pages: u64,
    pub cache_payload_bytes: u64,
    pub pinned_cache_pages: u64,
    pub pinned_cache_payload_bytes: u64,
    pub reader_working_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct PageBuf(PageBacking);
#[derive(Clone, Debug)]
enum PageBacking {
    Legacy(Arc<[u8]>),
    Budgeted(memory_budget::Shared<Vec<u8>>),
}
impl PageBuf {
    pub(crate) fn try_zeroed(budget: &memory_budget::MemoryBudget, len: usize) -> Result<Self> {
        let mut bytes = memory_budget::BudgetedVec::try_with_capacity(
            budget,
            memory_budget::Category::ReaderWorking,
            len,
        )?;
        bytes.try_resize(len, 0)?;
        Ok(Self(PageBacking::Budgeted(bytes.try_into_shared()?)))
    }
    pub fn ptr_eq(first: &Self, second: &Self) -> bool {
        match (&first.0, &second.0) {
            (PageBacking::Legacy(a), PageBacking::Legacy(b)) => Arc::ptr_eq(a, b),
            (PageBacking::Budgeted(a), PageBacking::Budgeted(b)) => {
                memory_budget::Shared::ptr_eq(a, b)
            }
            _ => false,
        }
    }
    pub(crate) fn is_pinned(&self) -> bool {
        match &self.0 {
            PageBacking::Legacy(v) => Arc::strong_count(v) > 1,
            PageBacking::Budgeted(v) => v.strong_count() > 1,
        }
    }
    pub(crate) fn transfer(&self, category: memory_budget::Category) -> Result<()> {
        if let PageBacking::Budgeted(value) = &self.0 {
            value
                .reservation()
                .transfer(category)
                .map_err(memory_budget::BudgetError::from)?;
        }
        Ok(())
    }
    pub(crate) fn unique_mut(&mut self) -> Option<&mut [u8]> {
        match &mut self.0 {
            PageBacking::Legacy(value) => Arc::get_mut(value),
            PageBacking::Budgeted(value) => {
                memory_budget::Shared::get_mut(value).map(Vec::as_mut_slice)
            }
        }
    }
    pub(crate) fn make_mut(this: &mut Self) -> &mut [u8] {
        if let PageBacking::Budgeted(_) = this.0 {
            this.0 = PageBacking::Legacy(Arc::from(this.as_ref()));
        }
        match &mut this.0 {
            PageBacking::Legacy(value) => Arc::make_mut(value),
            PageBacking::Budgeted(_) => std::process::abort(),
        }
    }
}
impl std::ops::Deref for PageBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}
impl AsRef<[u8]> for PageBuf {
    fn as_ref(&self) -> &[u8] {
        match &self.0 {
            PageBacking::Legacy(value) => value,
            PageBacking::Budgeted(value) => value.as_slice(),
        }
    }
}
impl From<Vec<u8>> for PageBuf {
    fn from(value: Vec<u8>) -> Self {
        Self(PageBacking::Legacy(value.into()))
    }
}
impl From<&[u8]> for PageBuf {
    fn from(value: &[u8]) -> Self {
        Self(PageBacking::Legacy(value.into()))
    }
}
impl From<Arc<[u8]>> for PageBuf {
    fn from(value: Arc<[u8]>) -> Self {
        Self(PageBacking::Legacy(value))
    }
}
impl PartialEq for PageBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for PageBuf {}

pub trait PageRead: Send + Sync {
    fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        None
    }
    fn page_size(&self) -> u32;
    fn num_pages(&self) -> u32;
    fn read_page(&self, no: u32) -> Result<PageBuf>;
    fn file_cache(&self) -> Option<(&files::FileCache, u64)> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PageKind {
    Content,
    Alloc,
    FileHeader,
    FreeList,
    DbHeader,
}
