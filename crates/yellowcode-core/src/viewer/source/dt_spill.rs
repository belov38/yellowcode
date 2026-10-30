use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
const PREFIX: &str = ".yellowcode-dt-spill-";

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn try_lock(file: &File) -> Result<(), std::fs::TryLockError> {
    file.try_lock()
}

#[cfg(unix)]
fn clean_pending(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() == 0 => meta,
            _ => continue,
        };
        let file = match rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(_) => continue,
        };
        if try_lock(&file).is_err() {
            continue;
        }
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if opened.ino() != meta.ino() || opened.dev() != meta.dev() || opened.len() != 0 {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unlink_pending(named: tempfile::NamedTempFile) -> io::Result<File> {
    named.as_file().lock()?;
    let (file, path) = named.into_parts();
    match path.close() {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(file)
}

#[cfg(unix)]
pub(super) fn create(dir: &Path) -> io::Result<File> {
    clean_pending(dir)?;
    unlink_pending(tempfile::Builder::new().prefix(PREFIX).tempfile_in(dir)?)
}

#[cfg(windows)]
pub(super) fn create(dir: &Path) -> io::Result<File> {
    tempfile::tempfile_in(dir)
}

#[cfg(unix)]
pub(super) fn available(file: &File, _dir: &Path) -> io::Result<u64> {
    let info = rustix::fs::fstatvfs(file)?;
    info.f_bavail
        .checked_mul(info.f_frsize)
        .ok_or_else(|| io::Error::other("DT spill available disk size overflow"))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn available(_file: &File, dir: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut bytes = 0;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(bytes)
    }
}

pub(super) fn admit(file: &File, dir: &Path, bytes: u64) -> io::Result<()> {
    if available(file, dir)? < bytes {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "DT CONFIG spill: insufficient free disk space",
        ));
    }
    Ok(())
}
