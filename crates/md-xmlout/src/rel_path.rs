use crate::{Error, Result};
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn bad_rel(rel: &str, what: &str) -> Error {
    Error::Format {
        entry: rel.to_string(),
        what: what.to_string(),
    }
}

pub(crate) fn check_rel(rel: &str) -> Result<()> {
    if rel.is_empty() {
        return Err(bad_rel(rel, "empty path"));
    }
    for c in rel.split('/') {
        if c.is_empty() || c == "." || c == ".." {
            return Err(bad_rel(rel, "bad path component"));
        }
        if c.contains(['\\', ':', '\0']) {
            return Err(bad_rel(rel, "component is not a plain file name"));
        }
    }
    Ok(())
}

pub(crate) fn collision_key(rel: &str) -> String {
    rel.nfc()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug, Default)]
pub(crate) struct PathSet {
    files: BTreeMap<String, String>,
    dirs: BTreeMap<String, (String, String)>,
}

#[derive(Debug)]
pub(crate) struct Checked {
    key: String,
    new_dirs: Vec<(String, String)>,
}

impl PathSet {
    pub(crate) fn check(&self, rel: &str) -> Result<Checked> {
        const WHY: &str = "one name on case-insensitive or normalizing file systems";
        let key = collision_key(rel);
        if let Some((dir, first)) = self.dirs.get(&key) {
            return Err(bad_rel(
                rel,
                &format!("file collides with directory {dir} of {first}: {WHY}"),
            ));
        }
        if let Some(seen) = self.files.get(&key)
            && seen != rel
        {
            return Err(bad_rel(rel, &format!("collides with {seen}: {WHY}")));
        }
        let mut new_dirs = Vec::new();
        for (at, _) in rel.match_indices('/') {
            let dir = rel.get(..at).unwrap_or_default();
            let dir_key = collision_key(dir);
            if let Some(seen) = self.files.get(&dir_key) {
                return Err(bad_rel(
                    rel,
                    &format!("directory {dir} collides with file {seen}: {WHY}"),
                ));
            }
            match self.dirs.get(&dir_key) {
                Some((seen, first)) if seen != dir => {
                    return Err(bad_rel(
                        rel,
                        &format!("directory {dir} collides with {seen} of {first}: {WHY}"),
                    ));
                }
                Some(_) => {}
                None => new_dirs.push((dir_key, dir.to_string())),
            }
        }
        Ok(Checked { key, new_dirs })
    }

    pub(crate) fn insert(&mut self, rel: &str, checked: Checked) {
        for (dir_key, dir) in checked.new_dirs {
            self.dirs.insert(dir_key, (dir, rel.to_string()));
        }
        self.files.insert(checked.key, rel.to_string());
    }
}
