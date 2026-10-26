use super::check::patch_error;
use super::classify::{EntryCategory, classify};
use super::modules::module_entry;
use super::structure;
use crate::codec::ConfigurationCore;
use crate::entries::Entries;
use crate::model::{MdObject, Module};
use crate::objects::patch::{Mode, PatchError, patch_entries};
use crate::parts::configdump::Names;
use crate::{Configuration, EntrySink, EntrySource, Error, LoadOptions, ModuleKind, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use v8cont::V8Time;

const FIRST_DIFFERS: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub reencode: BTreeSet<EntryCategory>,
    pub now: Option<v8cont::V8Time>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub entries: usize,
    pub copied: usize,
    pub reencoded: BTreeMap<EntryCategory, usize>,
    pub passthrough: BTreeMap<EntryCategory, usize>,
    pub changed: Vec<String>,
    pub reencode_differs: Vec<String>,
    pub reencode_differ_count: usize,
    pub kept: BTreeMap<String, u32>,
}

pub(crate) fn trace_rss(stage: &str) {
    if std::env::var_os("MDCONF_BUILD_RSS").is_none() {
        return;
    }
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map_or_else(String::new, |v| v.trim().to_string())
    };
    eprintln!(
        "BuildRss {stage}: VmRSS {} VmHWM {}",
        field("VmRSS:"),
        field("VmHWM:")
    );
}

fn at_entry(e: Error, name: &str) -> Error {
    match e {
        Error::Format { entry, what } if entry.is_empty() => Error::Format {
            entry: name.to_string(),
            what,
        },
        other => other,
    }
}

enum Put {
    Changed(String, Option<Vec<u8>>),
    Reencoded(Option<Vec<u8>>),
}

fn foreign(model: brace::Guid, base: brace::Guid) -> Error {
    Error::Build {
        object: "Configuration".into(),
        path: "root".into(),
        what: format!("configuration uuid {model} differs from the base {base}"),
    }
}

fn module_mut(
    model: &mut Configuration,
    owner: brace::Guid,
    kind: ModuleKind,
) -> Option<&mut Module> {
    if owner == model.header_id {
        return model.modules.iter_mut().find(|m| m.kind == kind);
    }
    let mut stack: Vec<&mut MdObject> = model.objects.iter_mut().collect();
    while let Some(o) = stack.pop() {
        if o.id == owner {
            return o.modules.iter_mut().find(|m| m.kind == kind);
        }
        if let Some(c) = o.children.iter_mut().find(|c| c.id == owner) {
            return c.module.as_mut().filter(|m| m.kind == kind);
        }
        if let Some(r) = o.recalculations.iter_mut().find(|r| r.id == owner) {
            return r.module.as_mut().filter(|m| m.kind == kind);
        }
        stack.extend(o.subsystems.iter_mut());
    }
    None
}

#[doc(hidden)]
pub fn build_edit_with(
    model: &Configuration,
    base: &dyn EntrySource,
    sink: &mut dyn EntrySink,
    opts: &BuildOptions,
    runner: &dyn crate::codec::StageRunner,
) -> Result<BuildReport> {
    model.require_full()?;
    trace_rss("start");
    let list = base.list()?;
    let mut publication = HashSet::new();
    for meta in &list {
        if !publication.insert(meta.name.as_str()) {
            return Err(Error::Build {
                object: meta.name.clone(),
                path: "publication list".into(),
                what: "duplicate captured publication entry".into(),
            });
        }
    }
    let root = crate::entries::root_uuid(&Entries::new(base)?)?;
    if root != model.id {
        return Err(foreign(model.id, root));
    }
    let (edited_props, base_versions, form_changes) = {
        let base_model = crate::codec::load_with(base, &LoadOptions::default(), runner)?;
        trace_rss("base loaded");
        if base_model.header_id != model.header_id {
            return Err(foreign(model.header_id, base_model.header_id));
        }
        structure::load_options(model, &base_model)?;
        let prepared = super::form_modules::Prepared::prepare(model, &base_model, base)?;
        prepared.validate_publication(&publication)?;
        let edited = structure::compare(model, &base_model, &prepared)?;
        trace_rss("compared");
        let versions = base_model
            .parts
            .dump_info
            .as_ref()
            .map(|d| d.versions.clone());
        (edited, versions, prepared.into_changes())
    };
    trace_rss("base dropped");
    let schema = mdschema::schema()?;
    let names: Vec<String> = list.iter().map(|m| m.name.clone()).collect();
    let present: HashSet<&str> = names.iter().map(String::as_str).collect();
    let slots = crate::modules::entry_slots(schema, model);
    let slot_names: HashSet<&str> = slots
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| present.contains(n))
        .collect();
    let classified = classify(model, &names, &slot_names)?;
    let resolver = Names::new(model).map_err(|what| Error::Format {
        entry: String::new(),
        what,
    })?;
    let full_name = |entry: &str| {
        resolver
            .element(entry)
            .unwrap_or_else(|_| entry.to_string())
    };
    let mut puts: HashMap<String, Put> = HashMap::new();
    let mut report = BuildReport {
        entries: list.len(),
        ..BuildReport::default()
    };

    let mode = if opts.reencode.contains(&EntryCategory::Object) {
        Mode::Strict
    } else {
        Mode::KeepEqual
    };
    let mut visited: Vec<brace::Guid> = Vec::new();
    let mut written: HashMap<brace::Guid, Vec<u8>> = HashMap::new();
    let patch = patch_entries(model, base, mode, &mut |entry, doc, patched| {
        visited.push(entry);
        let Some(root) = patched else {
            return Ok(());
        };
        let name = entry.to_string();
        let bytes = super::versions::stored_of(&brace::Doc {
            root,
            bom: doc.bom,
            lead: doc.lead.clone(),
            trailer: doc.trailer.clone(),
        })
        .map_err(PatchError::Entries)?;
        let stored = base.stored(&name).map_err(PatchError::Entries)?;
        if stored.as_deref() != Some(&bytes[..]) {
            written.insert(entry, bytes);
        }
        Ok(())
    })
    .map_err(patch_error)?;
    report.kept = patch.kept.clone();
    for entry in &visited {
        let name = entry.to_string();
        if classified.get(&name).map(|c| c.0) != Some(EntryCategory::Object) {
            return Err(Error::Format {
                entry: name,
                what: "the patch visited an entry not classified Object".into(),
            });
        }
    }
    let written_props: HashSet<(brace::Guid, String)> =
        patch.edited.values().flatten().cloned().collect();
    let mut lost: Vec<&(brace::Guid, String)> = edited_props.difference(&written_props).collect();
    lost.sort();
    if let Some((id, prop)) = lost.first() {
        return Err(Error::Build {
            object: model
                .full_name(*id)
                .map_or_else(|| id.to_string(), str::to_string),
            path: prop.clone(),
            what: "edit not written: element not replayed".into(),
        });
    }
    for entry in visited {
        let bytes = written.remove(&entry);
        let put = match patch.edited.get(&entry) {
            Some(props) => {
                let what: Vec<String> = props
                    .iter()
                    .map(|(id, prop)| {
                        if *id == entry {
                            prop.clone()
                        } else {
                            format!("{}.{prop}", model.full_name(*id).unwrap_or("?"))
                        }
                    })
                    .collect();
                Put::Changed(what.join(", "), bytes)
            }
            None if mode == Mode::Strict => Put::Reencoded(bytes),
            None => continue,
        };
        puts.insert(entry.to_string(), put);
    }

    trace_rss("patched");
    let now = opts.now.unwrap_or_else(V8Time::now);
    let replay = opts.reencode.contains(&EntryCategory::Module);
    for (name, module) in &slots {
        let Some(module) = module else {
            continue;
        };
        if !present.contains(name.as_str()) {
            continue;
        }
        let stored = base
            .stored(name)?
            .ok_or_else(|| Error::MissingEntry(name.clone()))?;
        let text = super::modules::text_of(&stored)
            .map_err(|e| at_entry(e, name))?
            .ok_or_else(|| Error::Format {
                entry: name.clone(),
                what: "the model has a module text the base entry has not".into(),
            })?;
        let put = if text != module.source {
            let bytes =
                module_entry(&stored, &module.source, Some(now)).map_err(|e| at_entry(e, name))?;
            Put::Changed("module text".into(), Some(bytes))
        } else if replay {
            let bytes =
                module_entry(&stored, &module.source, None).map_err(|e| at_entry(e, name))?;
            Put::Reencoded((bytes != stored).then_some(bytes))
        } else {
            continue;
        };
        puts.insert(name.clone(), put);
    }

    for change in form_changes {
        if puts
            .insert(
                change.entry.clone(),
                Put::Changed("form module text".into(), Some(change.stored)),
            )
            .is_some()
        {
            return Err(Error::Build {
                object: change.entry,
                path: "form module".into(),
                what: "duplicate prepared put".into(),
            });
        }
    }

    trace_rss("modules");
    if present.contains("versions") {
        let model_versions = model.parts.dump_info.as_ref().map(|d| &d.versions);
        let stored = base
            .stored("versions")?
            .ok_or_else(|| Error::MissingEntry("versions".into()))?;
        if model_versions == base_versions.as_ref() {
            if opts.reencode.contains(&EntryCategory::Root) {
                let bytes = super::versions::reencoded(&stored)?;
                puts.insert(
                    "versions".into(),
                    Put::Reencoded((bytes != stored).then_some(bytes)),
                );
            }
        } else {
            let (bytes, k) =
                super::versions::patched(&stored, model_versions.map_or(&[][..], Vec::as_slice))?;
            if k > 0 {
                puts.insert(
                    "versions".into(),
                    Put::Changed(format!("{k} versions"), Some(bytes)),
                );
            }
        }
    } else if model.parts.dump_info.as_ref().map(|d| &d.versions) != base_versions.as_ref() {
        return Err(Error::Build {
            object: "Configuration".into(),
            path: "versions".into(),
            what: "the base has no versions entry".into(),
        });
    }

    trace_rss("puts start");
    for (at, meta) in list.iter().enumerate() {
        if at == list.len() / 2 {
            trace_rss("puts half");
        }
        let name = meta.name.as_str();
        let category = classified.get(name).map_or(EntryCategory::Other, |c| c.0);
        let bytes = match puts.remove(name) {
            Some(Put::Changed(what, bytes)) => {
                let full = if name == "versions" {
                    "Configuration".to_string()
                } else {
                    full_name(name)
                };
                report.changed.push(format!("{name} {full}: {what}"));
                bytes
            }
            Some(Put::Reencoded(bytes)) => {
                *report.reencoded.entry(category).or_default() += 1;
                if bytes.is_some() {
                    report.reencode_differ_count += 1;
                    if report.reencode_differs.len() < FIRST_DIFFERS {
                        report
                            .reencode_differs
                            .push(format!("{name} {}", full_name(name)));
                    }
                }
                bytes
            }
            None => {
                report.copied += 1;
                if opts.reencode.contains(&category) {
                    *report.passthrough.entry(category).or_default() += 1;
                }
                None
            }
        };
        match bytes {
            Some(bytes) => sink.put(meta, &bytes)?,
            None => {
                let stored = base
                    .stored(name)?
                    .ok_or_else(|| Error::MissingEntry(name.to_string()))?;
                sink.put(meta, &stored)?;
            }
        }
    }
    trace_rss("puts end");
    Ok(report)
}

pub fn set_module(model: &mut Configuration, element: &str, source: Vec<u8>) -> Result<()> {
    let refuse = |what: String| Error::Build {
        object: element.to_string(),
        path: "module".into(),
        what,
    };
    let schema = mdschema::schema()?;
    let want = element.to_lowercase();
    let hits: Vec<(String, Option<ModuleKind>)> = {
        let names = Names::new(model).map_err(|what| Error::Format {
            entry: String::new(),
            what,
        })?;
        crate::modules::entry_slots(schema, model)
            .into_iter()
            .filter(|(entry, _)| {
                names
                    .element(entry)
                    .is_ok_and(|full| full.to_lowercase() == want)
            })
            .map(|(entry, m)| (entry, m.map(|m| m.kind)))
            .collect()
    };
    let with: Vec<&(String, Option<ModuleKind>)> = hits.iter().filter(|h| h.1.is_some()).collect();
    let (entry, kind) = match with.as_slice() {
        [(entry, Some(kind))] => (entry.clone(), *kind),
        [] if hits.is_empty() => return Err(refuse("unknown element".into())),
        [] => return Err(refuse(format!("no module at {element}"))),
        _ => return Err(refuse("ambiguous element".into())),
    };
    let owner = entry
        .split_once('.')
        .and_then(|(u, _)| brace::Guid::parse(u))
        .ok_or_else(|| refuse(format!("slot {entry} has no owner uuid")))?;
    let module =
        module_mut(model, owner, kind).ok_or_else(|| refuse(format!("no module at {element}")))?;
    module.source = source;
    Ok(())
}
