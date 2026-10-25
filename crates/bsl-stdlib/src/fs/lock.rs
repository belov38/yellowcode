use bsl_vm::{AppHost, FileSystem};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard, OnceLock};

type Key = (usize, String);

fn table() -> MutexGuard<'static, HashSet<Key>> {
    static T: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn key(fsys: &dyn FileSystem, path: &str) -> Key {
    let id = std::ptr::from_ref(fsys).cast::<()>() as usize;
    let mut p = path.replace('/', "\\");
    let absolute = p.starts_with('\\') || p.as_bytes().get(1) == Some(&b':');
    if !absolute {
        p = format!("{}{p}", fsys.temp_dir());
    }
    (id, p.chars().flat_map(char::to_lowercase).collect())
}

pub(crate) struct FileLock(Key, #[allow(dead_code)] Arc<dyn AppHost>);

impl FileLock {
    pub(crate) fn acquire(app: &Arc<dyn AppHost>, path: &str) -> Option<FileLock> {
        let k = key(app.file_system()?, path);
        let free = table().insert(k.clone());
        free.then(|| FileLock(k, app.clone()))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        table().remove(&self.0);
    }
}

pub(crate) fn is_held_under(fsys: &dyn FileSystem, dir: &str) -> bool {
    let (id, d) = key(fsys, dir);
    let prefix = if d.ends_with('\\') {
        d
    } else {
        format!("{d}\\")
    };
    table()
        .iter()
        .any(|(i, p)| *i == id && p.starts_with(&prefix))
}

pub(crate) fn is_held(fsys: &dyn FileSystem, path: &str) -> bool {
    table().contains(&key(fsys, path))
}

pub(crate) fn same_path(fsys: &dyn FileSystem, a: &str, b: &str) -> bool {
    key(fsys, a) == key(fsys, b)
}
