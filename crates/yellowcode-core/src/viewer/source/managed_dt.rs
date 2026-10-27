use super::{ConfigEntrySummary, Fingerprint, OpenMemory, dt};
use mdconf::{EntryMeta, ManagedEntrySource};
use memory_budget::{BudgetError, Category, Charged, MemoryBudget, Shared};
use std::{io, path::Path};

#[derive(Debug, thiserror::Error)]
pub enum ManagedDtExtractError {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Extraction(crate::Error),
    #[error("DT input changed during extraction")]
    Changed,
}
pub struct ManagedDtSource {
    spool: dt::Spool,
    fingerprint: Fingerprint,
    _opening: OpenMemory,
}
fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let meta = std::fs::metadata(path)?;
    Ok(Fingerprint::File {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}
impl ManagedDtSource {
    pub fn extract(
        path: &Path,
        memory: &OpenMemory,
        progress: &mut impl FnMut(&'static str) -> crate::Result<()>,
    ) -> Result<Shared<Self>, ManagedDtExtractError> {
        progress("container").map_err(ManagedDtExtractError::Extraction)?;
        let before = fingerprint(path)?;
        let spool = dt::extract_with_memory(path, memory, progress)
            .map_err(ManagedDtExtractError::Extraction)?;
        if fingerprint(path)? != before {
            return Err(ManagedDtExtractError::Changed);
        }
        let reservation = memory
            .ledger()
            .try_reserve(Category::Metadata, 0)
            .map_err(BudgetError::from)?;
        Shared::try_new(
            Self {
                spool,
                fingerprint: before,
                _opening: memory.clone(),
            },
            reservation,
        )
        .map_err(Into::into)
    }
    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }
    pub fn verify_origin(&self, path: &Path) -> Result<(), ManagedDtExtractError> {
        if fingerprint(path)? == self.fingerprint {
            Ok(())
        } else {
            Err(ManagedDtExtractError::Changed)
        }
    }
    pub fn summary(&self) -> ConfigEntrySummary {
        self.spool.summary()
    }
    pub fn read_range(
        &self,
        name: &str,
        offset: u64,
        len: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        mdconf::EntrySource::stored_range_owned(&self.spool, name, offset, len)
    }
    pub fn largest_range(
        &self,
        offset: u64,
        len: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        self.spool.largest_range(offset, len)
    }
}
impl ManagedEntrySource for ManagedDtSource {
    type Error = super::ManagedDtSourceError;
    fn list(&self) -> &[EntryMeta] {
        ManagedEntrySource::list(&self.spool)
    }
    fn memory_budget(&self) -> &MemoryBudget {
        ManagedEntrySource::memory_budget(&self.spool)
    }
    fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>, Self::Error> {
        ManagedEntrySource::stored(&self.spool, name)
    }
    fn stored_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Option<Charged<Vec<u8>>>, Self::Error> {
        ManagedEntrySource::stored_bounded(&self.spool, name, limit)
    }
}
