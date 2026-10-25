use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
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

impl Part {
    pub const ALL: [Part; 12] = [
        Part::Objects,
        Part::Modules,
        Part::Forms,
        Part::Rights,
        Part::Predefined,
        Part::CommandInterface,
        Part::Configuration,
        Part::Help,
        Part::Pictures,
        Part::Misc,
        Part::Templates,
        Part::Flowcharts,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartStatus {
    Written,
    NotImplemented,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpOptions {
    pub parts: BTreeSet<Part>,
}

impl DumpOptions {
    pub fn all() -> DumpOptions {
        DumpOptions {
            parts: Part::ALL.into_iter().collect(),
        }
    }

    pub fn only(parts: &[Part]) -> DumpOptions {
        DumpOptions {
            parts: parts.iter().copied().collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpReport {
    pub files: BTreeMap<Part, usize>,
    pub misses: BTreeMap<String, u32>,
    pub not_written: Vec<Part>,
    pub deferred: Vec<String>,
}

#[derive(Debug)]
pub struct DumpSink {
    out: PathBuf,
    files: BTreeMap<Part, usize>,
    deferred: Vec<String>,
}

fn bad_rel(rel: &str, what: &str) -> Error {
    Error::Format {
        entry: rel.to_string(),
        what: what.to_string(),
    }
}

fn check_rel(rel: &str) -> Result<()> {
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

impl DumpSink {
    pub fn new(out: &Path) -> DumpSink {
        DumpSink {
            out: out.to_path_buf(),
            files: BTreeMap::new(),
            deferred: Vec::new(),
        }
    }

    pub fn put(&mut self, part: Part, rel: &str, data: &[u8]) -> Result<()> {
        check_rel(rel)?;
        let path = rel.split('/').fold(self.out.clone(), |p, c| p.join(c));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, data)?;
        *self.files.entry(part).or_insert(0) += 1;
        Ok(())
    }

    pub fn defer(&mut self, rel: &str) {
        self.deferred.push(rel.to_string());
    }

    pub fn files(&self) -> &BTreeMap<Part, usize> {
        &self.files
    }

    #[doc(hidden)]
    pub fn into_report(self, misses: BTreeMap<String, u32>, not_written: Vec<Part>) -> DumpReport {
        DumpReport {
            files: self.files,
            misses,
            not_written,
            deferred: self.deferred,
        }
    }
}
