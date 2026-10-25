use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ui_style::skin::Skin;

include!("skin_icons/list.rs");

#[derive(Default)]
pub struct Skins {
    packages: BTreeMap<String, Skin>,
    active: String,
    preference: Option<PathBuf>,
}

pub type Store = Arc<Mutex<Skins>>;

impl Skins {
    pub fn store() -> Store {
        Arc::new(Mutex::new(Self::default()))
    }

    pub fn active(&self) -> &str {
        if self.active.is_empty() {
            "builtin"
        } else {
            &self.active
        }
    }

    pub fn add(&mut self, skin: Skin) {
        self.packages.insert(skin.manifest.id.clone(), skin);
    }

    pub fn select(&mut self, id: &str) -> Result<(), String> {
        if id != "builtin" && !self.packages.contains_key(id) {
            return Err(format!("unknown skin: {id}"));
        }
        if let Some(file) = &self.preference {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let pending = file.with_extension("pending");
            std::fs::write(&pending, id).map_err(|e| e.to_string())?;
            std::fs::rename(pending, file).map_err(|e| e.to_string())?;
        }
        self.active = id.to_string();
        Ok(())
    }

    pub fn installed(root: &Path) -> Self {
        let preference = root.join("active");
        let mut skins = Self::default();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                if entry.path().is_dir() {
                    match Skin::load(&entry.path()) {
                        Ok(skin) => skins.add(skin),
                        Err(e) => eprintln!("skin {}: {e}", entry.path().display()),
                    }
                }
            }
        }
        if let Ok(id) = std::fs::read_to_string(&preference) {
            let _ = skins.select(id.trim());
        }
        skins.preference = Some(preference);
        skins
    }

    pub fn list(&self) -> serde_json::Value {
        let mut list = vec![
            serde_json::from_str(include_str!("skin_icons/skin.json"))
                .unwrap_or_else(|_| serde_json::json!({"id":"builtin", "name":"1С классический", "version":"1", "format":1})),
        ];
        for skin in self.packages.values() {
            let m = &skin.manifest;
            list.push(
                serde_json::json!({"id":m.id,"name":m.name,"version":m.version,"format":m.format}),
            );
        }
        serde_json::json!({"active": self.active(), "skins": list})
    }

    pub fn css(&self) -> String {
        self.packages
            .get(self.active())
            .map_or_else(|| ui_style::skin::BUILTIN_CSS.to_string(), Skin::css)
    }

    pub fn icon(&self, key: &str, double: bool) -> Option<(&str, &[u8])> {
        self.packages
            .get(self.active())
            .and_then(|s| s.icon(key, double))
            .or_else(|| builtin_icon(key, double))
    }
}

fn builtin_icon(key: &str, double: bool) -> Option<(&'static str, &'static [u8])> {
    if let Some(kind) = key.strip_prefix("tree/") {
        let (_, png, high) = super::icon_table::ICONS
            .iter()
            .find(|(k, _, _)| *k == kind)?;
        return Some(("png", if double { high.unwrap_or(png) } else { png }));
    }
    if let Some(id) = key.strip_prefix("cmd/") {
        let (_, png, high) = COMMAND_ICONS.iter().find(|(k, _, _)| *k == id)?;
        return Some(("png", if double { high.unwrap_or(png) } else { png }));
    }
    if let Some(name) = key
        .strip_prefix("pic/")
        .or_else(|| key.strip_prefix("state/"))
    {
        let pic = ui_style::std_picture(name)?;
        return Some((
            if pic.format == ui_style::PicFormat::Svg {
                "svg"
            } else {
                "png"
            },
            pic.bytes,
        ));
    }
    None
}
