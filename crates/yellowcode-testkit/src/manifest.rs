use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

pub use toml;

#[derive(Debug, Clone, PartialEq)]
pub struct Dep {
    pub package: String,
    pub path: Option<PathBuf>,
    pub optional: bool,
}

pub fn workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn crate_dir(name: &str) -> PathBuf {
    workspace_dir().join("crates").join(name)
}

pub fn read_table(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn root() -> toml::Table {
    read_table(&workspace_dir().join("Cargo.toml"))
}

fn dep(key: &str, spec: &toml::Value, dir: &Path, root: &toml::Table, root_dir: &Path) -> Dep {
    let Some(t) = spec.as_table() else {
        return Dep {
            package: key.to_string(),
            path: None,
            optional: false,
        };
    };
    let optional = t.get("optional").and_then(|v| v.as_bool()) == Some(true);
    if t.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
        assert!(
            t.get("package").is_none(),
            "{key}: `package` next to `workspace = true`"
        );
        let ws = root
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .and_then(|d| d.get(key))
            .unwrap_or_else(|| {
                panic!("{key}: workspace = true, but no [workspace.dependencies] entry")
            });
        let mut d = dep(key, ws, root_dir, root, root_dir);
        d.optional = optional;
        return d;
    }
    Dep {
        package: t
            .get("package")
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string(),
        path: t.get("path").and_then(|v| v.as_str()).map(|p| dir.join(p)),
        optional,
    }
}

pub fn deps_of(
    manifest: &toml::Table,
    section: &str,
    dir: &Path,
    root: &toml::Table,
    root_dir: &Path,
) -> Vec<Dep> {
    let mut tables: Vec<&toml::Table> = Vec::new();
    if let Some(t) = manifest.get(section).and_then(|v| v.as_table()) {
        tables.push(t);
    }
    if let Some(targets) = manifest.get("target").and_then(|v| v.as_table()) {
        for target in targets.values() {
            if let Some(t) = target.get(section).and_then(|v| v.as_table()) {
                tables.push(t);
            }
        }
    }
    tables
        .into_iter()
        .flat_map(|t| t.iter())
        .map(|(k, v)| dep(k, v, dir, root, root_dir))
        .collect()
}

pub fn own(name: &str, section: &str) -> Vec<Dep> {
    let dir = crate_dir(name);
    deps_of(
        &read_table(&dir.join("Cargo.toml")),
        section,
        &dir,
        &root(),
        &workspace_dir(),
    )
}

pub fn closure(dir: &Path, features_off: bool) -> BTreeSet<String> {
    let root = root();
    let root_dir = workspace_dir();
    let top = dir
        .canonicalize()
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let mut names = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([top.clone()]);
    while let Some(dir) = queue.pop_front() {
        let dir = dir
            .canonicalize()
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        if !seen.insert(dir.clone()) {
            continue;
        }
        let manifest = read_table(&dir.join("Cargo.toml"));
        for d in deps_of(&manifest, "dependencies", &dir, &root, &root_dir) {
            if features_off && d.optional && dir == top {
                continue;
            }
            names.insert(d.package);
            queue.extend(d.path);
        }
    }
    names
}
