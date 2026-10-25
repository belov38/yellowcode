use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_ITEMS: usize = 10;
const MAX_FILE: u64 = 1 << 20;

#[derive(Default)]
pub struct Recent {
    pub items: Vec<RecentItem>,
}

#[derive(Clone, Debug)]
pub struct RecentItem {
    pub path: PathBuf,
    pub exists: bool,
    pub name: String,
}

#[derive(Serialize, Deserialize, Default)]
struct FileForm {
    #[serde(default)]
    items: Vec<SavedItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SavedItem {
    Legacy(String),
    Named { path: String, name: String },
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
    dunce::canonicalize(path).unwrap_or_else(|_| dunce::simplified(path).to_path_buf())
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
        for p in form.items {
            let (p, name) = match p {
                SavedItem::Legacy(p) => (p, String::new()),
                SavedItem::Named { path, name } => (path, name),
            };
            let path = dunce::simplified(Path::new(&p)).to_path_buf();
            let name = if name.is_empty() {
                display_name(&path)
            } else {
                name
            };
            if r.items.iter().all(|i| i.path != path) {
                r.items.push(RecentItem {
                    path,
                    exists: true,
                    name,
                });
            }
        }
        r.items.truncate(MAX_ITEMS);
        r.disambiguate_defaults();
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
                for number in 2..=MAX_ITEMS + 1 {
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
        let path = key(path);
        let name = self
            .items
            .iter()
            .find(|i| i.path == path)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| display_name(&path));
        self.items.retain(|i| i.path != path);
        self.items.insert(
            0,
            RecentItem {
                exists: path.exists(),
                path,
                name,
            },
        );
        self.items.truncate(MAX_ITEMS);
    }

    pub fn set(&mut self, old: Option<&Path>, path: &Path, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("source name must contain 1–256 bytes and no control characters".into());
        }
        if !path.is_absolute() || !path.exists() {
            return Err("choose an existing absolute source path".into());
        }
        if let Some(old) = old {
            self.remove(old);
        }
        self.touch(path);
        if let Some(item) = self.items.first_mut() {
            item.name = name.to_string();
        }
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) {
        self.items.retain(|i| i.path != path);
        let path = key(path);
        self.items.retain(|i| i.path != path);
    }

    pub fn exists_now(items: &[RecentItem]) -> Vec<RecentItem> {
        items
            .iter()
            .map(|i| RecentItem {
                exists: i.path.exists(),
                path: i.path.clone(),
                name: i.name.clone(),
            })
            .collect()
    }

    pub fn refresh(&mut self) {
        for i in &mut self.items {
            i.exists = i.path.exists();
        }
    }

    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let form = FileForm {
            items: self
                .items
                .iter()
                .filter_map(|i| {
                    i.path.to_str().map(|p| SavedItem::Named {
                        path: p.to_string(),
                        name: i.name.clone(),
                    })
                })
                .collect(),
        };
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&form)?)?;
        std::fs::rename(&tmp, file)
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
