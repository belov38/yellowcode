use crate::{ManagedSelectError, native};
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub struct DiscoveryRoots<'a> {
    pub override_root: Option<&'a Path>,
    pub executable: &'a Path,
}

pub struct DiscoverySnapshot {
    override_root: Option<PathBuf>,
    executable: PathBuf,
    _charges: [Option<Reservation>; 2],
}
impl DiscoverySnapshot {
    pub fn capture(budget: &MemoryBudget) -> Result<Self, ManagedSelectError> {
        let override_root = native::override_root(budget)?;
        let executable = native::executable(budget)?;
        Ok(match override_root {
            Some(path) => Self {
                override_root: Some(path.value),
                executable: executable.value,
                _charges: [Some(path._charge), Some(executable._charge)],
            },
            None => Self {
                override_root: None,
                executable: executable.value,
                _charges: [None, Some(executable._charge)],
            },
        })
    }

    pub fn adopt(
        override_root: Option<OsString>,
        executable: PathBuf,
        budget: &MemoryBudget,
    ) -> Result<Self, ManagedSelectError> {
        let bytes = override_root
            .as_ref()
            .map_or(0, OsString::capacity)
            .checked_add(executable.capacity())
            .ok_or(BudgetError::Overflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?;
        let charge = budget.try_reserve(Category::Metadata, bytes)?;
        Ok(Self {
            override_root: override_root.map(PathBuf::from),
            executable,
            _charges: [Some(charge), None],
        })
    }

    pub fn roots(&self) -> DiscoveryRoots<'_> {
        DiscoveryRoots {
            override_root: self.override_root.as_deref(),
            executable: &self.executable,
        }
    }
}
