use super::{metadata::Facts, *};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

#[derive(PartialEq, Eq)]
pub(super) struct Witness {
    identity: (u64, u64),
    length: u64,
    digest: [u8; 32],
}
struct Binding {
    nonce: String,
    path: PathBuf,
    witness: Witness,
}
static BINDINGS: OnceLock<Mutex<VecDeque<Binding>>> = OnceLock::new();
fn bindings() -> &'static Mutex<VecDeque<Binding>> {
    BINDINGS.get_or_init(|| Mutex::new(VecDeque::with_capacity(128)))
}
pub(super) fn nonce() -> String {
    v8users::new_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn path(base: &Path) -> Result<PathBuf, UserWriteError> {
    let path = if base.is_dir() {
        base.join("1Cv8.1CD")
    } else {
        base.to_path_buf()
    };
    if !path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("1cd"))
        || !path.is_file()
    {
        return Err(UserWriteError::UnsupportedSource);
    }
    std::fs::canonicalize(path).map_err(|_| UserWriteError::UnsupportedSource)
}
#[cfg(unix)]
pub(super) fn identity(file: &File) -> Result<(u64, u64), UserWriteError> {
    use std::os::unix::fs::MetadataExt;
    let m = file
        .metadata()
        .map_err(|_| UserWriteError::StorageBeforeCommit)?;
    Ok((m.dev(), m.ino()))
}
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn identity(file: &File) -> Result<(u64, u64), UserWriteError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(UserWriteError::UnsupportedSource);
    }
    let info = unsafe { info.assume_init() };
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
#[cfg(not(any(unix, windows)))]
pub(super) fn identity(_: &File) -> Result<(u64, u64), UserWriteError> {
    Err(UserWriteError::UnsupportedSource)
}
pub(super) fn witness(path: &Path) -> Result<Witness, UserWriteError> {
    let mut f = File::open(path).map_err(|_| UserWriteError::StorageBeforeCommit)?;
    let identity = identity(&f)?;
    if identity == (0, 0) {
        return Err(UserWriteError::UnsupportedSource);
    }
    let length = f
        .metadata()
        .map_err(|_| UserWriteError::StorageBeforeCommit)?
        .len();
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut read = 0u64;
    let mut bounded = (&mut f).take(length);
    loop {
        let n = bounded
            .read(&mut buf)
            .map_err(|_| UserWriteError::StorageBeforeCommit)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        digest.update(buf.get(..n).ok_or(UserWriteError::StorageBeforeCommit)?);
    }
    if read != length
        || f.metadata()
            .map_err(|_| UserWriteError::StorageBeforeCommit)?
            .len()
            != length
    {
        return Err(UserWriteError::StaleRevision);
    }
    Ok(Witness {
        identity,
        length,
        digest: digest.finalize().into(),
    })
}
pub(super) fn register(
    nonce: String,
    path: PathBuf,
    witness: Witness,
) -> Result<(), UserWriteError> {
    let mut b = bindings()
        .lock()
        .map_err(|_| UserWriteError::StorageBeforeCommit)?;
    if b.len() == 128 {
        b.pop_front();
    }
    b.push_back(Binding {
        nonce,
        path,
        witness,
    });
    Ok(())
}
pub(super) fn consume(nonce: &str, path: &Path, current: &Witness) -> Result<(), UserWriteError> {
    let mut b = bindings()
        .lock()
        .map_err(|_| UserWriteError::StorageBeforeCommit)?;
    let i = b
        .iter()
        .position(|b| b.nonce == nonce)
        .ok_or(UserWriteError::StaleRevision)?;
    let b = b.remove(i).ok_or(UserWriteError::StaleRevision)?;
    if b.path != path || &b.witness != current {
        return Err(UserWriteError::StaleRevision);
    }
    Ok(())
}
pub(super) fn load(base: &Path) -> Result<UserWriteSnapshot, UserWriteError> {
    let path = path(base)?;
    let db = Database::open(&path, OpenOptions::read_only())
        .map_err(|_| UserWriteError::StorageBeforeCommit)?;
    let before = witness(&path)?;
    let tx = db.read();
    let facts = Facts::read(&tx)?;
    let source_refusal = super::mutation::admit_source(&tx).err();
    let mut users = read_cards(&tx)
        .map_err(|_| UserWriteError::UnsupportedLayout("user DATA or collection limit"))?;
    resolve_roles(&mut users, &facts.cfg)
        .map_err(|_| UserWriteError::UnsupportedLayout("public output limit"))?;
    if witness(&path)? != before {
        return Err(UserWriteError::StaleRevision);
    }
    let revision = nonce();
    register(revision.clone(), path, before)?;
    Ok(UserWriteSnapshot {
        revision,
        users,
        roles: facts.roles,
        writable: cfg!(windows) && !facts.separated && source_refusal.is_none(),
        reason: if let Some(error) = source_refusal {
            Some(error.to_string())
        } else if !cfg!(windows) {
            Some(UserWriteError::UnsupportedPlatform.to_string())
        } else if facts.separated {
            Some(
                "global user semantics in a separated configuration require native qualification"
                    .into(),
            )
        } else {
            None
        },
    })
}
