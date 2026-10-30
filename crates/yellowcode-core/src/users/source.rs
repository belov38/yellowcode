use super::{UserWriteError, snapshot};
use onecd::{Database, FileStorage, Mode, OpenOptions, Storage};
use std::{
    ffi::CString,
    fs::{File, OpenOptions as FileOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Result<T> = std::result::Result<T, UserWriteError>;

pub(super) struct Source {
    storage: Arc<BoundStorage>,
}
struct Namespace {
    parent: File,
    parent_path: PathBuf,
    parent_identity: (u64, u64),
    name: CString,
    identity: (u64, u64),
}
struct BoundStorage {
    file: FileStorage,
    namespace: Namespace,
    namespace_changed: AtomicBool,
    writes_started: AtomicBool,
}

fn directory(path: &Path) -> Result<File> {
    FileOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| UserWriteError::StaleRevision)
}

impl Namespace {
    #[allow(unsafe_code)]
    fn open(&self, write: bool) -> Result<File> {
        let flags = if write { libc::O_RDWR } else { libc::O_RDONLY }
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        let fd = unsafe { libc::openat(self.parent.as_raw_fd(), self.name.as_ptr(), flags) };
        if fd < 0 {
            return Err(UserWriteError::StaleRevision);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file
            .metadata()
            .map_err(|_| UserWriteError::StorageBeforeCommit)?
            .is_file()
        {
            return Err(UserWriteError::UnsupportedSource);
        }
        Ok(file)
    }

    fn verify(&self, file: &File) -> Result<()> {
        let current_parent = directory(&self.parent_path)?;
        let named = self.open(false)?;
        if snapshot::identity(&current_parent)? != self.parent_identity
            || snapshot::identity(&named)? != self.identity
            || snapshot::identity(file)? != self.identity
        {
            return Err(UserWriteError::StaleRevision);
        }
        Ok(())
    }
}

impl Source {
    pub(super) fn open(path: &Path, mode: Mode) -> Result<Self> {
        let parent_path = path
            .parent()
            .ok_or(UserWriteError::UnsupportedSource)?
            .to_path_buf();
        let parent = directory(&parent_path)?;
        let name = CString::new(
            path.file_name()
                .ok_or(UserWriteError::UnsupportedSource)?
                .as_bytes(),
        )
        .map_err(|_| UserWriteError::UnsupportedSource)?;
        let mut namespace = Namespace {
            parent_identity: snapshot::identity(&parent)?,
            parent,
            parent_path,
            name,
            identity: (0, 0),
        };
        let file = namespace.open(mode == Mode::ReadWrite)?;
        namespace.identity = snapshot::identity(&file)?;
        let file =
            FileStorage::from_locked_file(path, file, mode).map_err(super::mutation::storage)?;
        let source = Self {
            storage: Arc::new(BoundStorage {
                file,
                namespace,
                namespace_changed: AtomicBool::new(false),
                writes_started: AtomicBool::new(false),
            }),
        };
        source.verify_path()?;
        if mode == Mode::ReadWrite {
            source
                .sync_parent()
                .map_err(|_| UserWriteError::StorageBeforeCommit)?;
        }
        Ok(source)
    }

    pub(super) fn database(&self, mode: Mode) -> Result<Database> {
        let options = if mode == Mode::ReadWrite {
            OpenOptions::read_write()
        } else {
            OpenOptions::read_only()
        };
        Database::open_storage(self.storage.clone(), options).map_err(super::mutation::storage)
    }
    pub(super) fn verify_path(&self) -> Result<()> {
        self.storage.namespace.verify(
            self.storage
                .file
                .as_file()
                .map_err(|_| UserWriteError::StorageBeforeCommit)?,
        )
    }
    pub(super) fn witness(&self) -> Result<snapshot::Witness> {
        snapshot::witness_file(
            self.storage
                .file
                .as_file()
                .map_err(|_| UserWriteError::StorageBeforeCommit)?,
        )
    }
    pub(super) fn sync_parent(&self) -> io::Result<()> {
        self.storage.namespace.parent.sync_all()
    }
    pub(super) fn refused_before_write(&self) -> bool {
        self.storage.namespace_changed.load(Ordering::Acquire)
            && !self.storage.writes_started.load(Ordering::Acquire)
    }
}

impl BoundStorage {
    fn guard(&self) -> io::Result<()> {
        if self.namespace.verify(self.file.as_file()?).is_err() {
            self.namespace_changed.store(true, Ordering::Release);
            return Err(io::Error::other("source namespace changed"));
        }
        Ok(())
    }
}
impl Storage for BoundStorage {
    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }
    fn read_at(&self, off: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.file.read_at(off, buffer)
    }
    fn write_at(&self, off: u64, buffer: &[u8]) -> io::Result<()> {
        self.guard()?;
        self.writes_started.store(true, Ordering::Release);
        self.file.write_at(off, buffer)?;
        #[cfg(test)]
        super::mutation::tests::transaction_event("page-written");
        Ok(())
    }
    fn set_len(&self, length: u64) -> io::Result<()> {
        self.guard()?;
        if self.file.len()? != length {
            self.writes_started.store(true, Ordering::Release);
        }
        self.file.set_len(length)
    }
    fn sync(&self) -> io::Result<()> {
        self.guard()?;
        self.file.sync()
    }
}
