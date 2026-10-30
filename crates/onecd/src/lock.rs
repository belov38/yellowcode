use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::db::Mode;
use crate::error::{Error, LockedBy, Result};

const COMPANION_SUFFIX: &str = ".cfl";
const ONEC_LOCK_EXTENSION: &str = "1CL";

#[derive(Debug)]
pub(crate) struct DbLock {
    file: Option<File>,
    companion: Option<File>,
    _registered: Registration,
}

impl DbLock {
    #[cfg(unix)]
    pub(crate) fn acquire_file(db_path: &Path, file: File, mode: Mode) -> Result<DbLock> {
        use std::os::unix::fs::MetadataExt;
        let path = database_path(db_path, mode)?;
        let named = fs::metadata(&path)?;
        let opened = file.metadata()?;
        if (named.dev(), named.ino()) != (opened.dev(), opened.ino()) {
            return Err(locked(db_path));
        }
        let registered = Registration::new(path.clone(), mode)?;
        let (companion, file) = os::lock_file(&path, mode, file)?;
        Ok(DbLock {
            file: Some(file),
            companion,
            _registered: registered,
        })
    }
    pub(crate) fn acquire(db_path: &Path, mode: Mode) -> Result<DbLock> {
        let path = database_path(db_path, mode)?;
        let registered = Registration::new(path.clone(), mode)?;
        let (companion, file) = os::lock(&path, mode)?;
        Ok(DbLock {
            file: Some(file),
            companion,
            _registered: registered,
        })
    }

    pub(crate) fn file(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("the database file is closed"))
    }
}

impl Drop for DbLock {
    fn drop(&mut self) {
        drop(self.file.take());
        drop(self.companion.take());
    }
}

fn database_path(db_path: &Path, mode: Mode) -> io::Result<PathBuf> {
    resolve_path(fs::canonicalize(db_path), db_path, mode, cfg!(windows))
}

fn resolve_path(
    canonical: io::Result<PathBuf>,
    db_path: &Path,
    mode: Mode,
    fallback: bool,
) -> io::Result<PathBuf> {
    match canonical {
        Ok(p) => Ok(p),
        Err(e) if fallback && mode == Mode::ReadOnly => std::path::absolute(db_path).map_err(|_| e),
        Err(e) => Err(e),
    }
}

fn companion_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(COMPANION_SUFFIX);
    PathBuf::from(name)
}

fn locked(db: &Path) -> Error {
    let onec_lock_file = db.with_extension(ONEC_LOCK_EXTENSION);
    Error::Locked {
        by: holder(onec_lock_file.try_exists().unwrap_or(false)),
    }
}

fn holder(onec_lock_file_exists: bool) -> LockedBy {
    if onec_lock_file_exists {
        LockedBy::OneC
    } else {
        LockedBy::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holders {
    Writer,
    Readers(usize),
}

static OPEN: Mutex<BTreeMap<PathBuf, Holders>> = Mutex::new(BTreeMap::new());

#[derive(Debug)]
struct Registration {
    path: PathBuf,
}

impl Registration {
    fn new(path: PathBuf, mode: Mode) -> Result<Registration> {
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        let next = match (open.get(&path), mode) {
            (None, Mode::ReadWrite) => Holders::Writer,
            (None, Mode::ReadOnly) => Holders::Readers(1),
            (Some(Holders::Readers(n)), Mode::ReadOnly) => Holders::Readers(n.saturating_add(1)),
            (Some(_), _) => return Err(Error::Locked { by: LockedBy::Ours }),
        };
        open.insert(path.clone(), next);
        Ok(Registration { path })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        match open.get_mut(&self.path) {
            Some(Holders::Readers(n)) if *n > 1 => *n -= 1,
            _ => {
                open.remove(&self.path);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
enum Refusal {
    Busy,
    Retry,
    Fail,
}

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn windows_refusal(code: Option<i32>, companion: bool) -> Refusal {
    match code {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => Refusal::Busy,
        Some(ERROR_ACCESS_DENIED | ERROR_FILE_NOT_FOUND) if companion => Refusal::Retry,
        _ => Refusal::Fail,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
enum Probe {
    Clear,
    Busy,
    Again,
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn reader_probe(code: Option<i32>, retries: u32) -> Probe {
    if code == Some(ERROR_FILE_NOT_FOUND) {
        return Probe::Clear;
    }
    match windows_refusal(code, true) {
        Refusal::Busy => Probe::Busy,
        Refusal::Retry if retries < COMPANION_RETRIES => Probe::Again,
        Refusal::Retry | Refusal::Fail => Probe::Clear,
    }
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
const COMPANION_RETRIES: u32 = 10;
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
const COMPANION_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

#[cfg(windows)]
mod os {
    use std::fs::{self, File};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_READ,
    };

    use super::{
        COMPANION_RETRIES, COMPANION_RETRY_PAUSE, Probe, Refusal, companion_path, locked,
        reader_probe, windows_refusal,
    };
    use crate::db::Mode;
    use crate::error::{Error, Result};

    pub(super) fn lock(db: &Path, mode: Mode) -> Result<(Option<File>, File)> {
        let companion = match mode {
            Mode::ReadWrite => Some(open_companion(db)?),
            Mode::ReadOnly => {
                probe_companion(db)?;
                None
            }
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .write(mode == Mode::ReadWrite)
            .share_mode(FILE_SHARE_READ)
            .open(db)
            .map_err(|e| match windows_refusal(e.raw_os_error(), false) {
                Refusal::Busy => locked(db),
                Refusal::Retry | Refusal::Fail => Error::Io(e),
            })?;
        Ok((companion, file))
    }

    fn open_companion(db: &Path) -> Result<File> {
        let path = companion_path(db);
        let mut retries = 0;
        loop {
            let opened = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
                .share_mode(0)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
                .open(&path);
            let e = match opened {
                Ok(f) => return Ok(f),
                Err(e) => e,
            };
            match windows_refusal(e.raw_os_error(), true) {
                Refusal::Busy => return Err(locked(db)),
                Refusal::Retry if retries < COMPANION_RETRIES => {
                    retries += 1;
                    std::thread::sleep(COMPANION_RETRY_PAUSE);
                }
                Refusal::Retry | Refusal::Fail => return Err(Error::Io(e)),
            }
        }
    }

    fn probe_companion(db: &Path) -> Result<()> {
        let path = companion_path(db);
        let mut retries = 0;
        loop {
            let e = match fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)
            {
                Ok(_) => return Ok(()),
                Err(e) => e,
            };
            match reader_probe(e.raw_os_error(), retries) {
                Probe::Clear => return Ok(()),
                Probe::Busy => return Err(locked(db)),
                Probe::Again => {
                    retries += 1;
                    std::thread::sleep(COMPANION_RETRY_PAUSE);
                }
            }
        }
    }
}

#[cfg(unix)]
mod os {
    use std::fs::{self, File, TryLockError};
    use std::io;
    use std::path::Path;

    use super::{companion_path, locked};
    use crate::db::Mode;
    use crate::error::{Error, Result};

    pub(super) fn lock(db: &Path, mode: Mode) -> Result<(Option<File>, File)> {
        lock_with(db, mode, || {
            fs::OpenOptions::new()
                .read(true)
                .write(mode != Mode::ReadOnly)
                .open(db)
        })
    }

    pub(super) fn lock_file(db: &Path, mode: Mode, file: File) -> Result<(Option<File>, File)> {
        lock_with(db, mode, || Ok(file))
    }

    fn lock_with(
        db: &Path,
        mode: Mode,
        open: impl FnOnce() -> io::Result<File>,
    ) -> Result<(Option<File>, File)> {
        let reader = mode == Mode::ReadOnly;
        let companion = if reader {
            open_existing_companion(db)
        } else {
            match open_companion(db) {
                Ok(f) => Some(f),
                Err(e) => return Err(Error::Io(e)),
            }
        };
        let companion = match companion {
            Some(f) => {
                let taken = if reader {
                    f.try_lock_shared()
                } else {
                    f.try_lock()
                };
                match taken {
                    Ok(()) => {
                        if !reader {
                            native_writer_lock(&f, db)?;
                        }
                        Some(f)
                    }
                    Err(TryLockError::WouldBlock) => return Err(locked(db)),
                    Err(TryLockError::Error(_)) if reader => None,
                    Err(TryLockError::Error(e)) => return Err(Error::Io(e)),
                }
            }
            None => None,
        };
        let file = open()?;
        let taken = if reader {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match taken {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(locked(db)),
            Err(TryLockError::Error(_)) if reader => {}
            Err(TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        if !reader {
            native_writer_lock(&file, db)?;
        }
        Ok((companion, file))
    }

    #[cfg(target_os = "linux")]
    #[allow(unsafe_code)]
    fn native_writer_lock(file: &File, db: &Path) -> Result<()> {
        use std::os::fd::AsRawFd;
        let mut lock = libc::flock {
            l_type: libc::F_WRLCK as _,
            l_whence: libc::SEEK_SET as _,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &mut lock) } == -1 {
            let error = io::Error::last_os_error();
            return Err(match error.raw_os_error() {
                Some(libc::EAGAIN | libc::EACCES) => locked(db),
                _ => Error::Io(error),
            });
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn native_writer_lock(_file: &File, _db: &Path) -> Result<()> {
        Ok(())
    }

    fn open_existing_companion(db: &Path) -> Option<File> {
        File::open(companion_path(db)).ok()
    }

    fn open_companion(db: &Path) -> io::Result<File> {
        let path = companion_path(db);
        match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
        {
            Ok(f) => Ok(f),
            Err(e)
                if !cfg!(target_os = "linux")
                    && matches!(
                        e.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                    ) =>
            {
                File::open(&path).map_err(|_| e)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use crate::db::Mode;
    use crate::error::{Error, Result};

    pub(super) fn lock(_db: &Path, _mode: Mode) -> Result<(Option<File>, File)> {
        Err(Error::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "database locks are not implemented on this OS; open with Locking::None",
        )))
    }
}
