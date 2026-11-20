use infobase::extensions::{Error, Result};
use std::{fs::File, path::Path, time::SystemTime};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    identity: (u64, u64),
    len: u64,
    modified: SystemTime,
}
impl Stamp {
    pub(super) fn check(self, file: &File) -> Result<()> {
        if Self::file(file)? != self {
            return Err(Error::Changed);
        }
        Ok(())
    }
    pub(super) fn file(file: &File) -> Result<Self> {
        let metadata = file.metadata().map_err(|e| Error::Io(e.kind()))?;
        let identity = crate::users::file_identity(file)
            .map_err(|_| Error::Unsupported("source identity unavailable"))?;
        if identity == (0, 0) {
            return Err(Error::Unsupported("source identity unavailable"));
        }
        Ok(Self {
            identity,
            len: metadata.len(),
            modified: metadata.modified().map_err(|e| Error::Io(e.kind()))?,
        })
    }
    pub(super) fn capture(path: &Path, budget: &memory_budget::MemoryBudget) -> Result<Self> {
        let _path = infobase::extensions::path_open_scratch(budget, path)?;
        Self::file(&File::open(path).map_err(|e| Error::Io(e.kind()))?)
    }
}
