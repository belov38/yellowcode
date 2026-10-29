use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
#[cfg(any(test, feature = "testing"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::db::Mode;
use crate::error::Result;
use crate::lock::DbLock;

#[allow(clippy::len_without_is_empty)]
pub trait Storage: Send + Sync {
    fn len(&self) -> io::Result<u64>;
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&self, off: u64, buf: &[u8]) -> io::Result<()>;
    fn set_len(&self, len: u64) -> io::Result<()>;
    fn sync(&self) -> io::Result<()>;
    fn file_close_observer(&self) -> Option<FileCloseObserver> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct FileCloseObserver(Arc<AtomicBool>);

impl FileCloseObserver {
    pub fn is_closed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) enum StorageOwner {
    External(std::sync::Arc<dyn Storage>),
    File(crate::catalog::Metadata<FileStorage>),
}
impl<T: Storage + 'static> From<std::sync::Arc<T>> for StorageOwner {
    fn from(storage: std::sync::Arc<T>) -> Self {
        Self::External(storage)
    }
}
impl From<std::sync::Arc<dyn Storage>> for StorageOwner {
    fn from(storage: std::sync::Arc<dyn Storage>) -> Self {
        Self::External(storage)
    }
}
impl std::ops::Deref for StorageOwner {
    type Target = dyn Storage;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::External(storage) => storage.as_ref(),
            Self::File(storage) => &**storage,
        }
    }
}

#[derive(Debug)]
pub struct FileStorage {
    handle: Option<Handle>,
    closed: OnceLock<Arc<AtomicBool>>,
}

#[derive(Debug)]
enum Handle {
    Plain(File),
    Locked(DbLock),
}

impl FileStorage {
    pub fn open(path: &Path, write: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(write).open(path)?;
        Ok(FileStorage {
            handle: Some(Handle::Plain(file)),
            closed: OnceLock::new(),
        })
    }

    pub(crate) fn open_locked(path: &Path, mode: Mode) -> Result<Self> {
        Ok(FileStorage {
            handle: Some(Handle::Locked(DbLock::acquire(path, mode)?)),
            closed: OnceLock::new(),
        })
    }

    fn file(&self) -> io::Result<&File> {
        match &self.handle {
            Some(Handle::Plain(f)) => Ok(f),
            Some(Handle::Locked(lock)) => lock.file(),
            None => Err(io::Error::other("the database file is closed")),
        }
    }
}

impl Drop for FileStorage {
    fn drop(&mut self) {
        drop(self.handle.take());
        if let Some(closed) = self.closed.get() {
            closed.store(true, Ordering::Release);
        }
    }
}

#[cfg(unix)]
fn file_read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, off)
}

#[cfg(unix)]
fn file_write_all_at(file: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, off)
}

#[cfg(windows)]
fn file_read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, off) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let (_, rest) = std::mem::take(&mut buf)
                    .split_at_mut_checked(n)
                    .ok_or_else(|| io::Error::other("read returned more than requested"))?;
                buf = rest;
                off = off.saturating_add(n as u64);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn file_write_all_at(file: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, off) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = buf
                    .get(n..)
                    .ok_or_else(|| io::Error::other("write returned more than requested"))?;
                off = off.saturating_add(n as u64);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Storage for FileStorage {
    fn file_close_observer(&self) -> Option<FileCloseObserver> {
        Some(FileCloseObserver(Arc::clone(
            self.closed.get_or_init(|| Arc::new(AtomicBool::new(false))),
        )))
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file()?.metadata()?.len())
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        file_read_exact_at(self.file()?, buf, off)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> io::Result<()> {
        file_write_all_at(self.file()?, buf, off)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file()?.set_len(len)
    }

    fn sync(&self) -> io::Result<()> {
        self.file()?.sync_all()
    }
}

#[derive(Debug, Default)]
pub struct MemStorage {
    bytes: RwLock<Vec<u8>>,
}

fn invalid_input(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what)
}

fn to_index(v: u64) -> io::Result<usize> {
    usize::try_from(v).map_err(|_| invalid_input("offset does not fit in memory"))
}

fn resize_zeroed(v: &mut Vec<u8>, len: usize) -> io::Result<()> {
    if let Some(extra) = len.checked_sub(v.len()) {
        v.try_reserve(extra).map_err(|_| {
            io::Error::new(io::ErrorKind::OutOfMemory, "cannot grow in-memory file")
        })?;
    }
    v.resize(len, 0);
    Ok(())
}

impl MemStorage {
    pub fn new(b: Vec<u8>) -> Self {
        MemStorage {
            bytes: RwLock::new(b),
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn read_guard(&self) -> RwLockReadGuard<'_, Vec<u8>> {
        self.bytes.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_guard(&self) -> RwLockWriteGuard<'_, Vec<u8>> {
        self.bytes.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Storage for MemStorage {
    fn len(&self) -> io::Result<u64> {
        Ok(self.read_guard().len() as u64)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let eof = || io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer");
        let start = usize::try_from(off).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        let guard = self.read_guard();
        buf.copy_from_slice(guard.get(start..end).ok_or_else(eof)?);
        Ok(())
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> io::Result<()> {
        let start = to_index(off)?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| invalid_input("write range overflows"))?;
        let mut guard = self.write_guard();
        if end > guard.len() {
            resize_zeroed(&mut guard, end)?;
        }
        guard
            .get_mut(start..end)
            .ok_or_else(|| invalid_input("write range is out of bounds"))?
            .copy_from_slice(buf);
        Ok(())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        resize_zeroed(&mut self.write_guard(), to_index(len)?)
    }

    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
#[derive(Debug)]
pub struct FailingStorage {
    inner: MemStorage,
    fail_write_at: Option<u64>,
    fail_sync: bool,
    fail_set_len: bool,
    writes: AtomicU64,
}

#[cfg(any(test, feature = "testing"))]
impl FailingStorage {
    pub fn new(inner: MemStorage, fail_write_at: Option<u64>, fail_sync: bool) -> Self {
        FailingStorage {
            inner,
            fail_write_at,
            fail_sync,
            fail_set_len: false,
            writes: AtomicU64::new(0),
        }
    }

    pub fn fail_set_len(mut self) -> Self {
        self.fail_set_len = true;
        self
    }

    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    pub fn inner(&self) -> &MemStorage {
        &self.inner
    }

    pub fn into_inner(self) -> MemStorage {
        self.inner
    }
}

#[cfg(any(test, feature = "testing"))]
impl Storage for FailingStorage {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(off, buf)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> io::Result<()> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        if self.fail_write_at == Some(n) {
            let (half, _) = buf.split_at(buf.len() / 2);
            self.inner.write_at(off, half)?;
            return Err(io::Error::other(format!("injected failure of write {n}")));
        }
        self.inner.write_at(off, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        if self.fail_set_len {
            return Err(io::Error::other("injected failure of set_len"));
        }
        self.inner.set_len(len)
    }

    fn sync(&self) -> io::Result<()> {
        if self.fail_sync {
            return Err(io::Error::other("injected failure of sync"));
        }
        self.inner.sync()
    }
}
