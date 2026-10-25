use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub const BUILTIN_CSS: &str = include_str!("../data/skin-vars.css");
pub const MAX_PACKAGE: u64 = 32 * 1024 * 1024;
const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct Skin {
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

type Result<T> = std::result::Result<T, String>;

fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && name.split('/').all(|p| !matches!(p, "" | "." | ".."))
        && !name.chars().any(char::is_control)
}

fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(_, ext)| ext)
}

fn check_file(name: &str, size: u64) -> Result<()> {
    if !safe_path(name) {
        return Err(format!("unsafe skin path: {name}"));
    }
    if !matches!(extension(name), Some("json" | "png" | "svg")) {
        return Err(format!(
            "skin file {name}: only JSON, PNG and SVG are allowed"
        ));
    }
    if size > MAX_FILE {
        return Err(format!("skin file {name} exceeds 4 MiB"));
    }
    Ok(())
}

pub fn builtin_vars() -> &'static BTreeMap<String, String> {
    static VARS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    VARS.get_or_init(|| {
        BUILTIN_CSS
            .lines()
            .filter_map(|line| {
                let (key, value) = line.trim().strip_prefix("--cfg-")?.split_once(':')?;
                Some((
                    key.to_string(),
                    value.trim().trim_end_matches(';').to_string(),
                ))
            })
            .collect()
    })
}

fn matches(pattern: &str, value: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|r| r.is_match(value))
}

fn font(value: &str) -> bool {
    value.len() <= 256
        && matches(r#"^[A-Za-z0-9 '"_-]+(,\s*[A-Za-z0-9 '"_-]+)*$"#, value)
        && value.split(',').all(|part| {
            let part = part.trim();
            !part.is_empty()
                && part.chars().filter(|c| *c == '\'').count() % 2 == 0
                && part.chars().filter(|c| *c == '"').count() % 2 == 0
        })
}

fn validate_value(key: &str, value: &str, default: &str) -> Result<()> {
    let color = |s: &str| matches(r"^#([0-9A-Fa-f]{3}|[0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$", s);
    let valid = if key.starts_with("font-") && !key.ends_with("size") {
        font(value)
    } else if default.ends_with("px") {
        matches(r"^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?px$", value)
    } else if default.starts_with("linear-gradient(") {
        matches(
            r"^linear-gradient\((to (bottom|right),\s*)?#[0-9A-Fa-f]{6}( (0|([1-9][0-9]?|100)%))?(,\s*#[0-9A-Fa-f]{6}( (0|([1-9][0-9]?|100)%))?)+\)$",
            value,
        )
    } else {
        color(value)
    };
    if valid {
        Ok(())
    } else {
        Err(format!("invalid value for skin role {key}: {value}"))
    }
}

impl Skin {
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("skin source must not be a symlink".into());
        }
        if metadata.is_dir() {
            let mut files = BTreeMap::new();
            fn visit(
                root: &Path,
                dir: &Path,
                files: &mut BTreeMap<String, Vec<u8>>,
                total: &mut u64,
            ) -> Result<()> {
                for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                    let name = path
                        .strip_prefix(root)
                        .map_err(|e| e.to_string())?
                        .to_str()
                        .ok_or("non-UTF8 skin path")?
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    if meta.file_type().is_symlink() || !safe_path(&name) {
                        return Err(format!("unsafe skin path: {name}"));
                    }
                    if meta.is_dir() {
                        if name.split('/').count() > 16 {
                            return Err("skin folder nesting exceeds 16".into());
                        }
                        visit(root, &path, files, total)?;
                    } else {
                        if !meta.is_file() {
                            return Err(format!("not a regular skin file: {name}"));
                        }
                        check_file(&name, meta.len())?;
                        let mut bytes = Vec::new();
                        std::fs::File::open(&path)
                            .map_err(|e| e.to_string())?
                            .take(MAX_FILE + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                        add_file(files, total, name, bytes)?;
                    }
                }
                Ok(())
            }
            visit(path, path, &mut files, &mut 0)?;
            Self::from_files(files)
        } else {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(MAX_PACKAGE + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Self::from_zip(&bytes)
        }
    }

    pub fn from_zip(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_PACKAGE {
            return Err("skin zip exceeds 32 MiB".into());
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| format!("invalid skin zip: {e}"))?;
        if zip.len() > MAX_FILES {
            return Err("too many skin entries".into());
        }
        let mut files = BTreeMap::new();
        let mut total = 0;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().to_string();
            if !safe_path(name.trim_end_matches('/'))
                || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            {
                return Err(format!("unsafe skin zip path: {name}"));
            }
            if entry.is_dir() {
                continue;
            }
            check_file(&name, entry.size())?;
            let mut data = Vec::new();
            entry
                .by_ref()
                .take(MAX_FILE + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            add_file(&mut files, &mut total, name, data)?;
        }
        Self::from_files(files)
    }

    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let mut total = 0;
        if files.len() > MAX_FILES {
            return Err("too many skin files".into());
        }
        for (name, bytes) in &files {
            check_file(name, bytes.len() as u64)?;
            total += bytes.len() as u64;
            if total > MAX_PACKAGE {
                return Err("skin exceeds 32 MiB unpacked".into());
            }
            if extension(name) == Some("json") {
                serde_json::from_slice::<serde_json::Value>(bytes)
                    .map_err(|e| format!("{name}: {e}"))?;
            }
            if extension(name) == Some("png") && !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(format!("invalid PNG: {name}"));
            }
            if extension(name) == Some("svg") {
                let text =
                    std::str::from_utf8(bytes).map_err(|_| format!("invalid SVG UTF-8: {name}"))?;
                if !text.contains("<svg") {
                    return Err(format!("invalid SVG: {name}"));
                }
            }
        }
        let manifest: Manifest = serde_json::from_slice(
            files
                .get("skin.json")
                .ok_or("skin.json is required at package root")?,
        )
        .map_err(|e| format!("skin.json: {e}"))?;
        if manifest.format != 1 {
            return Err("unsupported skin format (expected 1)".into());
        }
        if manifest.id == "builtin" || !matches(r"^[a-z0-9][a-z0-9-]{0,63}$", &manifest.id) {
            return Err("skin id must be 1–64 lowercase ASCII letters, digits or hyphens; builtin is reserved".into());
        }
        if manifest.name.trim().is_empty()
            || manifest.name.len() > 256
            || manifest.version.trim().is_empty()
            || manifest.version.len() > 64
        {
            return Err("skin name and version are required (max 256/64 bytes)".into());
        }
        for (key, value) in &manifest.vars {
            let default = builtin_vars()
                .get(key)
                .ok_or_else(|| format!("unknown skin role: {key}"))?;
            validate_value(key, value, default)?;
        }
        for (key, value) in &manifest.fonts {
            if !matches!(key.as_str(), "ui" | "mono") || !font(value) {
                return Err(format!("invalid skin font: {key}"));
            }
        }
        for (key, file) in &manifest.icons {
            if !safe_path(key)
                || !key.split_once('/').is_some_and(|(p, name)| {
                    matches!(p, "tree" | "state" | "cmd" | "pic") && !name.is_empty()
                })
            {
                return Err(format!("invalid skin icon key: {key}"));
            }
            if !safe_path(file)
                || !matches!(extension(file), Some("png" | "svg"))
                || !files.contains_key(file)
            {
                return Err(format!("missing or invalid skin icon file: {file}"));
            }
        }
        Ok(Self { manifest, files })
    }

    pub fn css(&self) -> String {
        let mut vars = self.manifest.vars.clone();
        for (key, value) in &self.manifest.fonts {
            vars.insert(format!("font-{key}"), value.clone());
        }
        let mut css = BUILTIN_CSS.to_string();
        css.push_str(":root {\n");
        for (key, value) in vars {
            css.push_str(&format!("  --cfg-{key}: {value};\n"));
        }
        css.push_str("}\n");
        css
    }

    pub fn icon(&self, key: &str, double: bool) -> Option<(&str, &[u8])> {
        let file = self.manifest.icons.get(key)?;
        if double {
            let (stem, ext) = file.rsplit_once('.')?;
            let high = format!("{stem}@2x.{ext}");
            if let Some(data) = self.files.get(&high) {
                return Some((extension(file)?, data));
            }
        }
        Some((extension(file)?, self.files.get(file)?))
    }

    pub fn install(&self, root: &Path) -> Result<std::path::PathBuf> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let target = root.join(&self.manifest.id);
        if target.exists() {
            return Err(format!("skin {} is already installed", self.manifest.id));
        }
        let staging = root.join(format!(
            ".install-{}-{}",
            self.manifest.id,
            std::process::id()
        ));
        std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
        let result = (|| {
            for (name, bytes) in &self.files {
                let path = staging.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(path, bytes).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
            Ok(target)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(staging);
        }
        result
    }
}

fn add_file(
    files: &mut BTreeMap<String, Vec<u8>>,
    total: &mut u64,
    name: String,
    bytes: Vec<u8>,
) -> Result<()> {
    check_file(&name, bytes.len() as u64)?;
    *total += bytes.len() as u64;
    if *total > MAX_PACKAGE || files.len() >= MAX_FILES {
        return Err("skin package size/count limit exceeded".into());
    }
    if files.insert(name.clone(), bytes).is_some() {
        return Err(format!("duplicate skin file: {name}"));
    }
    Ok(())
}
