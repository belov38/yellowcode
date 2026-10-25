use super::ShellOptions;
use crate::{Error, Result};
use caseless::Caseless;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub(super) fn err(path: &Path, what: impl Into<String>) -> Error {
    Error::Xml {
        path: path.into(),
        what: what.into(),
    }
}
pub(super) fn nfc(s: &str) -> String {
    s.nfc().collect()
}
fn portable_fs_collision_key(name: &str) -> String {
    name.nfc().default_case_fold().nfc().collect()
}
pub(super) fn component(path: &Path, s: &str) -> Result<String> {
    let n = nfc(s);
    if n.is_empty()
        || n == "."
        || n == ".."
        || n.ends_with(['.', ' '])
        || n.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err(err(path, "invalid path component"));
    }
    Ok(n)
}
pub(super) fn inventory(root: &Path, opts: &ShellOptions) -> Result<BTreeMap<PathBuf, PathBuf>> {
    if opts.max_directory_depth > 256 || opts.max_metadata_depth > 64 || opts.max_xml_depth > 256 {
        return Err(err(root, "depth option exceeds safe limit"));
    }
    let meta = std::fs::symlink_metadata(root).map_err(|e| err(root, e.to_string()))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(err(root, "dump root must be a real directory"));
    }
    let mut files = BTreeMap::new();
    let mut keys = HashMap::new();
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];
    let mut entries = 0usize;
    let mut path_bytes = 0usize;
    while let Some((real, rel, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&real).map_err(|e| err(&real, e.to_string()))? {
            let entry = entry.map_err(|e| err(&real, e.to_string()))?;
            let path = entry.path();
            entries += 1;
            if entries > opts.max_entries {
                return Err(err(&path, "file/directory count limit"));
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| err(&path, "non-Unicode file name"))?;
            let key = rel.join(component(&path, name)?);
            let fold = portable_fs_collision_key(&key.to_string_lossy());
            path_bytes = path
                .as_os_str()
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(key.as_os_str().len()))
                .and_then(|n| n.checked_add(fold.len()))
                .and_then(|n| n.checked_add(path_bytes))
                .filter(|n| *n <= opts.max_inventory_path_bytes)
                .ok_or_else(|| err(&path, "inventory path byte limit"))?;
            if let Some(previous) = keys.insert(fold, path.clone()) {
                return Err(err(
                    &path,
                    format!("NFC/case collision with {}", previous.display()),
                ));
            }
            let ty = entry.file_type().map_err(|e| err(&path, e.to_string()))?;
            if ty.is_symlink() {
                return Err(err(&path, "symlink in dump"));
            }
            if ty.is_dir() {
                if depth + 1 > opts.max_directory_depth {
                    return Err(err(&path, "directory depth limit"));
                }
                stack.push((path, key, depth + 1));
            } else if ty.is_file() {
                files.insert(key, path);
            } else {
                return Err(err(&path, "special file in dump"));
            }
        }
    }
    Ok(files)
}
