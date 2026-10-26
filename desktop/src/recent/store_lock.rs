use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

pub(super) fn acquire(file: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(file.with_extension("json.lock"))?;
    lock_exclusive(&lock)?;
    Ok(lock)
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    file.lock()
}

#[cfg(windows)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    file.lock()
}
