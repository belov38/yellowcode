use crate::scan::{ScanError, ScanErrorKind};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(PartialEq, Eq)]
enum Entry {
    Directory {
        children: usize,
    },
    File {
        digest: [u8; 32],
        link: Option<PathBuf>,
    },
    Link(PathBuf),
    Other,
}

#[cfg_attr(
    windows,
    allow(
        clippy::large_enum_variant,
        reason = "Windows ReadDir has a bounded inline OS buffer; keep eager enumeration allocation behavior"
    )
)]
pub(crate) enum DirectoryEntries {
    Permissive(std::fs::ReadDir),
    Checked(std::vec::IntoIter<std::io::Result<std::fs::DirEntry>>),
}
impl Iterator for DirectoryEntries {
    type Item = std::io::Result<std::fs::DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Permissive(entries) => entries.next(),
            Self::Checked(entries) => entries.next(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ScanInput {
    snapshot: Option<BTreeMap<PathBuf, Entry>>,
    failure: Mutex<Option<ScanError>>,
}

fn io_error(path: &Path, error: std::io::Error) -> ScanError {
    ScanError::typed(path, ScanErrorKind::Io, error.to_string())
}
fn changed(path: &Path) -> ScanError {
    ScanError::typed(path, ScanErrorKind::Changed, "names snapshot changed")
}
fn digest_file(path: &Path) -> Result<[u8; 32], ScanError> {
    let mut file = std::fs::File::open(path).map_err(|e| io_error(path, e))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 32 << 10];
    loop {
        let n = file.read(&mut buffer).map_err(|e| io_error(path, e))?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(digest.finalize().into())
}
fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Entry>, ScanError> {
    fn walk(dir: &Path, entries: &mut BTreeMap<PathBuf, Entry>) -> Result<(), ScanError> {
        let mut children = 0usize;
        let rd = std::fs::read_dir(dir).map_err(|e| io_error(dir, e))?;
        for entry in rd {
            children = children.checked_add(1).ok_or_else(|| {
                ScanError::typed(
                    dir,
                    ScanErrorKind::InvalidData,
                    "directory entry count overflow",
                )
            })?;
            let entry = entry.map_err(|e| io_error(dir, e))?;
            let path = entry.path();
            let ty = entry.file_type().map_err(|e| io_error(&path, e))?;
            if ty.is_dir() {
                walk(&path, entries)?;
            } else if ty.is_file() {
                entries.insert(
                    path.clone(),
                    Entry::File {
                        digest: digest_file(&path)?,
                        link: None,
                    },
                );
            } else if ty.is_symlink() {
                let target = std::fs::read_link(&path).map_err(|e| io_error(&path, e))?;
                let metadata = std::fs::metadata(&path).map_err(|e| io_error(&path, e))?;
                let value = if metadata.is_file() {
                    Entry::File {
                        digest: digest_file(&path)?,
                        link: Some(target),
                    }
                } else {
                    Entry::Link(target)
                };
                entries.insert(path, value);
            } else {
                entries.insert(path, Entry::Other);
            }
        }
        entries.insert(dir.to_path_buf(), Entry::Directory { children });
        Ok(())
    }
    let mut entries = BTreeMap::new();
    walk(root, &mut entries)?;
    Ok(entries)
}
impl ScanInput {
    pub(crate) fn strict(root: &Path) -> Result<Self, ScanError> {
        Ok(Self {
            snapshot: Some(inventory(root)?),
            failure: Mutex::new(None),
        })
    }
    pub(crate) fn latch(&self, error: ScanError) {
        if self.snapshot.is_none() {
            return;
        }
        let mut state = match self.failure.lock() {
            Ok(state) => state,
            Err(error) => error.into_inner(),
        };
        if state.is_none() {
            *state = Some(error);
        }
    }
    pub(crate) fn check(&self) -> Result<(), ScanError> {
        match self.failure.lock() {
            Ok(state) => state.as_ref().map_or(Ok(()), |e| Err(e.clone())),
            Err(_) => Err(ScanError::typed(
                Path::new(""),
                ScanErrorKind::Poisoned,
                "scanner failure latch poisoned",
            )),
        }
    }
    pub(crate) fn read(&self, path: &Path) -> Result<Vec<u8>, ScanError> {
        let result = (|| {
            if let Some(snapshot) = &self.snapshot
                && !matches!(snapshot.get(path), Some(Entry::File { .. }))
            {
                return Err(changed(path));
            }
            let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
            if let Some(snapshot) = &self.snapshot {
                let Some(Entry::File { digest, .. }) = snapshot.get(path) else {
                    return Err(changed(path));
                };
                let actual: [u8; 32] = Sha256::digest(&bytes).into();
                if *digest != actual {
                    return Err(changed(path));
                }
            }
            Ok(bytes)
        })();
        if let Err(error) = &result {
            self.latch(error.clone());
        }
        result
    }
    pub(crate) fn read_dir(&self, path: &Path) -> Option<DirectoryEntries> {
        let result = (|| {
            let entries = std::fs::read_dir(path).map_err(|e| io_error(path, e))?;
            let Some(snapshot) = &self.snapshot else {
                return Ok(DirectoryEntries::Permissive(entries));
            };
            let Some(Entry::Directory { children: expected }) = snapshot.get(path) else {
                return Err(changed(path));
            };
            let mut checked = Vec::new();
            let mut seen = BTreeSet::new();
            for entry in entries {
                let entry = entry.map_err(|e| io_error(path, e))?;
                let entry_path = entry.path();
                let ty = entry.file_type().map_err(|e| io_error(&entry_path, e))?;
                let same_kind = match snapshot.get(&entry_path) {
                    Some(Entry::Directory { .. }) => ty.is_dir(),
                    Some(Entry::File { link: None, .. }) => ty.is_file(),
                    Some(Entry::File { link: Some(_), .. } | Entry::Link(_)) => ty.is_symlink(),
                    Some(Entry::Other) => !ty.is_dir() && !ty.is_file() && !ty.is_symlink(),
                    None => false,
                };
                if !same_kind || !seen.insert(entry_path) {
                    return Err(changed(path));
                }
                checked.push(Ok(entry));
            }
            if seen.len() != *expected {
                return Err(changed(path));
            }
            Ok(DirectoryEntries::Checked(checked.into_iter()))
        })();
        match result {
            Ok(entries) => Some(entries),
            Err(error) => {
                self.latch(error);
                None
            }
        }
    }
    pub(crate) fn entry(
        &self,
        dir: &Path,
        entry: std::io::Result<std::fs::DirEntry>,
    ) -> Option<(PathBuf, std::fs::FileType)> {
        let result = entry.map_err(|e| io_error(dir, e)).and_then(|entry| {
            let path = entry.path();
            entry
                .file_type()
                .map(|ty| (path.clone(), ty))
                .map_err(|e| io_error(&path, e))
        });
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.latch(error);
                None
            }
        }
    }
    pub(crate) fn optional_present(&self, path: &Path) -> bool {
        match &self.snapshot {
            Some(snapshot) => matches!(snapshot.get(path), Some(Entry::File { .. })),
            None => path.is_file(),
        }
    }
    pub(crate) fn verify(&self, root: &Path) -> Result<(), ScanError> {
        self.check()?;
        if let Some(snapshot) = &self.snapshot {
            let result = inventory(root).and_then(|current| {
                if &current == snapshot {
                    Ok(())
                } else {
                    Err(changed(root))
                }
            });
            if let Err(error) = result {
                self.latch(error);
            }
        }
        self.check()
    }
}
