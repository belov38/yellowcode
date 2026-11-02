use crate::rel_path::{PathSet, check_rel, collision_key};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOptions {
    pub remove_extra: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub written: Vec<String>,
    pub unchanged: usize,
    pub removed: Vec<String>,
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    entries: usize,
    depth: usize,
}

const LIMITS: Limits = Limits {
    entries: 1_000_000,
    depth: 256,
};

const BUFFER: usize = 64 * 1024;

const TEMP_PREFIX: &str = ".yc-sync-";
const TEMP_SUFFIX: &str = ".tmp";

const MAX_COMPONENT: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
    Other,
    Kept,
}

#[derive(Debug)]
struct Entry {
    os: OsString,
    name: Option<String>,
    kind: Kind,
    children: Level,
    used: bool,
}

#[derive(Debug, Default)]
struct Level {
    entries: Vec<Entry>,
    by_key: BTreeMap<String, Vec<usize>>,
}

impl Entry {
    fn display(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.os.to_string_lossy().into_owned())
    }
}

fn refuse(entry: &str, what: impl Into<String>) -> Error {
    Error::Format {
        entry: entry.to_string(),
        what: what.into(),
    }
}

fn io_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    if e.get_ref().is_none() {
        return text;
    }
    match text.split_once(" at path \"") {
        Some((head, _)) => head.to_string(),
        None => text,
    }
}

fn io_error(entry: &str, action: &str, e: &std::io::Error) -> Error {
    Error::Io(std::io::Error::new(
        e.kind(),
        format!("{entry}: {action}: {}", io_text(e)),
    ))
}

fn join(parent: &str, name: &str) -> String {
    if parent == "." {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn lstat(path: &Path, entry: &str) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(entry, "cannot inspect", &e)),
    }
}

fn is_device_stem(stem: &str) -> bool {
    let upper = stem.trim_end_matches(' ').to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    ["COM", "LPT"].iter().any(|prefix| {
        let mut rest = upper.strip_prefix(prefix).unwrap_or_default().chars();
        match (rest.next(), rest.next()) {
            (Some(c), None) => c.is_ascii_digit() || matches!(c, '¹' | '²' | '³'),
            _ => false,
        }
    })
}

fn check_portable(rel: &str) -> Result<()> {
    for c in rel.split('/') {
        if c.starts_with('.') {
            return Err(refuse(
                rel,
                "component starts with a dot (reserved for files kept outside the dump)",
            ));
        }
        if c.len() > MAX_COMPONENT || c.encode_utf16().count() > MAX_COMPONENT {
            return Err(refuse(
                rel,
                format!(
                    "component is longer than {MAX_COMPONENT} UTF-8 bytes or UTF-16 code units"
                ),
            ));
        }
        if c.chars().any(|ch| ch < ' ' || "<>\"|?*".contains(ch)) {
            return Err(refuse(rel, "component has a character Windows refuses"));
        }
        if c.ends_with(['.', ' ']) {
            return Err(refuse(rel, "component ends with a dot or a space"));
        }
        if is_device_stem(c.split('.').next().unwrap_or(c)) {
            return Err(refuse(rel, "component is a Windows device name"));
        }
    }
    Ok(())
}

fn is_temp(name: &str) -> bool {
    name.strip_prefix(TEMP_PREFIX)
        .is_some_and(|rest| rest.ends_with(TEMP_SUFFIX))
}

fn scan(dir: &Path, rel: &str, depth: usize, limits: Limits, count: &mut usize) -> Result<Level> {
    let mut out = Level::default();
    let list = std::fs::read_dir(dir).map_err(|e| io_error(rel, "cannot list", &e))?;
    for item in list {
        let item = item.map_err(|e| io_error(rel, "cannot list", &e))?;
        let os = item.file_name();
        let name = os.to_str().map(|s| s.nfc().collect::<String>());
        let shown = name
            .clone()
            .unwrap_or_else(|| os.to_string_lossy().into_owned());
        let path = join(rel, &shown);
        *count += 1;
        if *count > limits.entries {
            return Err(refuse(
                ".",
                format!("more than {} existing entries", limits.entries),
            ));
        }
        if depth >= limits.depth {
            return Err(refuse(
                &path,
                format!("deeper than {} levels", limits.depth),
            ));
        }
        let ty = item
            .file_type()
            .map_err(|e| io_error(&path, "cannot inspect", &e))?;
        let kind = if ty.is_symlink() {
            Kind::Link
        } else if ty.is_dir() {
            Kind::Dir
        } else if ty.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let temp = kind == Kind::File && name.as_deref().is_some_and(is_temp);
        let kind = if shown.starts_with('.') && !temp {
            Kind::Kept
        } else {
            kind
        };
        let children = if kind == Kind::Dir {
            scan(&dir.join(&os), &path, depth + 1, limits, count)?
        } else {
            Level::default()
        };
        if let Some(name) = &name {
            out.by_key
                .entry(collision_key(name))
                .or_default()
                .push(out.entries.len());
        }
        out.entries.push(Entry {
            os,
            name,
            kind,
            children,
            used: false,
        });
    }
    Ok(out)
}

fn claim(root: &Path, mut level: &mut Level, rel: &str) -> Result<bool> {
    let mut path = root.to_path_buf();
    let mut disk = String::from(".");
    let mut spelled = String::from(".");
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        path.push(part);
        spelled = join(&spelled, part);
        let (i, second) = match level.by_key.get(&collision_key(part)).map(Vec::as_slice) {
            Some([i]) => (*i, None),
            Some([i, j, ..]) => (*i, Some(*j)),
            _ => return Ok(false),
        };
        if let Some(j) = second {
            let shown = |k: usize| {
                let name = level.entries.get(k).map(Entry::display).unwrap_or_default();
                join(&disk, &name)
            };
            return Err(refuse(
                rel,
                format!(
                    "existing {} and {} are one name on case-insensitive or normalizing file systems",
                    shown(i),
                    shown(j)
                ),
            ));
        }
        let current = level;
        let Some(entry) = current.entries.get_mut(i) else {
            return Ok(false);
        };
        let shown = join(&disk, &entry.display());
        if entry.os != part {
            let nfc = part.nfc().collect::<String>();
            let normalized = entry.name.as_deref() == Some(nfc.as_str());
            if !(normalized && lstat(&path, rel)?.is_some()) {
                let differ = if normalized {
                    "Unicode normalization"
                } else {
                    "letter case"
                };
                return Err(refuse(
                    rel,
                    format!(
                        "existing {shown} and key spelling {spelled} differ only in {differ}; rename or remove the \
                         existing entry and sync again (remove_extra does not resolve it)"
                    ),
                ));
            }
        }
        match (entry.kind, last) {
            (Kind::Dir, false) => {
                entry.used = true;
                disk = shown;
                level = &mut entry.children;
            }
            (Kind::File, true) => {
                entry.used = true;
                return Ok(true);
            }
            (Kind::Link, _) => return Err(refuse(rel, format!("existing {shown} is a symlink"))),
            (Kind::Kept, _) => {
                return Err(refuse(
                    rel,
                    format!("existing {shown} is kept outside the dump"),
                ));
            }
            (Kind::Other, _) => {
                return Err(refuse(
                    rel,
                    format!("existing {shown} is not a file or a directory"),
                ));
            }
            (Kind::Dir, true) => {
                return Err(refuse(rel, format!("existing {shown} is a directory")));
            }
            (Kind::File, false) => {
                return Err(refuse(rel, format!("existing {shown} is not a directory")));
            }
        }
    }
    Ok(false)
}

fn check_root(dir: &Path) -> Result<Option<()>> {
    match lstat(dir, ".")? {
        None => Ok(None),
        Some(m) if m.is_dir() => Ok(Some(())),
        Some(m) if m.file_type().is_symlink() => Err(refuse(".", "output directory is a symlink")),
        Some(_) => Err(refuse(".", "output is not a directory")),
    }
}

fn prepare(root: &Path, rel: &str) -> Result<PathBuf> {
    if check_root(root)?.is_none() {
        return Err(refuse(".", "output directory disappeared"));
    }
    let mut path = root.to_path_buf();
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        if parts.peek().is_none() {
            break;
        }
        if lstat(&path, rel)?.is_none() {
            match std::fs::create_dir(&path) {
                Ok(()) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_error(rel, "cannot create a directory", &e)),
            }
        }
        if !lstat(&path, rel)?.is_some_and(|m| m.is_dir()) {
            return Err(refuse(rel, "an ancestor is no longer a directory"));
        }
    }
    Ok(path)
}

fn same_content(path: &Path, len: u64, data: &[u8]) -> std::io::Result<bool> {
    if u64::try_from(data.len()).ok() != Some(len) {
        return Ok(false);
    }
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; BUFFER];
    let mut rest = data;
    loop {
        let n = match file.read(&mut buffer) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(rest.is_empty());
        }
        let (Some(block), Some((head, tail))) = (buffer.get(..n), rest.split_at_checked(n)) else {
            return Ok(false);
        };
        if block != head {
            return Ok(false);
        }
        rest = tail;
    }
}

fn replace(path: &Path, rel: &str, data: &[u8], old: Option<&std::fs::Metadata>) -> Result<()> {
    let fail = |action: &str, e: &std::io::Error| io_error(rel, action, e);
    let path = &long_path(path).map_err(|e| fail("cannot resolve", &e))?;
    let parent = path
        .parent()
        .ok_or_else(|| refuse(rel, "target has no parent directory"))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(TEMP_PREFIX).suffix(TEMP_SUFFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut temp = builder
        .tempfile_in(parent)
        .map_err(|e| fail("cannot create a temporary file", &e))?;
    let file = temp.as_file_mut();
    file.write_all(data).map_err(|e| fail("cannot write", &e))?;
    if let Some(old) = old {
        file.set_permissions(old.permissions())
            .map_err(|e| fail("cannot keep permissions", &e))?;
    }
    if old.is_some() {
        file.sync_all().map_err(|e| fail("cannot flush", &e))?;
    }
    temp.persist(path)
        .map_err(|e| fail("cannot replace", &e.error))?;
    Ok(())
}

#[cfg(windows)]
fn long_path(path: &Path) -> std::io::Result<PathBuf> {
    use std::path::{Component, Prefix};
    let path = std::path::absolute(path)?;
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Ok(path);
    };
    let mut long = OsString::from(r"\\?\");
    match prefix.kind() {
        Prefix::Disk(_) => long.push(prefix.as_os_str()),
        Prefix::UNC(server, share) => {
            long.push(r"UNC\");
            long.push(server);
            long.push(r"\");
            long.push(share);
        }
        _ => return Ok(path),
    }
    long.push(parts.as_path());
    Ok(PathBuf::from(long))
}

#[cfg(not(windows))]
fn long_path(path: &Path) -> std::io::Result<PathBuf> {
    Ok(path.to_path_buf())
}

fn sweep(
    dir: &Path,
    rel: &str,
    entries: &Level,
    remove: bool,
    report: &mut SyncReport,
) -> Result<bool> {
    if remove && !lstat(dir, rel)?.is_some_and(|m| m.is_dir()) {
        return Err(refuse(rel, "directory is no longer a directory"));
    }
    let mut empty = true;
    for entry in &entries.entries {
        let path = dir.join(&entry.os);
        let shown = join(rel, &entry.display());
        match entry.kind {
            Kind::Dir => {
                let gone = sweep(&path, &shown, &entry.children, remove, report)?;
                if gone && !entry.used && remove {
                    std::fs::remove_dir(&path)
                        .map_err(|e| io_error(&shown, "cannot remove", &e))?;
                } else {
                    if gone && !entry.used {
                        report.extra.push(shown);
                    }
                    empty = false;
                }
            }
            Kind::File if entry.used => empty = false,
            Kind::File if remove => {
                if !lstat(&path, &shown)?.is_some_and(|m| m.is_file()) {
                    return Err(refuse(&shown, "extra file is no longer a plain file"));
                }
                std::fs::remove_file(&path).map_err(|e| io_error(&shown, "cannot remove", &e))?;
                report.removed.push(shown);
            }
            Kind::File | Kind::Link | Kind::Other | Kind::Kept => {
                report.extra.push(shown);
                empty = false;
            }
        }
    }
    Ok(empty)
}

pub fn sync_dir(
    files: &BTreeMap<String, Vec<u8>>,
    dir: &Path,
    opts: SyncOptions,
) -> Result<SyncReport> {
    sync_with_limits(files, dir, opts, LIMITS)
}

fn sync_with_limits(
    files: &BTreeMap<String, Vec<u8>>,
    dir: &Path,
    opts: SyncOptions,
    limits: Limits,
) -> Result<SyncReport> {
    let exists = check_root(dir)?.is_some();
    let mut keys = PathSet::default();
    for rel in files.keys() {
        check_rel(rel)?;
        check_portable(rel)?;
        let checked = keys.check(rel)?;
        keys.insert(rel, checked);
    }
    let mut tree = if exists {
        scan(dir, ".", 0, limits, &mut 0)?
    } else {
        Level::default()
    };
    let mut plan = Vec::with_capacity(files.len());
    for (rel, data) in files {
        if !claim(dir, &mut tree, rel)? {
            plan.push(Some(None));
            continue;
        }
        let path = rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
        let Some(old) = lstat(&path, rel)?.filter(|m| m.is_file()) else {
            return Err(refuse(rel, "target is no longer a plain file"));
        };
        if same_content(&path, old.len(), data).map_err(|e| io_error(rel, "cannot compare", &e))? {
            plan.push(None);
        } else if old.permissions().readonly() {
            return Err(refuse(rel, "read-only file cannot be replaced"));
        } else {
            plan.push(Some(Some(old)));
        }
    }

    if !exists {
        std::fs::create_dir_all(dir).map_err(|e| io_error(".", "cannot create", &e))?;
    }
    let mut report = SyncReport::default();
    for ((rel, data), step) in files.iter().zip(plan) {
        let Some(old) = step else {
            report.unchanged += 1;
            continue;
        };
        let path = prepare(dir, rel)?;
        if lstat(&path, rel)?.is_some_and(|m| !m.is_file()) {
            return Err(refuse(rel, "target is no longer a plain file"));
        }
        replace(&path, rel, data, old.as_ref())?;
        report.written.push(rel.clone());
    }

    sweep(dir, ".", &tree, opts.remove_extra, &mut report)?;
    report.removed.sort();
    report.extra.sort();
    Ok(report)
}
