use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlFormat {
    V2_20,
    V2_21,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlPart {
    Objects,
    Modules,
    Forms,
    Rights,
    Predefined,
    CommandInterface,
    Configuration,
    Help,
    Pictures,
    Misc,
    Templates,
    Flowcharts,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlFile {
    part: XmlPart,
    bytes: Arc<[u8]>,
}
impl XmlFile {
    pub fn part(&self) -> XmlPart {
        self.part
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlOrigin {
    format: XmlFormat,
    files: BTreeMap<PathBuf, XmlFile>,
}
impl XmlOrigin {
    pub fn new(format: XmlFormat) -> Self {
        Self {
            format,
            files: BTreeMap::new(),
        }
    }
    pub fn format(&self) -> XmlFormat {
        self.format
    }
    pub fn files(&self) -> impl Iterator<Item = (&Path, &XmlFile)> {
        self.files.iter().map(|(p, f)| (p.as_path(), f))
    }
    pub fn get(&self, path: &Path) -> Option<&XmlFile> {
        self.files.get(path)
    }
    pub fn insert(&mut self, path: PathBuf, part: XmlPart, bytes: Vec<u8>) -> bool {
        if path.as_os_str().is_empty()
            || path.components().any(|c| match c {
                Component::Normal(n) => n.to_str().is_none_or(|s| s.contains(['\\', ':', '\0'])),
                _ => true,
            })
            || self.files.contains_key(&path)
        {
            return false;
        }
        self.files.insert(
            path,
            XmlFile {
                part,
                bytes: bytes.into(),
            },
        );
        true
    }
}
