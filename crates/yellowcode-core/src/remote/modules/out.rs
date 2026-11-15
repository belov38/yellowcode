use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::Result;

const MAX_NAME_BYTES: usize = 255;

const EXT: &str = ".bsl";

pub(super) fn file_names(names: &[&str]) -> std::result::Result<Vec<String>, String> {
    let mut seen: HashMap<String, &str> = HashMap::new();
    let mut files = Vec::with_capacity(names.len());
    for name in names {
        let nfc: String = name.nfc().collect();
        let file = format!("{nfc}{EXT}");
        portable_stem(&nfc).map_err(|why| format!("module {name:?}: {why}"))?;
        if file.len() > MAX_NAME_BYTES || file.encode_utf16().count() > MAX_NAME_BYTES {
            return Err(format!(
                "module {name:?}: the file name takes {} bytes, more than {MAX_NAME_BYTES}",
                file.len()
            ));
        }
        if let Some(other) = seen.insert(fold_case(&nfc), name) {
            return Err(format!(
                "modules {other:?} and {name:?} are one file name on a case-insensitive file system"
            ));
        }
        files.push(file);
    }
    Ok(files)
}

fn fold_case(name: &str) -> String {
    name.to_uppercase().to_lowercase()
}

const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_device(stem: &str) -> bool {
    let base = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    DEVICES.contains(&upper.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            upper.strip_prefix(p).is_some_and(|n| {
                let mut c = n.chars();
                matches!(
                    (c.next(), c.next()),
                    (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
                )
            })
        })
}

fn portable_stem(stem: &str) -> std::result::Result<(), &'static str> {
    if stem.is_empty() {
        return Err("an empty name is not a file name");
    }
    if stem.chars().any(|c| {
        c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err("the name holds a character a file name cannot hold");
    }
    if stem.starts_with('.') || stem.ends_with(' ') {
        return Err("a file name cannot start with a dot or end with a space");
    }
    if is_device(stem) {
        return Err("the name is a device name on Windows");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum OutDir {
    Missing,
    Empty,
}

pub(super) fn check_output_dir(dir: &Path) -> Result<OutDir> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => Err(format!(
            "output is a symbolic link; give the directory itself: {}",
            dir.display()
        )
        .into()),
        Ok(m) if m.is_dir() => {
            let used = std::fs::read_dir(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .next()
                .is_some();
            if used {
                return Err(format!("output directory is not empty: {}", dir.display()).into());
            }
            Ok(OutDir::Empty)
        }
        Ok(_) => Err(format!("output is not a directory: {}", dir.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OutDir::Missing),
        Err(e) => Err(format!("{}: {e}", dir.display()).into()),
    }
}

type WriteFile<'a> = &'a mut dyn FnMut(&Path, &[u8]) -> std::io::Result<()>;

pub(super) fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

pub(super) fn write_all(
    dir: &Path,
    files: &[(&str, &[u8])],
    write: &mut dyn FnMut(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<()> {
    match check_output_dir(dir)? {
        OutDir::Missing => into_new(dir, files, write),
        OutDir::Empty => into_empty(dir, files, write),
    }
}

fn into_new(dir: &Path, files: &[(&str, &[u8])], write: WriteFile) -> Result<()> {
    into_new_with(dir, files, write, &mut unique_suffix)
}

const STAGE_TRIES: usize = 8;

fn into_new_with(
    dir: &Path,
    files: &[(&str, &[u8])],
    write: WriteFile,
    suffix: &mut dyn FnMut() -> String,
) -> Result<()> {
    let dir = std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = dir.as_path();
    let (Some(name), Some(parent)) = (dir.file_name(), dir.parent()) else {
        return Err(format!("output path has no directory name: {}", dir.display()).into());
    };
    let parent = parent.to_path_buf();
    let created = create_parents(&parent)?;
    let remove_parents = || {
        for p in created.iter().rev() {
            let _ = std::fs::remove_dir(p);
        }
    };
    let stage = match create_stage(&parent, &name.to_string_lossy(), suffix) {
        Ok(stage) => stage,
        Err(e) => {
            remove_parents();
            return Err(e.into());
        }
    };
    let outcome = write_files(&stage, files, write)
        .and_then(|()| sync_dir(&stage))
        .and_then(|()| std::fs::rename(&stage, dir).map_err(|e| format!("{}: {e}", dir.display())))
        .and_then(|()| sync_dir(&parent));
    if let Err(e) = outcome {
        let _ = std::fs::remove_dir_all(&stage);
        remove_parents();
        return Err(e.into());
    }
    Ok(())
}

fn create_stage(
    parent: &Path,
    name: &str,
    suffix: &mut dyn FnMut() -> String,
) -> std::result::Result<PathBuf, String> {
    let mut last = None;
    for _ in 0..STAGE_TRIES {
        let stage = parent.join(format!(".{name}.onec-tmp-{}", suffix()));
        match std::fs::create_dir(&stage) {
            Ok(()) => return Ok(stage),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(stage),
            Err(e) => return Err(format!("{}: {e}", stage.display())),
        }
    }
    Err(format!(
        "no free name for the stage of the output after {STAGE_TRIES} tries, the last {}",
        last.map_or_else(String::new, |p| p.display().to_string())
    ))
}

fn into_empty(dir: &Path, files: &[(&str, &[u8])], write: WriteFile) -> Result<()> {
    let mut written = Vec::new();
    let mut outcome = Ok(());
    for (name, bytes) in files {
        let path = dir.join(name);
        match write(&path, bytes) {
            Ok(()) => written.push(path),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::AlreadyExists && path.exists() {
                    written.push(path.clone());
                }
                outcome = Err(format!("{}: {e}", path.display()));
                break;
            }
        }
    }
    let outcome = outcome.and_then(|()| sync_dir(dir));
    if let Err(e) = outcome {
        let mut left = Vec::new();
        for p in &written {
            if let Err(r) = std::fs::remove_file(p) {
                left.push(format!("{}: {r}", p.display()));
            }
        }
        if left.is_empty() {
            return Err(e.into());
        }
        return Err(format!("{e}; could not remove {}", left.join(", ")).into());
    }
    Ok(())
}

fn write_files(
    dir: &Path,
    files: &[(&str, &[u8])],
    write: WriteFile,
) -> std::result::Result<(), String> {
    for (name, bytes) in files {
        let path = dir.join(name);
        write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn create_parents(parent: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let mut missing = Vec::new();
    let mut p = Some(parent);
    while let Some(cur) = p {
        if std::fs::symlink_metadata(cur).is_ok() {
            break;
        }
        missing.push(cur.to_path_buf());
        p = cur.parent();
    }
    missing.reverse();
    let mut created = Vec::new();
    for m in missing {
        if let Err(e) = std::fs::create_dir(&m) {
            for c in created.iter().rev() {
                let _ = std::fs::remove_dir(c);
            }
            return Err(format!("{}: {e}", m.display()));
        }
        created.push(m);
    }
    Ok(created)
}

fn unique_suffix() -> String {
    use std::hash::BuildHasher;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let random = std::hash::RandomState::new().hash_one(nanos);
    format!("{}-{random:016x}", std::process::id())
}

fn sync_dir(dir: &Path) -> std::result::Result<(), String> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| format!("{}: {e}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}
