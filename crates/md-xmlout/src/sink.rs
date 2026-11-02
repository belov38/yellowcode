use crate::rel_path::{PathSet, bad_rel, check_rel};
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

pub const DEFAULT_MEMORY_DUMP_BYTES: u64 = 512 << 20;

#[derive(Debug)]
pub struct DumpSink {
    target: Target,
    paths: PathSet,
    parts: BTreeMap<String, Part>,
    files: BTreeMap<Part, usize>,
    deferred: Vec<String>,
}

#[derive(Debug)]
enum Target {
    Directory(PathBuf),
    Memory {
        files: BTreeMap<String, Vec<u8>>,
        limit: u64,
        used: u64,
    },
}

fn charge(rel: &str, data: &[u8]) -> u64 {
    let len = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    len(rel.len()).saturating_add(len(data.len()))
}

fn no_memory(rel: &str, bytes: usize) -> Error {
    Error::Format {
        entry: rel.to_string(),
        what: format!("cannot allocate {bytes} bytes for the memory dump"),
    }
}

impl DumpSink {
    pub fn new(out: &Path) -> DumpSink {
        DumpSink::with_target(Target::Directory(out.to_path_buf()))
    }

    pub fn memory() -> DumpSink {
        DumpSink::memory_with_limit(DEFAULT_MEMORY_DUMP_BYTES)
    }

    pub fn memory_with_limit(limit: u64) -> DumpSink {
        DumpSink::with_target(Target::Memory {
            files: BTreeMap::new(),
            limit,
            used: 0,
        })
    }

    fn with_target(target: Target) -> DumpSink {
        DumpSink {
            target,
            paths: PathSet::default(),
            parts: BTreeMap::new(),
            files: BTreeMap::new(),
            deferred: Vec::new(),
        }
    }

    pub fn put(&mut self, part: Part, rel: &str, data: &[u8]) -> Result<()> {
        check_rel(rel)?;
        let checked = self.paths.check(rel)?;
        let previous = self.parts.get(rel).copied();
        match &mut self.target {
            Target::Directory(out) => {
                let path = rel.split('/').fold(out.clone(), |p, c| p.join(c));
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, data)?;
            }
            Target::Memory { files, limit, used } => {
                let old = files.get(rel).map_or(0, |d| charge(rel, d));
                let total = used.saturating_sub(old).saturating_add(charge(rel, data));
                if total > *limit {
                    return Err(bad_rel(
                        rel,
                        &format!("memory dump limit of {limit} bytes exceeded"),
                    ));
                }
                let mut copy = Vec::new();
                copy.try_reserve_exact(data.len())
                    .map_err(|_| no_memory(rel, data.len()))?;
                copy.extend_from_slice(data);
                if let Some(file) = files.get_mut(rel) {
                    *file = copy;
                } else {
                    let mut name = String::new();
                    name.try_reserve_exact(rel.len())
                        .map_err(|_| no_memory(rel, rel.len()))?;
                    name.push_str(rel);
                    files.insert(name, copy);
                }
                *used = total;
            }
        }
        self.paths.insert(rel, checked);
        match previous {
            Some(seen_part) if seen_part == part => {}
            Some(seen_part) => {
                if let Some(n) = self.files.get_mut(&seen_part) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.files.remove(&seen_part);
                    }
                }
                *self.files.entry(part).or_insert(0) += 1;
                self.parts.insert(rel.to_string(), part);
            }
            None => {
                *self.files.entry(part).or_insert(0) += 1;
                self.parts.insert(rel.to_string(), part);
            }
        }
        Ok(())
    }

    pub fn defer(&mut self, rel: &str) {
        self.deferred.push(rel.to_string());
    }

    pub fn files(&self) -> &BTreeMap<Part, usize> {
        &self.files
    }

    pub fn into_files(self) -> BTreeMap<String, Vec<u8>> {
        self.finish(BTreeMap::new(), Vec::new()).1
    }

    #[doc(hidden)]
    pub fn into_report(self, misses: BTreeMap<String, u32>, not_written: Vec<Part>) -> DumpReport {
        self.finish(misses, not_written).0
    }

    #[doc(hidden)]
    pub fn finish(
        self,
        misses: BTreeMap<String, u32>,
        not_written: Vec<Part>,
    ) -> (DumpReport, BTreeMap<String, Vec<u8>>) {
        let files = match self.target {
            Target::Directory(_) => BTreeMap::new(),
            Target::Memory { files, .. } => files,
        };
        let report = DumpReport {
            files: self.files,
            misses,
            not_written,
            deferred: self.deferred,
        };
        (report, files)
    }
}
