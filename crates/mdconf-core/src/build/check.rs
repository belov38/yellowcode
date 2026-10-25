use super::classify::{EntryCategory, classify};
use super::modules::{ModuleCheck, Probe, check_module, probe_module};
use crate::codec::StageRunner;
use crate::objects::patch::{PatchError, check_object_entries};
use crate::{Configuration, EntrySource, Error, LoadOptions, Result};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

const FIRST_DIFFERENCES: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckCount {
    pub entries: usize,
    pub equal: usize,
    pub differ: usize,
    pub unchecked: BTreeMap<String, usize>,
    pub missing: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub by_category: BTreeMap<EntryCategory, CheckCount>,
    pub first_differences: Vec<String>,
    pub other: Vec<String>,
}

impl CheckReport {
    pub fn differ(&self) -> usize {
        self.by_category.values().map(|c| c.differ).sum()
    }

    pub fn entries(&self) -> usize {
        self.by_category.values().map(|c| c.entries).sum()
    }

    pub fn table(&self) -> String {
        let mut out = String::new();
        for (category, c) in &self.by_category {
            let _ = write!(
                out,
                "{:<17} entries {:>7}  equal {:>7}  differ {:>5}",
                category.name(),
                c.entries,
                c.equal,
                c.differ
            );
            if c.missing > 0 {
                let _ = write!(out, "  missing entries {}", c.missing);
            }
            if !c.unchecked.is_empty() {
                let reasons: Vec<String> = c
                    .unchecked
                    .iter()
                    .map(|(r, n)| format!("{r} {n}"))
                    .collect();
                let _ = write!(out, "  not checked: {}", reasons.join(", "));
            }
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "total entries {}, differ {}",
            self.entries(),
            self.differ()
        );
        out
    }

    fn note(&mut self, line: String) {
        if self.first_differences.len() < FIRST_DIFFERENCES {
            self.first_differences.push(line);
        }
    }
}

pub(super) fn patch_error(e: PatchError) -> Error {
    match e {
        PatchError::Unwritable { object, path, what } => Error::Build { object, path, what },
        PatchError::Entries(e) => e,
        other => Error::Format {
            entry: String::new(),
            what: format!("object check: {other:?}"),
        },
    }
}

fn indexed_ids(cfg: &Configuration) -> HashSet<brace::Guid> {
    let ix = &cfg.index;
    let mut ids: HashSet<brace::Guid> = ix.objs.keys().copied().collect();
    ids.extend(ix.subs.values().flatten().map(|(_, id)| *id));
    ids.extend(ix.recalcs.values().flatten().copied());
    ids.extend(ix.nested.iter().flat_map(|(a, b)| [*a, *b]));
    ids
}

fn unvisited_object(
    cfg: &Configuration,
    indexed: &HashSet<brace::Guid>,
    src: &dyn EntrySource,
    name: &str,
) -> Result<&'static str> {
    let Some(id) = brace::Guid::parse(name).filter(|g| indexed.contains(g)) else {
        return Ok("not_indexed");
    };
    if cfg
        .index
        .objs
        .get(&id)
        .is_some_and(|(_, spec)| spec.is_none())
    {
        return Ok("no_header_spec");
    }
    let stored = src
        .stored(name)?
        .ok_or_else(|| Error::MissingEntry(name.to_string()))?;
    Ok(match crate::entries::decode_stored(&stored)? {
        crate::EntryData::Brace(_) => "no_properties",
        _ => "not_brace",
    })
}

fn kept_binary(cfg: &Configuration, name: &str, inflated: &[u8]) -> bool {
    let Some(id) = name
        .split_once('.')
        .and_then(|(u, _)| brace::Guid::parse(u))
    else {
        return false;
    };
    let mut objects: Vec<&crate::MdObject> = cfg.objects.iter().collect();
    while let Some(o) = objects.pop() {
        if o.id == id {
            return o.parts.binary_modules.iter().any(|(_, b)| b == inflated);
        }
        objects.extend(&o.subsystems);
    }
    false
}

#[doc(hidden)]
pub fn check_with(src: &dyn EntrySource, runner: &dyn StageRunner) -> Result<CheckReport> {
    let cfg = crate::codec::load_with(src, &LoadOptions::default(), runner)?;
    check_model(&cfg, src)
}

pub(crate) fn check_model(cfg: &Configuration, src: &dyn EntrySource) -> Result<CheckReport> {
    let schema = mdschema::schema()?;
    let list = src.list()?;
    let names: Vec<String> = list.iter().map(|m| m.name.clone()).collect();
    let present: HashSet<&str> = names.iter().map(String::as_str).collect();
    let slots = crate::modules::entry_slots(schema, cfg);
    let slot_names: HashSet<&str> = slots
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| present.contains(n))
        .collect();
    let classified = classify(cfg, &names, &slot_names)?;
    let objects = check_object_entries(cfg, src).map_err(patch_error)?;
    let mut report = CheckReport::default();
    for (entry, outcome) in &objects.per_entry {
        let name = entry.to_string();
        if classified.get(&name).map(|c| c.0) != Some(EntryCategory::Object) {
            return Err(Error::Format {
                entry: name,
                what: "the object check visited an entry not classified Object".into(),
            });
        }
        let _ = outcome;
    }
    let indexed = indexed_ids(cfg);
    let module_of: BTreeMap<&str, Option<&crate::Module>> =
        slots.iter().map(|(n, m)| (n.as_str(), *m)).collect();
    for name in &names {
        let Some((category, reason)) = classified.get(name) else {
            continue;
        };
        let count = report.by_category.entry(*category).or_default();
        count.entries += 1;
        let unchecked = |count: &mut CheckCount, why: &str| {
            *count.unchecked.entry(why.to_string()).or_default() += 1;
        };
        match category {
            EntryCategory::Object => {
                let visited = brace::Guid::parse(name).and_then(|g| objects.per_entry.get(&g));
                match visited {
                    Some(None) => count.equal += 1,
                    Some(Some(what)) => {
                        count.differ += 1;
                        let line = format!("{name} Object: {what}");
                        report.note(line);
                    }
                    None => {
                        let why = unvisited_object(cfg, &indexed, src, name)?;
                        unchecked(count, why);
                    }
                }
            }
            EntryCategory::Module => match module_of.get(name.as_str()) {
                Some(Some(module)) => {
                    let stored = src
                        .stored(name)?
                        .ok_or_else(|| Error::MissingEntry(name.clone()))?;
                    match check_module(&stored, &module.source)? {
                        ModuleCheck::Equal => count.equal += 1,
                        ModuleCheck::Differ(what) => {
                            count.differ += 1;
                            report.note(format!("{name} Module: {what}"));
                        }
                        ModuleCheck::NoText => unchecked(count, "no_text"),
                    }
                }
                Some(None) => {
                    let stored = src
                        .stored(name)?
                        .ok_or_else(|| Error::MissingEntry(name.clone()))?;
                    match probe_module(&stored)? {
                        Probe::NoText(inflated) if kept_binary(cfg, name, &inflated) => {
                            unchecked(count, "binary_module")
                        }
                        Probe::NoText(_) => {
                            count.differ += 1;
                            report.note(format!("{name} Module: no_text_not_kept"));
                        }
                        Probe::HasText => {
                            count.differ += 1;
                            report.note(format!("{name} Module: model_has_no_module"));
                        }
                        Probe::Unreadable(what) => {
                            count.differ += 1;
                            report.note(format!("{name} Module: {what}"));
                        }
                    }
                }
                None => unchecked(count, "not_a_slot"),
            },
            EntryCategory::Other => {
                unchecked(count, "passthrough");
                let why = reason.clone().unwrap_or_default();
                report.other.push(format!("{name}: {why}"));
            }
            _ => unchecked(count, "passthrough"),
        }
    }
    for (name, module) in &slots {
        if module.is_some() && !present.contains(name.as_str()) {
            let count = report.by_category.entry(EntryCategory::Module).or_default();
            count.missing += 1;
            count.differ += 1;
            report.note(format!("{name} Module: missing_entry"));
        }
    }
    Ok(report)
}
