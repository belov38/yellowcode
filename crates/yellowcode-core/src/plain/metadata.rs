use super::scope::{Scope, ScopeArgs};
use crate::{
    Result,
    viewer::{
        self, Lang,
        names::{child_tag_en, fold_path_name, kind_from_name, sub_kind_en},
        path::{self, Target},
        tree::{self, TreeNode},
    },
};
use mdconf::{Configuration, MdKind, MdObject, Module, ModuleKind};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    io::{BufWriter, Write},
    path::PathBuf,
};

#[derive(Clone, Copy)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Format {
    Json,
    Text,
}

pub enum Command {
    ExtensionInfo {
        source: PathBuf,
        format: Format,
    },
    ExtensionObjects {
        source: PathBuf,
        format: Format,
    },
    ExtensionModules {
        source: PathBuf,
        format: Format,
        selection: Option<(String, String)>,
    },
    ExtensionIntercepts {
        source: PathBuf,
        format: Format,
    },
    Tree {
        source: PathBuf,
        depth: Option<usize>,
        path: Option<String>,
        kind: Option<String>,
        subsystems: Vec<String>,
        flat: bool,
        format: Format,
    },
    Show {
        source: PathBuf,
        object: String,
        format: Format,
    },
}

#[derive(Clone, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[serde(rename_all = "camelCase")]
struct Node {
    id: String,
    kind: String,
    node_type: String,
    path: Option<String>,
    uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<String>,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_text: Option<String>,
    has_children: bool,
    children_truncated: bool,
    children: Vec<Node>,
}

fn uuid(target: &Target<'_>) -> Option<String> {
    Some(
        match target {
            Target::Configuration(c) => c.id,
            Target::Object(o) => o.id,
            Target::Child { chain, .. } => chain.last()?.id,
            Target::Sub { sub, .. } => sub.id,
            Target::Recalc { recalc, .. } => recalc.id,
        }
        .to_string(),
    )
}

enum UuidLookup<'a> {
    Exact(HashMap<String, brace::Guid>),
    Fallback(path::Resolver<'a>),
}

impl<'a> UuidLookup<'a> {
    fn new(cfg: &'a Configuration) -> Self {
        let mut map: HashMap<String, brace::Guid> = HashMap::with_capacity(cfg.objects.len() * 4);
        map.insert("Configuration".to_string(), cfg.id);
        let mut seen_top: HashSet<(MdKind, String)> = HashSet::with_capacity(cfg.objects.len());
        let mut collision = false;
        for o in &cfg.objects {
            if !seen_top.insert((o.kind, fold_path_name(&o.name))) {
                collision = true;
                break;
            }
            let path = format!("{}.{}", o.kind.as_str(), o.name);
            if !index_object_uuids(o, &path, &mut map) {
                collision = true;
                break;
            }
        }
        if collision {
            Self::Fallback(path::Resolver::new(cfg))
        } else {
            Self::Exact(map)
        }
    }

    fn uuid_of(&self, path: &str) -> Option<String> {
        match self {
            Self::Exact(map) => map.get(path).map(ToString::to_string),
            Self::Fallback(resolver) => resolver.resolve(path).ok().and_then(|t| uuid(&t)),
        }
    }
}

fn index_children_uuids(
    owner_path: &str,
    children: &[mdconf::Child],
    map: &mut HashMap<String, brace::Guid>,
) -> bool {
    let mut seen: HashSet<(mdconf::ChildTag, String)> = HashSet::new();
    for c in children {
        if !seen.insert((c.tag, fold_path_name(&c.name))) {
            return false;
        }
        let path = format!("{owner_path}.{}.{}", child_tag_en(c.tag), c.name);
        map.insert(path.clone(), c.id);
        if !c.children.is_empty() && !index_children_uuids(&path, &c.children, map) {
            return false;
        }
    }
    true
}

fn index_object_uuids(o: &MdObject, path: &str, map: &mut HashMap<String, brace::Guid>) -> bool {
    map.insert(path.to_string(), o.id);
    if !o.subsystems.is_empty() {
        let mut seen = HashSet::new();
        for s in &o.subsystems {
            if !seen.insert(fold_path_name(&s.name)) {
                return false;
            }
            let sp = format!("{path}.Subsystem.{}", s.name);
            if !index_object_uuids(s, &sp, map) {
                return false;
            }
        }
    }
    if !o.children.is_empty() && !index_children_uuids(path, &o.children, map) {
        return false;
    }
    for list in [&o.forms, &o.templates] {
        if !list.is_empty() {
            let mut seen = HashSet::new();
            for s in list {
                if !seen.insert(fold_path_name(&s.name)) {
                    return false;
                }
                map.insert(format!("{path}.{}.{}", sub_kind_en(s.kind), s.name), s.id);
            }
        }
    }
    if !o.recalculations.is_empty() {
        let mut seen = HashSet::new();
        for r in &o.recalculations {
            if !seen.insert(fold_path_name(&r.name)) {
                return false;
            }
            map.insert(format!("{path}.Recalculation.{}", r.name), r.id);
        }
    }
    true
}

fn project(uuids: &UuidLookup<'_>, node: TreeNode, parent: &str) -> Node {
    let icon = node.icon.as_deref().unwrap_or("group:Unknown");
    let (node_type, kind) = match icon.split_once(':') {
        Some((typ, kind)) => (typ, kind),
        None if icon == "root" => ("configuration", "Configuration"),
        None => ("object", "Recalculation"),
    };
    let id = match (&node.path, &node.module) {
        (Some(path), None) => path.clone(),
        (Some(path), Some(module)) => format!("{path}::module:{module}"),
        _ => format!("{parent}::{icon}"),
    };
    let uuid = if node.module.is_none() {
        node.path.as_deref().and_then(|p| uuids.uuid_of(p))
    } else {
        None
    };
    let has_children = !node.children.is_empty();
    let kind: String = kind.into();
    let node_type: String = node_type.into();
    let children = node
        .children
        .into_iter()
        .map(|c| project(uuids, c, &id))
        .collect();
    Node {
        id,
        kind,
        node_type,
        path: node.path,
        uuid,
        module: node.module,
        label: node.label,
        type_text: node.type_text,
        has_children,
        children_truncated: false,
        children,
    }
}

fn find<'a>(root: &'a Node, uuid: &str) -> Option<&'a Node> {
    if root.uuid.as_deref() == Some(uuid) {
        return Some(root);
    }
    root.children.iter().find_map(|c| find(c, uuid))
}

fn prune(node: &mut Node, depth: usize) {
    if depth == 0 {
        node.children_truncated = node.has_children;
        node.children.clear();
    } else {
        for child in &mut node.children {
            prune(child, depth - 1);
        }
        node.children_truncated = node.children.iter().any(|c| c.children_truncated);
    }
}

fn prune_scope(node: &mut Node, scope: &Scope) -> bool {
    node.children.retain_mut(|c| prune_scope(c, scope));
    node.has_children = !node.children.is_empty();
    node.node_type == "configuration"
        || node.has_children
        || node.path.as_deref().is_some_and(|p| scope.contains(p))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FlatRow<'a> {
    id: &'a str,
    kind: &'a str,
    node_type: &'a str,
    path: Option<&'a str>,
    uuid: Option<&'a str>,
    label: &'a str,
    has_children: bool,
    children_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_text: Option<&'a str>,
    parent_id: Option<&'a str>,
    depth: usize,
}

struct FlatNodes<'a>(&'a Node);

impl Serialize for FlatNodes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        serialize_flat_dfs(self.0, None, 0, &mut seq)?;
        seq.end()
    }
}

fn serialize_flat_dfs<S: SerializeSeq>(
    node: &Node,
    parent: Option<&str>,
    depth: usize,
    seq: &mut S,
) -> std::result::Result<(), S::Error> {
    seq.serialize_element(&FlatRow {
        id: &node.id,
        kind: &node.kind,
        node_type: &node.node_type,
        path: node.path.as_deref(),
        uuid: node.uuid.as_deref(),
        label: &node.label,
        has_children: node.has_children,
        children_truncated: node.children_truncated,
        module: node.module.as_deref(),
        type_text: node.type_text.as_deref(),
        parent_id: parent,
        depth,
    })?;
    for child in &node.children {
        serialize_flat_dfs(child, Some(&node.id), depth + 1, seq)?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FlatTreeOutput<'a> {
    schema_version: u32,
    nodes: FlatNodes<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NestedTreeOutput<'a> {
    schema_version: u32,
    root: &'a Node,
}

fn text(node: &Node, depth: usize, out: &mut dyn Write) -> Result<()> {
    write!(out, "{}{}", "  ".repeat(depth), node.label)?;
    if let Some(ty) = &node.type_text {
        write!(out, ": {ty}")?;
    }
    if node.children_truncated {
        write!(out, " [children truncated]")?;
    }
    writeln!(out)?;
    for child in &node.children {
        text(child, depth + 1, out)?;
    }
    Ok(())
}

pub fn parse_entry_id(value: &str) -> std::result::Result<String, String> {
    super::extension_modules::parse_entry_id(value)
}

pub fn run(command: Command, out: &mut dyn Write) -> Result<()> {
    run_with_memory_limit(command, None, out)
}
pub fn run_with_memory_limit(
    command: Command,
    limit: Option<String>,
    out: &mut dyn Write,
) -> Result<()> {
    match command {
        Command::ExtensionModules {
            source,
            format,
            selection,
        } => super::extension_modules::run(&source, format, selection, out)?,
        Command::ExtensionIntercepts { source, format } => {
            super::extension_modules::run_intercepts(&source, format, out)?
        }
        Command::ExtensionObjects { source, format } => {
            super::extension_objects::run(&source, format, out)?
        }
        Command::ExtensionInfo { source, format } => {
            super::extension_info::run(&source, format, out)?
        }
        Command::Tree {
            source,
            depth,
            path,
            kind,
            subsystems,
            flat,
            format,
        } => {
            let memory = viewer::source::OpenMemory::resolve(limit)?;
            let mut cfg = match root_source(&source, depth, path.as_deref(), &subsystems) {
                Some(kind) => {
                    viewer::source::with_config_entries_with_memory(&source, &memory, |src| {
                        root_alone(kind, src)
                    })?
                }
                None => open_for_tree(
                    &source,
                    depth,
                    path.as_deref(),
                    kind.as_deref(),
                    &subsystems,
                    &memory,
                )?,
            };
            trim_for_tree(&mut cfg, !subsystems.is_empty());
            write_tree(
                &cfg,
                &TreeArgs {
                    depth,
                    path: path.as_deref(),
                    kind: kind.as_deref(),
                    subsystems,
                    flat,
                    format,
                },
                out,
            )?;
        }
        Command::Show {
            source,
            object,
            format,
        } => {
            let memory = viewer::source::OpenMemory::resolve(limit)?;
            let cfg = open_for_show(&source, &object, format, &memory)?;
            write_show(&cfg, &object, format, out)?;
        }
    }
    Ok(())
}

fn is_plain_module_text(bytes: &[u8]) -> bool {
    const BOM: &[u8] = b"\xef\xbb\xbf";
    let Some(body) = bytes.strip_prefix(BOM) else {
        return false;
    };
    std::str::from_utf8(body).is_ok()
        && !body.contains(&0)
        && body.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{')
}

fn has_separate_module(
    src: &dyn mdconf::EntrySource,
    present: &HashSet<String>,
    name: &str,
) -> Result<bool> {
    if !present.contains(name) {
        return Ok(false);
    }
    let Some(stored) = src.stored_owned(name)? else {
        return Ok(false);
    };
    if stored.is_empty() {
        return Ok(false);
    }
    let Some(raw) = zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT) else {
        return Ok(is_plain_module_text(&stored));
    };
    if v8cont::is_container(&raw) {
        if let Ok(c) = v8cont::Container::open(&raw) {
            if !c.entries().iter().any(|e| e.name == "text") {
                return Ok(false);
            }
            let readable = c
                .entries()
                .iter()
                .all(|e| std::io::copy(&mut c.data_reader(e), &mut std::io::sink()).is_ok());
            return Ok(readable);
        }
        return Ok(false);
    }
    Ok(is_plain_module_text(&raw))
}

fn has_lenient_form_module(raw: &[u8]) -> bool {
    const BOM: &[u8] = b"\xef\xbb\xbf";
    let Some(body) = raw.strip_prefix(BOM) else {
        return false;
    };
    if body.first() != Some(&b'{') {
        return false;
    }
    let (mut depth, mut in_str, mut commas, mut i) = (0usize, false, 0usize, 0usize);
    while let Some(&b) = body.get(i) {
        match (in_str, b) {
            (true, b'"') => in_str = false,
            (true, _) => {}
            (false, b'"') => in_str = true,
            (false, b'{') => depth += 1,
            (false, b'}') => match depth.checked_sub(1) {
                Some(next) => depth = next,
                None => return false,
            },
            (false, b',') if depth == 1 => {
                commas += 1;
                if commas == 2 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if commas != 2 {
        return false;
    }
    let Some(slice) = body.get(i + 1..) else {
        return false;
    };
    let mut at = 0usize;
    while slice.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    if slice.get(at) != Some(&b'"') {
        return false;
    }
    at += 1;
    let start = at;
    let mut len = 0usize;
    loop {
        match slice.get(at).copied() {
            Some(b'"') if slice.get(at + 1) == Some(&b'"') => {
                at += 2;
                len += 1;
            }
            Some(b'"') => {
                return len > 0
                    && slice
                        .get(start..at)
                        .is_some_and(|s| std::str::from_utf8(s).is_ok());
            }
            Some(_) => {
                at += 1;
                len += 1;
            }
            None => return false,
        }
    }
}

fn has_common_form_module(
    src: &dyn mdconf::EntrySource,
    present: &HashSet<String>,
    id: brace::Guid,
) -> Result<bool> {
    let name = format!("{id}.0");
    if !present.contains(&name) {
        return Ok(false);
    }
    let Some(stored) = src.stored_owned(&name)? else {
        return Ok(false);
    };
    match mdconf::decode_stored(&stored)? {
        mdconf::EntryData::Brace(doc) => Ok(doc.bom
            && doc.lead.is_empty()
            && matches!(doc.root.at(2), Some(brace::Node::Str(s)) if !s.is_empty())),
        mdconf::EntryData::Bytes(raw) => Ok(has_lenient_form_module(&raw)),
        _ => Ok(false),
    }
}

fn populate_object_tree_modules(
    src: &dyn mdconf::EntrySource,
    present: &HashSet<String>,
    o: &mut MdObject,
) -> Result<()> {
    for (suf, file) in mdconf::startup::modules(o.kind.as_str()) {
        let Some(kind) = mdconf::startup::module_kind(file) else {
            continue;
        };
        if has_separate_module(src, present, &format!("{}.{suf}", o.id))? {
            o.modules.push(Module {
                kind,
                source: Vec::new(),
            });
        }
    }
    for r in &mut o.recalculations {
        if has_separate_module(src, present, &format!("{}.0", r.id))? {
            r.module = Some(Module {
                kind: ModuleKind::RecordSetModule,
                source: Vec::new(),
            });
        }
    }
    if o.kind == MdKind::CommonForm && has_common_form_module(src, present, o.id)? {
        o.modules.push(Module {
            kind: ModuleKind::FormModule,
            source: Vec::new(),
        });
    }
    for s in &mut o.subsystems {
        populate_object_tree_modules(src, present, s)?;
    }
    Ok(())
}

fn populate_tree_modules(src: &dyn mdconf::EntrySource, cfg: &mut Configuration) -> Result<()> {
    if cfg.objects.is_empty() {
        return Ok(());
    }
    let present: HashSet<String> = src.list()?.into_iter().map(|m| m.name).collect();
    for o in &mut cfg.objects {
        populate_object_tree_modules(src, &present, o)?;
    }
    Ok(())
}

fn is_configuration_path(path: &str) -> bool {
    let mut segs = path.split('.').map(str::trim);
    match (segs.next(), segs.next()) {
        (Some(first), None) => kind_from_name(first) == Some(MdKind::Configuration),
        _ => false,
    }
}

fn target_object_key(path: &str) -> Option<(MdKind, String)> {
    let mut segs = path.split('.').map(str::trim);
    let kind = kind_from_name(segs.next()?)?;
    if kind == MdKind::Configuration {
        return None;
    }
    let name = segs.next().filter(|s| !s.is_empty())?;
    Some((kind, fold_path_name(name)))
}

fn trim_for_tree(cfg: &mut Configuration, keep_subsystem_content: bool) {
    fn trim_child(c: &mut mdconf::Child) {
        c.props.retain(|p| p.name == "Type");
        c.synonym.0.clear();
        c.comment.clear();
        c.generated_types.clear();
        for nested in &mut c.children {
            trim_child(nested);
        }
    }
    fn trim_object(o: &mut MdObject, keep_subsystem_content: bool) {
        let keep_content = keep_subsystem_content && o.kind == MdKind::Subsystem;
        o.props
            .retain(|p| p.name == "Type" || (keep_content && p.name == "Content"));
        o.synonym.0.clear();
        o.comment.clear();
        o.generated_types.clear();
        o.parts = Default::default();
        o.external.clear();
        for c in &mut o.children {
            trim_child(c);
        }
        for s in o.forms.iter_mut().chain(&mut o.templates) {
            s.props.clear();
        }
        for r in &mut o.recalculations {
            r.props.clear();
            r.synonym.0.clear();
            r.comment.clear();
            r.generated_types.clear();
        }
        for sub in &mut o.subsystems {
            trim_object(sub, keep_subsystem_content);
        }
    }
    cfg.props.retain(|p| p.name == "Name");
    cfg.index = Default::default();
    cfg.predef.clear();
    cfg.parts = Default::default();
    for o in &mut cfg.objects {
        trim_object(o, keep_subsystem_content);
    }
}

fn parse_kinds_arg(kind: Option<&str>) -> Result<Vec<MdKind>> {
    Ok(kind
        .map(|kinds| {
            kinds
                .split(',')
                .map(|kind| {
                    viewer::names::kind_from_name(kind.trim())
                        .ok_or_else(|| format!("unknown metadata kind: {kind}"))
                })
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default())
}

fn open_for_tree(
    source: &std::path::Path,
    depth: Option<usize>,
    path: Option<&str>,
    kind: Option<&str>,
    subsystems: &[String],
    memory: &viewer::source::OpenMemory,
) -> Result<Configuration> {
    let kinds_filter = parse_kinds_arg(kind)?;
    let need_modules = !matches!((depth, path.is_some()), (Some(0 | 1), false));
    let base_opts = super::load_options(super::Reads::Tree {
        depth,
        rooted: path.is_some(),
    });

    if subsystems.is_empty()
        && let Some(p) = path
        && let Some((target_kind, target_name)) = target_object_key(p)
    {
        let keep_name = target_name.clone();
        let mut opts = base_opts.clone();
        opts.objects = Some(mdconf::ObjectFilter {
            index_kinds: None,
            keep: std::sync::Arc::new(move |k, n| {
                k == target_kind && fold_path_name(n) == keep_name
            }),
        });
        let (cfg, found) = viewer::source::open_with_entries_and_memory(
            source,
            &opts,
            super::BOUNDED_CACHE,
            memory,
            |cfg, src| {
                if path::resolve(cfg, p).is_ok() {
                    if need_modules {
                        populate_tree_modules(src, cfg)?;
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            },
        )?;
        if found {
            return Ok(cfg);
        }
        return viewer::source::open_with_memory_bounded(
            source,
            &base_opts,
            super::BOUNDED_CACHE,
            memory,
        );
    }

    if subsystems.is_empty() && path.is_none() && !kinds_filter.is_empty() {
        let keep_kinds = kinds_filter;
        let mut opts = base_opts;
        opts.objects = Some(mdconf::ObjectFilter {
            index_kinds: None,
            keep: std::sync::Arc::new(move |k, _| keep_kinds.contains(&k)),
        });
        let (cfg, ()) = viewer::source::open_with_entries_and_memory(
            source,
            &opts,
            super::BOUNDED_CACHE,
            memory,
            |cfg, src| {
                if need_modules {
                    populate_tree_modules(src, cfg)?;
                }
                Ok(())
            },
        )?;
        return Ok(cfg);
    }

    let (cfg, ()) = viewer::source::open_with_entries_and_memory(
        source,
        &base_opts,
        super::BOUNDED_CACHE,
        memory,
        |cfg, src| {
            if need_modules {
                populate_tree_modules(src, cfg)?;
            }
            Ok(())
        },
    )?;
    Ok(cfg)
}

fn open_for_show(
    source: &std::path::Path,
    object: &str,
    format: Format,
    memory: &viewer::source::OpenMemory,
) -> Result<Configuration> {
    let base_opts = super::load_options(super::Reads::Show);
    if is_configuration_path(object) {
        return match format {
            Format::Text => {
                let mut opts = base_opts;
                opts.objects = Some(mdconf::ObjectFilter {
                    index_kinds: None,
                    keep: std::sync::Arc::new(|_, _| false),
                });
                viewer::source::open_with_memory_bounded(
                    source,
                    &opts,
                    super::BOUNDED_CACHE,
                    memory,
                )
            }
            Format::Json => {
                let (cfg, ()) = viewer::source::open_with_entries_and_memory(
                    source,
                    &base_opts,
                    super::BOUNDED_CACHE,
                    memory,
                    |cfg, src| populate_tree_modules(src, cfg),
                )?;
                Ok(cfg)
            }
        };
    }

    if let Some((target_kind, target_name)) = target_object_key(object) {
        let keep_name = target_name.clone();
        let mut opts = base_opts.clone();
        opts.objects = Some(mdconf::ObjectFilter {
            index_kinds: None,
            keep: std::sync::Arc::new(move |k, n| {
                k == MdKind::DefinedType || (k == target_kind && fold_path_name(n) == keep_name)
            }),
        });
        let (cfg, found) = viewer::source::open_with_entries_and_memory(
            source,
            &opts,
            super::BOUNDED_CACHE,
            memory,
            |cfg, src| {
                if path::resolve(cfg, object).is_ok() {
                    if matches!(format, Format::Json) {
                        let present: HashSet<String> =
                            src.list()?.into_iter().map(|m| m.name).collect();
                        for o in &mut cfg.objects {
                            if o.kind == target_kind && fold_path_name(&o.name) == target_name {
                                populate_object_tree_modules(src, &present, o)?;
                            }
                        }
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            },
        )?;
        if found {
            return Ok(cfg);
        }
    }

    viewer::source::open_with_memory_bounded(source, &base_opts, super::BOUNDED_CACHE, memory)
}

fn root_source(
    source: &std::path::Path,
    depth: Option<usize>,
    path: Option<&str>,
    subsystems: &[String],
) -> Option<viewer::source::SourceKind> {
    use viewer::source::SourceKind;
    if depth != Some(0) || path.is_some() || !subsystems.is_empty() {
        return None;
    }
    match viewer::source::detect(&viewer::source::resolve(source)) {
        Ok(kind @ (SourceKind::Cf | SourceKind::Dt | SourceKind::Infobase)) => Some(kind),
        _ => None,
    }
}

fn root_alone(
    kind: viewer::source::SourceKind,
    src: &dyn mdconf::EntrySource,
) -> Result<Configuration> {
    if kind == viewer::source::SourceKind::Infobase && src.list()?.is_empty() {
        return Ok(Configuration::default());
    }
    Ok(mdconf::root_facts::configuration_root(src)?)
}

struct TreeArgs<'a> {
    depth: Option<usize>,
    path: Option<&'a str>,
    kind: Option<&'a str>,
    subsystems: Vec<String>,
    flat: bool,
    format: Format,
}

fn write_tree(cfg: &Configuration, args: &TreeArgs<'_>, out: &mut dyn Write) -> Result<()> {
    let &TreeArgs {
        depth,
        path,
        kind,
        flat,
        format,
        ..
    } = args;
    let kinds = parse_kinds_arg(kind)?;
    let scope = Scope::resolve(
        cfg,
        &ScopeArgs {
            subsystems: args.subsystems.clone(),
            ..Default::default()
        },
    )?;
    let built = depth
        .filter(|_| path.is_none() && scope.is_all())
        .map(|d| d.saturating_add(1));
    let mut root = {
        let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::Tree);
        project(
            &UuidLookup::new(cfg),
            tree::tree(cfg, built, &kinds, Lang::Ru),
            "",
        )
    };
    if !scope.is_all() {
        prune_scope(&mut root, &scope);
    }
    if let Some(path) = path {
        let target = path::resolve(cfg, path)?;
        let id = uuid(&target).ok_or("metadata target has no UUID")?;
        let selected =
            find(&root, &id).ok_or("metadata path excluded by kind or subsystem filter")?;
        root = selected.clone();
    }
    if let Some(depth) = depth {
        prune(&mut root, depth);
    }
    let mut w = BufWriter::with_capacity(64 * 1024, out);
    match format {
        Format::Text => text(&root, 0, &mut w)?,
        Format::Json => {
            if flat {
                let payload = FlatTreeOutput {
                    schema_version: 1,
                    nodes: FlatNodes(&root),
                };
                serde_json::to_writer_pretty(&mut w, &payload)?;
            } else {
                let payload = NestedTreeOutput {
                    schema_version: 1,
                    root: &root,
                };
                serde_json::to_writer_pretty(&mut w, &payload)?;
            }
            writeln!(w)?;
        }
    }
    w.flush()?;
    Ok(())
}

fn write_show(
    cfg: &Configuration,
    object: &str,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let target = path::resolve(cfg, object)?;
    let only_kind: Vec<MdKind> = match &target {
        Target::Configuration(_) => Vec::new(),
        Target::Object(o) => vec![o.kind],
        Target::Child { owner, .. } | Target::Sub { owner, .. } | Target::Recalc { owner, .. } => {
            vec![owner.kind]
        }
    };
    let root = project(
        &UuidLookup::new(cfg),
        tree::tree(cfg, None, &only_kind, Lang::Ru),
        "",
    );
    let id = uuid(&target).ok_or("metadata target has no UUID")?;
    let selected = find(&root, &id).ok_or("metadata path absent from viewer tree")?;
    let props = viewer::show::show(cfg, &target, Lang::Ru);
    let mut w = BufWriter::with_capacity(64 * 1024, out);
    match format {
        Format::Json => {
            serde_json::to_writer_pretty(
                &mut w,
                &json!({"schemaVersion": 1, "object": selected, "properties": props}),
            )?;
            writeln!(w)?;
        }
        Format::Text => {
            writeln!(w, "{}", selected.path.as_deref().unwrap_or(&selected.label))?;
            for prop in props {
                writeln!(w, "{}: {}", prop.title, prop.text)?;
            }
        }
    }
    w.flush()?;
    Ok(())
}
