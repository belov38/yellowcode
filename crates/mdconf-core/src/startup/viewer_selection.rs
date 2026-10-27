use super::{
    ManagedConfigurationError, ManagedViewerOpen, ModuleDescriptor, ModuleRead, SelectedObject,
};
use crate::ManagedEntrySource;
use memory_budget::BudgetError;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub struct ViewerDataIdentity(u64);
impl ViewerDataIdentity {
    pub(super) fn allocate() -> std::result::Result<Self, BudgetError> {
        NEXT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(Self)
            .map_err(|_| BudgetError::Overflow)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ManagedObjectKey {
    identity: ViewerDataIdentity,
    ordinal: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ManagedModuleKey {
    identity: ViewerDataIdentity,
    ordinal: usize,
}
impl ManagedObjectKey {
    pub fn ordinal(self) -> usize {
        self.ordinal
    }
}
impl ManagedModuleKey {
    pub fn ordinal(self) -> usize {
        self.ordinal
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ManagedSelectionError<E> {
    #[error("selection belongs to another snapshot")]
    ForeignSnapshot,
    #[error("validated selection is no longer present")]
    MissingValidated,
    #[error("{0}")]
    Decode(#[from] ManagedConfigurationError<E>),
}
type Result<T, E> = std::result::Result<T, ManagedSelectionError<E>>;
impl<S: ManagedEntrySource> ManagedViewerOpen<S> {
    pub fn data_identity(&self) -> ViewerDataIdentity {
        self.data_id
    }
    pub fn object_key(&self, ordinal: usize) -> Option<ManagedObjectKey> {
        self.objects().get(ordinal).map(|_| ManagedObjectKey {
            identity: self.data_id,
            ordinal,
        })
    }
    pub fn module_key(&self, ordinal: usize) -> Option<ManagedModuleKey> {
        self.modules().get(ordinal).map(|_| ManagedModuleKey {
            identity: self.data_id,
            ordinal,
        })
    }
    pub fn read_selected_object(&self, key: ManagedObjectKey) -> Result<SelectedObject, S::Error> {
        if key.identity != self.data_id {
            return Err(ManagedSelectionError::ForeignSnapshot);
        }
        let row = self
            .objects()
            .get(key.ordinal)
            .ok_or(ManagedSelectionError::MissingValidated)?;
        self.context()
            .read_object(row.object.id)?
            .ok_or(ManagedSelectionError::MissingValidated)
    }
    pub fn module_descriptor(&self, key: ManagedModuleKey) -> Result<&ModuleDescriptor, S::Error> {
        if key.identity != self.data_id {
            return Err(ManagedSelectionError::ForeignSnapshot);
        }
        self.modules()
            .get(key.ordinal)
            .ok_or(ManagedSelectionError::MissingValidated)
    }
    pub fn read_selected_module(&self, key: ManagedModuleKey) -> Result<ModuleRead, S::Error> {
        let descriptor = self.module_descriptor(key)?;
        let entries = self.context().entries()?;
        let read = descriptor
            .slot
            .read(&entries)
            .map_err(ManagedConfigurationError::from)?;
        if read.module.is_none() {
            return Err(ManagedSelectionError::MissingValidated);
        }
        Ok(read)
    }
}
