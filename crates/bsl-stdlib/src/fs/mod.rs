mod error;
mod file;
mod globals;
mod lock;

pub(crate) use error::{
    copy_error, copy_error_text, delete_error, file_not_found, file_sharing, fs, fs_error,
    move_error, stream_not_found,
};
pub(crate) use lock::{FileLock, is_held, is_held_under, same_path};

use bsl_prim::{BslDate, Number};
use bsl_vm::{FileSystem, FsError, FsErrorKind, FsStat, Registry};
use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::{Mutex, MutexGuard};

const TEMP_DIR: &str = r"C:\Users\replica\AppData\Local\Temp\";

const NODE_OVERHEAD: u64 = 96;

const STAND_LIMIT: u64 = 256 << 20;

enum Kind {
    File(Vec<u8>),
    Dir,
}

struct Node {
    display: String,
    kind: Kind,
    modified: BslDate,
}

impl Node {
    fn cost(&self) -> u64 {
        let data = match &self.kind {
            Kind::File(d) => d.len() as u64,
            Kind::Dir => 0,
        };
        data.saturating_add(self.display.len() as u64 * 2)
            .saturating_add(NODE_OVERHEAD)
    }
}

struct State {
    nodes: BTreeMap<String, Node>,
    used: u64,
}

pub struct MemFs {
    limit: u64,
    st: Mutex<State>,
}

struct Parsed {
    key: String,
    display: String,
    parent: Option<String>,
    leaf: String,
    chain: Vec<(String, String)>,
}

fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

fn fold_chars(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn lock(st: &Mutex<State>) -> MutexGuard<'_, State> {
    st.lock().unwrap_or_else(|e| e.into_inner())
}

fn valid_char(c: char) -> bool {
    !matches!(c, '<' | '>' | '"' | '|' | '*' | '?') && c >= ' '
}

fn parse(path: &str) -> Option<Parsed> {
    if path.is_empty() || !path.chars().all(valid_char) {
        return None;
    }
    let p = path.replace('/', "\\");
    let b = p.as_bytes();
    let mut segs: Vec<&str> = Vec::new();
    let (drive, rest) = if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        (p[..1].to_ascii_uppercase(), &p[2..])
    } else if p.starts_with("\\\\") {
        return None;
    } else if p.starts_with('\\') {
        ("C".to_owned(), &p[..])
    } else {
        segs.extend(TEMP_DIR[3..].split('\\').filter(|s| !s.is_empty()));
        ("C".to_owned(), &p[..])
    };
    for s in rest.split('\\') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s if s.contains(':') => return None,
            s => segs.push(s),
        }
    }
    let root_display = format!("{drive}:\\");
    let mut chain = vec![(fold(&root_display), root_display)];
    for s in segs {
        let (pk, pd) = chain.last()?;
        let sep = if pk.ends_with('\\') { "" } else { "\\" };
        let next = (format!("{pk}{sep}{}", fold(s)), format!("{pd}{sep}{s}"));
        chain.push(next);
    }
    let (key, display) = chain.last()?.clone();
    let parent = chain
        .len()
        .checked_sub(2)
        .and_then(|i| chain.get(i))
        .map(|(k, _)| k.clone());
    let leaf = if parent.is_some() {
        leaf_of(&display).to_owned()
    } else {
        String::new()
    };
    Some(Parsed {
        key,
        display,
        parent,
        leaf,
        chain,
    })
}

fn glob(mask: &[char], name: &[char]) -> bool {
    let (mut m, mut n) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while n < name.len() {
        match mask.get(m) {
            Some('*') => {
                star = m;
                mark = n;
                m += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                m += 1;
                n += 1;
            }
            _ if star != usize::MAX => {
                m = star + 1;
                mark += 1;
                n = mark;
            }
            _ => return false,
        }
    }
    mask[m..].iter().all(|&c| c == '*')
}

fn mask_matches(mask: &str, name: &str) -> bool {
    let mask = if mask == "*.*" { "*" } else { mask };
    glob(&fold_chars(mask), &fold_chars(name))
}

fn now_utc() -> BslDate {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    BslDate::from_parts(1970, 1, 1, 0, 0, 0).map_or(BslDate::EMPTY, |e| {
        e.add_seconds(&Number::from_i64(i64::try_from(secs).unwrap_or(0)))
    })
}

fn err(kind: FsErrorKind, p: &Parsed) -> FsError {
    FsError {
        kind,
        path: p.display.clone(),
    }
}

fn err_raw(kind: FsErrorKind, path: &str) -> FsError {
    FsError {
        kind,
        path: path.to_owned(),
    }
}

impl MemFs {
    pub fn new(limit_bytes: u64) -> MemFs {
        let fs = MemFs {
            limit: limit_bytes,
            st: Mutex::new(State {
                nodes: BTreeMap::new(),
                used: 0,
            }),
        };
        {
            let mut st = lock(&fs.st);
            if let Some(p) = parse(TEMP_DIR) {
                let _ = make_dirs(&mut st, &p, u64::MAX, true);
            }
        }
        fs
    }

    pub fn stand() -> MemFs {
        MemFs::new(STAND_LIMIT)
    }

    fn parsed(&self, path: &str) -> Result<Parsed, FsError> {
        parse(path).ok_or_else(|| err_raw(FsErrorKind::AccessDenied, path))
    }
}

fn make_dirs(st: &mut State, p: &Parsed, limit: u64, create_root: bool) -> Result<(), FsError> {
    for (i, (k, d)) in p.chain.iter().enumerate() {
        match st.nodes.get(k) {
            Some(Node {
                kind: Kind::Dir, ..
            }) => continue,
            Some(_) => return Err(err(FsErrorKind::AccessDenied, p)),
            None if i == 0 && !create_root => return Err(err(FsErrorKind::AccessDenied, p)),
            None => {}
        }
        let node = Node {
            display: d.clone(),
            kind: Kind::Dir,
            modified: now_utc(),
        };
        let cost = node.cost();
        if st.used.saturating_add(cost) > limit {
            return Err(err(FsErrorKind::TooLarge, p));
        }
        st.used += cost;
        st.nodes.insert(k.clone(), node);
    }
    Ok(())
}

fn below<'a>(st: &'a State, dir_key: &str) -> impl Iterator<Item = (&'a String, &'a Node)> {
    let prefix = if dir_key.ends_with('\\') {
        dir_key.to_owned()
    } else {
        format!("{dir_key}\\")
    };
    st.nodes
        .range::<String, _>((Bound::Included(prefix.clone()), Bound::Unbounded))
        .take_while(move |(k, _)| k.starts_with(&prefix))
}

fn remove_node(st: &mut State, key: &str) {
    if let Some(n) = st.nodes.remove(key) {
        st.used = st.used.saturating_sub(n.cost());
    }
}

impl FileSystem for MemFs {
    fn temp_dir(&self) -> String {
        TEMP_DIR.to_owned()
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let p = self.parsed(path)?;
        let st = lock(&self.st);
        match st.nodes.get(&p.key) {
            Some(Node {
                kind: Kind::File(d),
                ..
            }) => Ok(d.clone()),
            Some(_) => Err(err(FsErrorKind::AccessDenied, &p)),
            None => match &p.parent {
                Some(par) if !st.nodes.contains_key(par) => Err(err(FsErrorKind::DirNotFound, &p)),
                _ => Err(err(FsErrorKind::NotFound, &p)),
            },
        }
    }

    fn write(&self, path: &str, data: &[u8], append: bool) -> Result<(), FsError> {
        let p = self.parsed(path)?;
        let mut st = lock(&self.st);
        let Some(par) = &p.parent else {
            return Err(err(FsErrorKind::AccessDenied, &p));
        };
        let par_display = match st.nodes.get(par) {
            Some(Node {
                kind: Kind::Dir,
                display,
                ..
            }) => display.clone(),
            _ => return Err(err(FsErrorKind::DirNotFound, &p)),
        };
        let old_len = match st.nodes.get(&p.key) {
            Some(Node {
                kind: Kind::File(d),
                ..
            }) => Some(d.len() as u64),
            Some(_) => return Err(err(FsErrorKind::AccessDenied, &p)),
            None => None,
        };
        let data_len = data.len() as u64;
        match old_len {
            Some(old_len) => {
                let new_len = if append {
                    old_len.saturating_add(data_len)
                } else {
                    data_len
                };
                let used = st.used.saturating_sub(old_len).saturating_add(new_len);
                if used > self.limit {
                    return Err(err(FsErrorKind::TooLarge, &p));
                }
                if let Some(Node {
                    kind: Kind::File(old),
                    modified,
                    ..
                }) = st.nodes.get_mut(&p.key)
                {
                    if append {
                        old.extend_from_slice(data);
                    } else {
                        *old = data.to_vec();
                    }
                    *modified = now_utc();
                }
                st.used = used;
            }
            None => {
                let node = Node {
                    display: format!("{}{}{}", par_display, sep_for(&par_display), p.leaf),
                    kind: Kind::File(data.to_vec()),
                    modified: now_utc(),
                };
                let cost = node.cost();
                if st.used.saturating_add(cost) > self.limit {
                    return Err(err(FsErrorKind::TooLarge, &p));
                }
                st.used += cost;
                st.nodes.insert(p.key.clone(), node);
            }
        }
        Ok(())
    }

    fn stat(&self, path: &str) -> Result<Option<FsStat>, FsError> {
        let Some(p) = parse(path) else {
            return Ok(None);
        };
        let st = lock(&self.st);
        Ok(st.nodes.get(&p.key).map(|n| FsStat {
            is_dir: matches!(n.kind, Kind::Dir),
            size: match &n.kind {
                Kind::File(d) => d.len() as u64,
                Kind::Dir => 0,
            },
            modified_utc: n.modified,
        }))
    }

    fn path_key(&self, path: &str) -> Option<String> {
        parse(path).map(|p| p.key)
    }

    fn remove(&self, path: &str, mask: Option<&str>) -> Result<(), FsError> {
        let p = self.parsed(path)?;
        let mut st = lock(&self.st);
        match mask {
            None => {
                if p.parent.is_none() {
                    return Err(err(FsErrorKind::AccessDenied, &p));
                }
                let keys: Vec<String> = below(&st, &p.key).map(|(k, _)| k.clone()).collect();
                for k in keys {
                    remove_node(&mut st, &k);
                }
                remove_node(&mut st, &p.key);
            }
            Some(mask) => {
                let depth = p.key.matches('\\').count() + usize::from(!p.key.ends_with('\\'));
                let keys: Vec<String> = below(&st, &p.key)
                    .filter(|(k, n)| {
                        matches!(n.kind, Kind::File(_))
                            && k.matches('\\').count() == depth
                            && mask_matches(mask, leaf_of(&n.display))
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in keys {
                    remove_node(&mut st, &k);
                }
            }
        }
        Ok(())
    }

    fn create_dir(&self, path: &str) -> Result<(), FsError> {
        let p = self.parsed(path)?;
        let mut st = lock(&self.st);
        make_dirs(&mut st, &p, self.limit, false)
    }

    fn list(&self, dir: &str, mask: &str, recursive: bool) -> Result<Vec<String>, FsError> {
        let p = self.parsed(dir)?;
        let st = lock(&self.st);
        if !matches!(
            st.nodes.get(&p.key),
            Some(Node {
                kind: Kind::Dir,
                ..
            })
        ) {
            return Ok(Vec::new());
        }
        let depth = p.key.matches('\\').count() + usize::from(!p.key.ends_with('\\'));
        let mut found: Vec<&Node> = below(&st, &p.key)
            .filter(|(k, n)| {
                (recursive || k.matches('\\').count() == depth)
                    && mask_matches(mask, leaf_of(&n.display))
            })
            .map(|(_, n)| n)
            .collect();
        found.sort_by_cached_key(|n| ntfs_order(&n.display));
        Ok(found.into_iter().map(|n| n.display.clone()).collect())
    }
}

fn ntfs_order(display: &str) -> Vec<Vec<char>> {
    display
        .split('\\')
        .map(|part| part.chars().flat_map(char::to_uppercase).collect())
        .collect()
}

fn sep_for(parent_display: &str) -> &'static str {
    if parent_display.ends_with('\\') {
        ""
    } else {
        "\\"
    }
}

fn leaf_of(display: &str) -> &str {
    display.rsplit('\\').next().unwrap_or(display)
}

pub(crate) fn register(reg: &mut Registry) {
    globals::register(reg);
    file::register(reg);
}
