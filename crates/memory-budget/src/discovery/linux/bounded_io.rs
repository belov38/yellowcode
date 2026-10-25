use std::{
    ffi::CStr,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Capacity,
    UnsafePath,
    Io(Option<i32>),
}
#[derive(Clone, Copy, Debug)]
pub enum ProcFile {
    Membership,
    Mountinfo,
    Meminfo,
}
impl ProcFile {
    fn path(self) -> &'static CStr {
        match self {
            Self::Membership => c"/proc/self/cgroup",
            Self::Mountinfo => c"/proc/self/mountinfo",
            Self::Meminfo => c"/proc/meminfo",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum KernelFile {
    MemoryMax,
    MemoryHigh,
    MemoryLimit,
    MemoryHierarchy,
    MemoryMemsw,
    MemoryStat,
}
impl KernelFile {
    fn name(self) -> &'static CStr {
        match self {
            Self::MemoryMax => c"memory.max",
            Self::MemoryHigh => c"memory.high",
            Self::MemoryLimit => c"memory.limit_in_bytes",
            Self::MemoryHierarchy => c"memory.use_hierarchy",
            Self::MemoryMemsw => c"memory.memsw.limit_in_bytes",
            Self::MemoryStat => c"memory.stat",
        }
    }
}
pub struct NativeBoundedReader;
impl NativeBoundedReader {
    pub fn read_proc(&self, file: ProcFile, output: &mut [u8]) -> Result<usize, Error> {
        read_bounded(open_at(libc::AT_FDCWD, file.path(), false)?, output)
    }

    pub fn read_cgroup(
        &self,
        mount: &Path,
        relative: &Path,
        file: KernelFile,
        output: &mut [u8],
        component: &mut [u8],
    ) -> Result<usize, Error> {
        let mount = mount.as_os_str().as_bytes();
        let relative = relative.as_os_str().as_bytes();
        if !mount.starts_with(b"/")
            || relative.starts_with(b"/")
            || !safe_path(mount)
            || !safe_path(relative)
        {
            return Err(Error::UnsafePath);
        }
        let mut anchor = open_at(libc::AT_FDCWD, c"/", true)?;
        for name in mount
            .split(|byte| *byte == b'/')
            .chain(relative.split(|byte| *byte == b'/'))
        {
            if name.is_empty() {
                continue;
            }
            let required = name.len().checked_add(1).ok_or(Error::Capacity)?;
            let terminated = component.get_mut(..required).ok_or(Error::Capacity)?;
            let (bytes, nul) = terminated.split_at_mut(name.len());
            bytes.copy_from_slice(name);
            nul[0] = 0;
            let name = CStr::from_bytes_with_nul(terminated).map_err(|_| Error::UnsafePath)?;
            anchor = open_at(anchor.as_raw_fd(), name, true)?;
        }
        read_bounded(open_at(anchor.as_raw_fd(), file.name(), false)?, output)
    }
}
fn safe_path(path: &[u8]) -> bool {
    !path.contains(&0)
        && !path
            .split(|byte| *byte == b'/')
            .any(|part| part == b"." || part == b"..")
}
fn open_at(parent: i32, name: &CStr, directory: bool) -> Result<OwnedFd, Error> {
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(Error::Io(std::io::Error::last_os_error().raw_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn read_once(file: &mut File, output: &mut [u8]) -> Result<usize, Error> {
    for _ in 0..16 {
        match file.read(output) {
            Ok(bytes) => return Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Error::Io(error.raw_os_error())),
        }
    }
    Err(Error::Io(Some(libc::EINTR)))
}
fn read_bounded(fd: OwnedFd, output: &mut [u8]) -> Result<usize, Error> {
    let mut file = File::from(fd);
    let mut used = 0;
    while used < output.len() {
        let bytes = read_once(&mut file, &mut output[used..])?;
        if bytes == 0 {
            return Ok(used);
        }
        used += bytes;
    }
    let mut extra = [0];
    if read_once(&mut file, &mut extra)? != 0 {
        return Err(Error::Capacity);
    }
    Ok(used)
}
