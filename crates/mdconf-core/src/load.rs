use crate::entries::{CfFile, Entries, EntrySource, configuration_version};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::model::{Configuration, MdObject, SubKind, SubObject};
use crate::predefined::predefined_names;
use crate::settings::decode_settings;
use brace::{Guid, Node};
use mdschema::{MdKind, Schema};
use memory_budget::profile::{Phase, phase};
use std::collections::HashMap;
use std::path::PathBuf;

pub use md_model::{LoadProfile, Stages};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterRefusal(pub memory_budget::MemoryLimit);

pub trait LoadMeter: Send + Sync {
    fn admit(&self, entry: &str, inflated: u64) -> std::result::Result<(), MeterRefusal>;
    fn hold(&self, entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        let _ = (entry, bytes);
        Ok(())
    }
    fn release(&self, bytes: u64) {
        let _ = bytes;
    }
    fn admit_copy(&self, entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.admit(entry, bytes)
    }
}

#[derive(Clone)]
pub struct LoadOptions {
    pub keep_source: bool,
    pub private_forms: Option<PathBuf>,
    pub profile: LoadProfile,
    pub extra_stages: Stages,
    pub meter: Option<std::sync::Arc<dyn LoadMeter>>,
}

impl std::fmt::Debug for LoadOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadOptions")
            .field("keep_source", &self.keep_source)
            .field("private_forms", &self.private_forms)
            .field("profile", &self.profile)
            .field("extra_stages", &self.extra_stages)
            .field("meter", &self.meter.as_ref().map(|_| "LoadMeter"))
            .finish()
    }
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            keep_source: true,
            private_forms: None,
            profile: LoadProfile::Full,
            extra_stages: Stages::NONE,
            meter: None,
        }
    }
}

#[derive(Debug)]
pub enum ConfigurationInspection {
    NoConfiguration,
    Configuration(Box<Configuration>),
}

impl CfFile {
    pub fn inspect_with(
        &self,
        opts: &LoadOptions,
        runner: &dyn crate::codec::StageRunner,
    ) -> Result<ConfigurationInspection> {
        if self.list()?.is_empty() {
            if !self.is_header_only() {
                return Err(Error::Format {
                    entry: "root".into(),
                    what: "no-configuration inspection unsupported for empty entry list beyond file header".into(),
                });
            }
            return Ok(ConfigurationInspection::NoConfiguration);
        }
        load_with(self, opts, runner)
            .map(|cfg| ConfigurationInspection::Configuration(Box::new(cfg)))
    }
}

#[doc(hidden)]
pub struct LoadCtx<'a> {
    pub schema: &'static Schema,
    pub entries: &'a Entries<'a>,
    pub opts: &'a LoadOptions,
}
impl LoadCtx<'_> {
    pub fn resource(&self) -> &'static mdschema::SchemaResource {
        self.schema.resource()
    }
}

fn shell(kind: MdKind, id: Guid, name: &str) -> MdObject {
    MdObject {
        kind,
        id,
        name: name.to_string(),
        synonym: Default::default(),
        comment: String::new(),
        generated_types: Vec::new(),
        this_node: None,
        props: Vec::new(),
        children: Vec::new(),
        forms: Vec::new(),
        templates: Vec::new(),
        recalculations: Vec::new(),
        subsystems: Vec::new(),
        modules: Vec::new(),
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    }
}

fn shell_with_subs(ix: &Index, kind: MdKind, id: Guid, name: &str) -> MdObject {
    let mut o = shell(kind, id, name);
    for &(sk, su) in ix.subs.get(&id).map(Vec::as_slice).unwrap_or_default() {
        let full = ix.names.get(&su).map(String::as_str).unwrap_or_default();
        let sub = SubObject {
            kind: sk,
            id: su,
            name: full.rsplit('.').next().unwrap_or_default().to_string(),
            props: Vec::new(),
            module: None,
            form: None,
            template: None,
        };
        match sk {
            SubKind::Form => o.forms.push(sub),
            SubKind::Template => o.templates.push(sub),
        }
    }
    o
}

fn settings_of(e: &Entries, s: &Schema, ix: &Index) -> Result<crate::settings::ConfigSettings> {
    let entry = ix.cfg_id.to_string();
    let bad = |what: &str| Error::Format {
        entry: entry.clone(),
        what: what.to_string(),
    };
    let doc = e.doc(&entry)?.ok_or_else(|| bad("not a brace document"))?;
    let crate::entries::EntryData::Brace(doc) = &*doc else {
        return Err(bad("not a brace document"));
    };
    let t: &Node = &doc.root;
    let body = t
        .get(&[3, 1, 1])
        .ok_or_else(|| bad("no configuration body"))?;
    let header = body
        .get(&[1, 1])
        .ok_or_else(|| bad("no configuration header"))?;
    decode_settings(s, body, header, &ix.names).map_err(|err| match err {
        Error::Format { what, .. } => Error::Format {
            entry: entry.clone(),
            what,
        },
        other => other,
    })
}

pub fn build_shells(ix: &Index) -> Vec<MdObject> {
    let name_of = |id: &Guid| -> Option<(MdKind, &str)> {
        match ix.objs.get(id) {
            Some((k, Some(n))) => Some((*k, n.as_str())),
            _ => None,
        }
    };
    let mut objects: Vec<MdObject> = ix
        .order
        .iter()
        .filter_map(|id| name_of(id).map(|(k, n)| shell_with_subs(ix, k, *id, n)))
        .collect();
    let mut top: HashMap<Guid, usize> = HashMap::new();
    for (i, o) in objects.iter().enumerate() {
        top.entry(o.id).or_insert(i);
    }
    let mut pool: HashMap<Guid, MdObject> = HashMap::new();
    for (child, _) in &ix.nested {
        if let Some((k, n)) = name_of(child) {
            pool.insert(*child, shell_with_subs(ix, k, *child, n));
        }
    }
    for (child, parent) in ix.nested.iter().rev() {
        let Some(mut c) = pool.remove(child) else {
            continue;
        };
        c.subsystems.reverse();
        if let Some(p) = pool.get_mut(parent) {
            p.subsystems.push(c);
        } else if let Some(p) = top.get(parent).and_then(|&i| objects.get_mut(i)) {
            p.subsystems.push(c);
        }
    }
    for o in &mut objects {
        o.subsystems.reverse();
    }
    objects
}

pub(crate) fn load_with(
    src: &dyn EntrySource,
    opts: &LoadOptions,
    runner: &dyn crate::codec::StageRunner,
) -> Result<Configuration> {
    let stages = opts.profile.stages().union(opts.extra_stages);
    let uncovered = stages
        .minus(runner.covers())
        .minus(Stages::ALL.minus(crate::codec::CODEC_STAGES));
    debug_assert!(
        uncovered == Stages::NONE || runner.probe(),
        "codec stages {uncovered:?} requested but not run"
    );
    let _phase = phase(Phase::Inventory);
    let entries = Entries::with_meter(src, opts.meter.clone())?;
    let (format, compat) = configuration_version(&entries)?;
    let resource = mdschema::for_format(format)?;
    load_selected(entries, opts, runner, stages, resource, compat)
}

fn load_selected(
    entries: Entries<'_>,
    opts: &LoadOptions,
    runner: &dyn crate::codec::StageRunner,
    stages: Stages,
    resource: &'static mdschema::SchemaResource,
    compat: u32,
) -> Result<Configuration> {
    resource.require_embedded()?;
    let schema = resource.schema()?;
    let root_info = entries.root_info()?;
    let id = root_info.id();
    let index = {
        let _phase = phase(Phase::Index);
        crate::index::build(&entries, schema)?
    };
    let predef = {
        let _phase = phase(Phase::Predefined);
        predefined_names(&entries, &index)
    };
    entries.check_refused()?;
    let settings = {
        let _phase = phase(Phase::Settings);
        settings_of(&entries, schema, &index)?
    };

    let mut diagnostics = crate::model::Diagnostics::default();
    let mut none: Vec<(String, MdKind, Guid)> = index
        .objs
        .iter()
        .filter(|(_, (_, n))| n.is_none())
        .map(|(u, (k, _))| (u.to_string(), *k, *u))
        .collect();
    none.sort();
    for (u, k, _) in none {
        diagnostics
            .unsupported
            .push(format!("{} {u}: no schema", k.as_str()));
    }
    for u in &index.missing {
        diagnostics.unsupported.push(format!("{u}: missing entry"));
    }
    for u in &index.too_deep {
        diagnostics
            .unsupported
            .push(format!("{u}: nesting too deep"));
    }

    diagnostics
        .unsupported
        .extend(index.skipped.iter().cloned());

    let shells_phase = phase(Phase::Shells);
    let mut cfg = Configuration {
        id,
        root_info: Some(root_info),
        header_id: index.header_id,
        compat,
        settings,
        objects: build_shells(&index),
        diagnostics,
        index,
        predef,
        profile: if stages == Stages::ALL && runner.covers().contains(crate::codec::CODEC_STAGES) {
            LoadProfile::Full
        } else {
            LoadProfile::Query
        },
        ..Default::default()
    };
    drop(shells_phase);
    {
        let cx = LoadCtx {
            schema,
            entries: &entries,
            opts,
        };
        {
            let _phase = phase(Phase::Objects);
            crate::objects::decode::load(&cx, &mut cfg)?;
        }
        if stages.contains(Stages::MODULES) {
            {
                let _phase = phase(Phase::Modules);
                crate::modules::load(&cx, &mut cfg)?;
            }
        }
        if stages.contains(Stages::FORMS) {
            {
                let _phase = phase(Phase::Forms);
                runner.forms(&cx, &mut cfg)?;
            }
        }
        {
            let _phase = phase(Phase::Parts);
            crate::parts::load(&cx, &mut cfg, stages)?;
        }
        if stages.contains(Stages::TEMPLATES) {
            {
                let _phase = phase(Phase::Templates);
                runner.templates(&cx, &mut cfg)?;
            }
        }
        if stages.contains(Stages::FLOWCHARTS) {
            {
                let _phase = phase(Phase::Flowcharts);
                runner.flowcharts(&cx, &mut cfg)?;
            }
        }
    }
    entries.check_refused()?;
    {
        let _phase = phase(Phase::EntriesDrop);
        drop(entries);
    }
    Ok(cfg)
}
