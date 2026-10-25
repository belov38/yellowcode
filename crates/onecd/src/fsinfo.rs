use std::io;
use std::path::Path;

pub(crate) fn is_network_fs(path: &Path) -> io::Result<bool> {
    imp::is_network_fs(path)
}

const LINUX_NETWORK_MAGICS: &[(u32, &str)] = &[
    (0x0000_6969, "nfs"),
    (0x0000_517B, "smb"),
    (0xFF53_4D42, "cifs"),
    (0xFE53_4D42, "smb2"),
    (0x6573_5546, "fuse, also virtiofs"),
    (0x5346_414F, "afs"),
    (0x6B41_4653, "kafs"),
    (0x7375_7245, "coda"),
    (0x0102_1997, "9p, also WSL2 /mnt/c"),
    (0x00C3_6400, "ceph"),
    (0x0BD0_0BD0, "lustre"),
    (0x4750_4653, "gpfs"),
    (0x1983_0326, "beegfs"),
    (0x2003_0528, "orangefs"),
];

#[cfg_attr(
    not(any(test, target_os = "linux", target_os = "android")),
    allow(dead_code)
)]
fn linux_magic_is_network(magic: u32) -> bool {
    LINUX_NETWORK_MAGICS.iter().any(|&(m, _)| m == magic)
}

const MACOS_NETWORK_TYPES: &[&str] = &[
    "nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp", "osxfuse", "macfuse",
];

#[cfg_attr(not(any(test, target_vendor = "apple")), allow(dead_code))]
fn macos_type_is_network(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    MACOS_NETWORK_TYPES.contains(&name.as_str()) || name.starts_with("fuse")
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
enum WinRoot {
    Unc,
    Volume(String),
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn windows_root(path: &str) -> Option<WinRoot> {
    let p = path.replace('/', "\\");
    let starts = |prefix: &str| {
        p.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    let drive = |s: &str| -> Option<WinRoot> {
        let mut chars = s.chars();
        let (letter, colon, rest) = (chars.next()?, chars.next()?, chars.as_str());
        (letter.is_ascii_alphabetic()
            && colon == ':'
            && (rest.is_empty() || rest.starts_with('\\')))
        .then(|| WinRoot::Volume(format!("{}:\\", letter.to_ascii_uppercase())))
    };
    if starts(r"\\?\UNC\") || starts(r"\\.\UNC\") {
        return Some(WinRoot::Unc);
    }
    if starts(r"\\?\") || starts(r"\\.\") {
        let rest = p.get(4..)?;
        if let Some(root) = drive(rest) {
            return Some(root);
        }
        let first = rest.split('\\').next()?;
        let is_volume = first
            .get(..7)
            .is_some_and(|h| h.eq_ignore_ascii_case("Volume{"))
            && first.ends_with('}');
        return is_volume.then(|| WinRoot::Volume(format!(r"\\?\{first}\")));
    }
    if starts(r"\\") {
        return Some(WinRoot::Unc);
    }
    drive(&p)
}

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn windows_drive_is_network(drive_type: u32) -> Option<bool> {
    match drive_type {
        DRIVE_UNKNOWN | DRIVE_NO_ROOT_DIR => None,
        t => Some(t == DRIVE_REMOTE),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn is_network_fs(path: &Path) -> io::Result<bool> {
        let st = rustix::fs::statfs(path)?;
        #[allow(clippy::unnecessary_cast)]
        let magic = st.f_type as u32;
        Ok(super::linux_magic_is_network(magic))
    }
}

#[cfg(target_vendor = "apple")]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn is_network_fs(path: &Path) -> io::Result<bool> {
        let st = rustix::fs::statfs(path)?;
        let name: Vec<u8> = st
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        Ok(super::macos_type_is_network(&String::from_utf8_lossy(
            &name,
        )))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    use super::{WinRoot, windows_drive_is_network, windows_root};

    pub(super) fn is_network_fs(path: &Path) -> io::Result<bool> {
        let full = std::fs::canonicalize(path)?;
        match windows_root(&full.to_string_lossy()) {
            Some(WinRoot::Unc) => Ok(true),
            Some(WinRoot::Volume(root)) => {
                let wide: Vec<u16> = std::ffi::OsStr::new(&root)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                let t = unsafe { GetDriveTypeW(wide.as_ptr()) };
                windows_drive_is_network(t).ok_or_else(|| {
                    io::Error::other(format!("cannot tell the drive type of {root} ({t})"))
                })
            }
            None => Err(io::Error::other(format!(
                "cannot tell the volume of {}",
                full.display()
            ))),
        }
    }
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn is_network_fs(_path: &Path) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "network file system detection is not implemented on this OS",
        ))
    }
}
