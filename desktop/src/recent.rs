use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod store_lock;

const MAX_ITEMS: usize = 10;
const MAX_FILE: u64 = 1 << 20;

#[derive(Clone, Default)]
pub struct Recent {
    pub items: Vec<RecentItem>,
    pub sources: Vec<RecentItem>,
    pending: Vec<Mutation>,
}

#[derive(Clone)]
enum Mutation {
    Touch(PathBuf),
    Set {
        old: Option<PathBuf>,
        path: PathBuf,
        name: String,
    },
    Origin(PathBuf, Origin),
    Remove(PathBuf),
}

impl Mutation {
    fn apply(&self, recent: &mut Recent) -> Result<(), String> {
        match self {
            Self::Touch(path) => recent.touch_inner(path),
            Self::Set { old, path, name } => {
                recent.set_inner(old.as_deref(), path, name)?;
            }
            Self::Origin(path, origin) => recent.set_origin_inner(path, *origin)?,
            Self::Remove(path) => recent.remove_inner(path),
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct RecentItem {
    pub path: PathBuf,
    pub exists: bool,
    pub name: String,
    pub kind: Kind,
    pub origin: Origin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Unknown,
    Cf,
    Dt,
    Infobase,
    Xml,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    OneC,
    Yellowcode,
}

fn kind(path: &Path) -> Option<Kind> {
    use yellowcode_core::viewer::source::{SourceKind, detect};
    match detect(path) {
        Ok(SourceKind::Cf) => Some(Kind::Cf),
        Ok(SourceKind::Dt) => Some(Kind::Dt),
        Ok(SourceKind::Infobase) => Some(Kind::Infobase),
        Ok(SourceKind::XmlDir) => Some(Kind::Xml),
        Err(_) => None,
    }
}

#[derive(Serialize, Deserialize, Default)]
struct FileForm {
    #[serde(default)]
    items: Vec<SavedItem>,
    sources: Option<Vec<SavedItem>>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SavedItem {
    Legacy(String),
    Named {
        path: String,
        name: String,
        #[serde(default)]
        kind: Kind,
        #[serde(default)]
        origin: Origin,
    },
}

impl SavedItem {
    fn into_item(self) -> RecentItem {
        let (path, name, kind, origin) = match self {
            Self::Legacy(path) => (path, String::new(), Kind::Unknown, Origin::OneC),
            Self::Named {
                path,
                name,
                kind,
                origin,
            } => (path, name, kind, origin),
        };
        let path = dunce::simplified(Path::new(&path)).to_path_buf();
        let name = if name.is_empty() {
            display_name(&path)
        } else {
            name
        };
        RecentItem {
            path,
            exists: true,
            name,
            kind,
            origin,
        }
    }

    fn from_item(item: &RecentItem) -> Option<Self> {
        Some(Self::Named {
            path: item.path.to_str()?.to_string(),
            name: item.name.clone(),
            kind: item.kind,
            origin: item.origin,
        })
    }
}

fn read_capped(file: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_FILE + 1)
        .read_to_end(&mut buf)
        .ok()?;
    (buf.len() as u64 <= MAX_FILE).then_some(buf)
}

fn key(path: &Path) -> PathBuf {
    let mut ancestor = path;
    let mut tail = Vec::new();
    loop {
        if let Ok(mut canonical) = dunce::canonicalize(ancestor) {
            for part in tail.iter().rev() {
                canonical.push(part);
            }
            return canonical;
        }
        let (Some(parent), Some(name)) = (ancestor.parent(), ancestor.file_name()) else {
            return dunce::simplified(path).to_path_buf();
        };
        tail.push(name.to_os_string());
        ancestor = parent;
    }
}

impl Recent {
    pub fn load(file: &Path) -> Recent {
        let mut r = Recent::load_unchecked(file);
        r.refresh();
        r
    }

    pub fn load_unchecked(file: &Path) -> Recent {
        let form: FileForm = read_capped(file)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut r = Recent::default();
        for saved in form.items {
            let item = saved.into_item();
            if r.items.iter().all(|i| i.path != item.path) {
                r.items.push(item);
            }
        }
        r.disambiguate_defaults();
        r.sources = match form.sources {
            Some(saved) => {
                let mut sources = Vec::<RecentItem>::new();
                for item in saved.into_iter().map(SavedItem::into_item) {
                    if sources.iter().all(|s| s.path != item.path) {
                        sources.push(item);
                    }
                }
                sources
            }
            None => r.items.clone(),
        };
        r.items.truncate(MAX_ITEMS);
        for item in &mut r.items {
            if let Some(source) = r.sources.iter().find(|s| s.path == item.path) {
                *item = source.clone();
            } else {
                r.sources.push(item.clone());
            }
        }
        r
    }

    fn disambiguate_defaults(&mut self) {
        let duplicates: Vec<String> = self
            .items
            .iter()
            .filter(|item| {
                self.items
                    .iter()
                    .filter(|other| other.name.to_lowercase() == item.name.to_lowercase())
                    .count()
                    > 1
            })
            .map(|item| item.name.to_lowercase())
            .collect();
        for index in 0..self.items.len() {
            let Some(item) = self.items.get(index) else {
                continue;
            };
            let base = display_name(&item.path);
            if item.name != base || !duplicates.contains(&base.to_lowercase()) {
                continue;
            }
            let mut suffix = PathBuf::new();
            let mut name = base.clone();
            if let Some(parent) = item.path.parent() {
                for part in parent.components().rev() {
                    suffix = if suffix.as_os_str().is_empty() {
                        PathBuf::from(part.as_os_str())
                    } else {
                        PathBuf::from(part.as_os_str()).join(suffix)
                    };
                    name = format!("{base} ({})", suffix.display());
                    if self
                        .items
                        .iter()
                        .all(|other| other.name.to_lowercase() != name.to_lowercase())
                    {
                        break;
                    }
                }
            }
            if self
                .items
                .iter()
                .any(|other| other.name.to_lowercase() == name.to_lowercase())
            {
                name = format!("{base} ({})", item.path.display());
                for number in 2..=self.items.len() + 1 {
                    if self
                        .items
                        .iter()
                        .all(|other| other.name.to_lowercase() != name.to_lowercase())
                    {
                        break;
                    }
                    name = format!("{base} ({}) {number}", item.path.display());
                }
            }
            if let Some(item) = self.items.get_mut(index) {
                item.name = name;
            }
        }
    }

    pub fn touch(&mut self, path: &Path) {
        self.touch_inner(path);
        self.pending.push(Mutation::Touch(path.to_path_buf()));
    }

    fn touch_inner(&mut self, path: &Path) {
        let listed = path;
        let path = key(path);
        let matches = |i: &RecentItem| i.path == path || i.path == listed || key(&i.path) == path;
        let index = self.sources.iter().position(matches);
        let mut item = self
            .sources
            .iter()
            .find(|i| matches(i))
            .cloned()
            .unwrap_or_else(|| RecentItem {
                name: display_name(&path),
                exists: path.exists(),
                kind: Kind::Unknown,
                origin: Origin::OneC,
                path: path.clone(),
            });
        item.exists = path.exists();
        if item.exists {
            item.kind = kind(&path).unwrap_or(item.kind);
        }
        item.path = path.clone();
        self.sources.retain(|i| !matches(i));
        self.sources.insert(
            index.unwrap_or(self.sources.len()).min(self.sources.len()),
            item.clone(),
        );
        self.items.retain(|i| !matches(i));
        self.items.insert(0, item);
        self.items.truncate(MAX_ITEMS);
    }

    pub fn set(&mut self, old: Option<&Path>, path: &Path, name: &str) -> Result<PathBuf, String> {
        let saved = self.set_inner(old, path, name)?;
        self.pending.push(Mutation::Set {
            old: old.map(Path::to_path_buf),
            path: path.to_path_buf(),
            name: name.to_string(),
        });
        Ok(saved)
    }

    fn set_inner(
        &mut self,
        old: Option<&Path>,
        path: &Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("source name must contain 1–256 bytes and no control characters".into());
        }
        let listed = old.unwrap_or(path);
        let unchanged = old == Some(path)
            && self
                .sources
                .iter()
                .any(|i| i.path == path || i.path == key(path));
        if !path.is_absolute() || (!unchanged && !path.exists()) {
            return Err("choose an existing absolute source path".into());
        }
        let path = key(path);
        let old = old.map(key).unwrap_or_else(|| path.clone());
        let previous = self
            .sources
            .iter()
            .find(|i| i.path == old || i.path == listed || i.path == path);
        let origin = previous.map_or(Origin::OneC, |i| i.origin);
        let previous_kind = previous.map_or(Kind::Unknown, |i| i.kind);
        let item = RecentItem {
            name: name.to_string(),
            exists: path.exists(),
            kind: kind(&path).unwrap_or(previous_kind),
            origin,
            path: path.clone(),
        };
        let history = self
            .items
            .iter()
            .position(|i| i.path == old || i.path == listed || i.path == path);
        self.remove_inner(listed);
        self.remove_inner(&old);
        self.remove_inner(&path);
        self.sources.push(item.clone());
        if let Some(index) = history {
            self.items.insert(index.min(self.items.len()), item);
        }
        Ok(path)
    }

    pub fn set_origin(&mut self, path: &Path, origin: Origin) -> Result<(), String> {
        self.set_origin_inner(path, origin)?;
        self.pending
            .push(Mutation::Origin(path.to_path_buf(), origin));
        Ok(())
    }

    fn set_origin_inner(&mut self, path: &Path, origin: Origin) -> Result<(), String> {
        let path = key(path);
        if self
            .sources
            .iter()
            .any(|i| i.path == path && i.origin == origin)
        {
            return Ok(());
        }
        if origin == Origin::Yellowcode && kind(&path) != Some(Kind::Infobase) {
            return Err("Yellowcode base origin requires an existing 1CD infobase".into());
        }
        for item in self
            .sources
            .iter_mut()
            .chain(self.items.iter_mut())
            .filter(|i| i.path == path)
        {
            item.origin = origin;
        }
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) {
        self.remove_inner(path);
        self.pending.push(Mutation::Remove(path.to_path_buf()));
    }

    fn remove_inner(&mut self, path: &Path) {
        let canonical = key(path);
        self.items.retain(|i| i.path != path && i.path != canonical);
        self.sources
            .retain(|i| i.path != path && i.path != canonical);
    }

    pub fn exists_now(items: &[RecentItem]) -> Vec<RecentItem> {
        items
            .iter()
            .map(|i| {
                let detected = kind(&i.path);
                RecentItem {
                    exists: detected.is_some(),
                    path: i.path.clone(),
                    name: i.name.clone(),
                    kind: detected.unwrap_or(i.kind),
                    origin: i.origin,
                }
            })
            .collect()
    }

    pub fn refresh(&mut self) {
        for i in &mut self.items {
            i.exists = i.path.exists();
        }
    }

    fn encoded(&self) -> std::io::Result<Vec<u8>> {
        let form = FileForm {
            items: self.items.iter().filter_map(SavedItem::from_item).collect(),
            sources: Some(
                self.sources
                    .iter()
                    .filter_map(SavedItem::from_item)
                    .collect(),
            ),
        };
        let bytes = serde_json::to_vec_pretty(&form)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(std::io::Error::other("source registry exceeds 1 MiB"));
        }
        Ok(bytes)
    }

    pub fn save(&mut self, file: &Path) -> std::io::Result<()> {
        use std::io::Write;
        self.encoded()?;
        let dir = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let _lock = store_lock::acquire(file)?;
        let mut updated = Self::load_unchecked(file);
        for mutation in &self.pending {
            mutation
                .apply(&mut updated)
                .map_err(std::io::Error::other)?;
        }
        let bytes = updated.encoded()?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".recent-")
            .tempfile_in(dir)?;
        tmp.write_all(&bytes)?;
        tmp.persist(file).map_err(|error| error.error)?;
        *self = updated;
        Ok(())
    }
}

fn display_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

const LABEL_WIDTH: usize = 50;

pub fn menu_label(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        return short_label(&text, None, '\\');
    }
    let mut text: &str = &text;
    if cfg!(target_os = "macos") {
        for dir in ["/private/tmp", "/private/var"] {
            if text == dir || text.starts_with(&format!("{dir}/")) {
                text = &text["/private".len()..];
            }
        }
    }
    short_label(text, std::env::var("HOME").ok().as_deref(), '/')
}

fn short_label(text: &str, home: Option<&str>, sep: char) -> String {
    let home = home
        .map(|h| h.trim_end_matches(sep))
        .filter(|h| !h.is_empty());
    let text = match home.and_then(|h| text.strip_prefix(h)) {
        Some(rest) if rest.is_empty() || rest.starts_with(sep) => format!("~{rest}"),
        _ => text.to_string(),
    };
    let width = |s: &str| s.chars().count();
    if width(&text) <= LABEL_WIDTH {
        return text;
    }
    let parts: Vec<&str> = text.split(sep).collect();
    let head_len = parts.iter().take_while(|p| p.is_empty()).count() + 1;
    if parts.len() < head_len + 3 {
        return text;
    }
    let sep_text = sep.to_string();
    let (mut head, mut tail) = (head_len, parts.len() - 2);
    let label = |head: usize, tail: usize| {
        if head == tail {
            parts.join(&sep_text)
        } else {
            [&parts[..head], &["…"], &parts[tail..]]
                .concat()
                .join(&sep_text)
        }
    };
    loop {
        if head < tail && width(&label(head, tail - 1)) <= LABEL_WIDTH {
            tail -= 1;
        } else if head < tail && width(&label(head + 1, tail)) <= LABEL_WIDTH {
            head += 1;
        } else {
            return label(head, tail);
        }
    }
}
