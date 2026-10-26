use super::{PartsDump, PartsOptions, XmlFormat, load_parts, parse_part, shell_xml, walk::err};
use crate::{Error, Result};
use memory_budget::{Limits, MemoryBudget};
use quick_xml::{Reader, events::Event};
use std::{collections::BTreeSet, io::Write, path::Path};

struct Element {
    name: String,
    parent: Option<usize>,
    children: Vec<usize>,
    start: usize,
    open_end: usize,
    close_start: usize,
    end: usize,
    metadata_ns: bool,
}
struct Edit {
    start: usize,
    end: usize,
    text: String,
}
fn element<'a>(nodes: &'a [Element], id: usize, path: &Path) -> Result<&'a Element> {
    nodes
        .get(id)
        .ok_or_else(|| err(path, "translation element index"))
}
fn scan(bytes: &[u8], path: &Path, o: &PartsOptions, work: &mut u64) -> Result<Vec<Element>> {
    let budget = MemoryBudget::new(Limits::new(o.objects.shells.max_xml_memory));
    let arena = parse_part(bytes, path, &o.objects.shells, &budget, work)?;
    let mut nodes: Vec<Element> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let input = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let offset = bytes.len() - input.len();
    let mut reader = Reader::from_reader(input);
    loop {
        let start = reader.buffer_position() as usize + offset;
        let event = reader.read_event().map_err(|e| err(path, e.to_string()))?;
        let end = reader.buffer_position() as usize + offset;
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = bytes.get(end.saturating_sub(2)..end) == Some(b"/>");
                let id = nodes.len();
                let parent = stack.last().copied();
                if let Some(parent) = parent {
                    nodes
                        .get_mut(parent)
                        .ok_or_else(|| err(path, "translation parent"))?
                        .children
                        .push(id);
                }
                nodes.push(Element {
                    name: std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| err(path, e.to_string()))?
                        .into(),
                    parent,
                    children: Vec::new(),
                    start,
                    open_end: end,
                    close_start: end,
                    end,
                    metadata_ns: false,
                });
                if !empty {
                    stack.push(id);
                }
            }
            Event::End(_) => {
                let id = stack.pop().ok_or_else(|| err(path, "translation end"))?;
                let n = nodes
                    .get_mut(id)
                    .ok_or_else(|| err(path, "translation end index"))?;
                n.close_start = start;
                n.end = end;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let root = arena
        .root()
        .ok_or_else(|| err(path, "missing translation root"))?;
    let mut pending = vec![(root, 0)];
    while let Some((node, id)) = pending.pop() {
        let record = nodes
            .get_mut(id)
            .ok_or_else(|| err(path, "translation node mismatch"))?;
        record.metadata_ns = node.ns() == Some(shell_xml::MD);
        if record.metadata_ns && record.name != node.local() {
            return Err(err(path, "unqualified 2.20 namespace prefix"));
        }
        let children: Vec<_> = node
            .children()
            .zip(record.children.iter().copied())
            .collect();
        pending.extend(children);
    }
    Ok(nodes)
}
fn context(nodes: &[Element], mut id: usize, expected: &str) -> bool {
    for name in expected.rsplit('/') {
        let Some(node) = nodes.get(id) else {
            return false;
        };
        if !node.metadata_ns || node.name != name {
            return false;
        }
        let Some(parent) = node.parent else {
            return false;
        };
        id = parent;
    }
    nodes
        .get(id)
        .is_some_and(|n| n.name == "MetaDataObject" && n.parent.is_none())
}
fn metadata(bytes: &[u8], path: &Path, o: &PartsOptions, work: &mut u64) -> Result<Vec<Edit>> {
    let source = std::str::from_utf8(bytes).map_err(|e| err(path, e.to_string()))?;
    let nodes = scan(bytes, path, o, work)?;
    let root = element(&nodes, 0, path)?;
    let rules = mdschema::fmt220::rules()?;
    if source.get(root.start..root.open_end) != Some(&rules.headers.before) {
        return Err(err(path, "unqualified 2.20 metadata header"));
    }
    let mut edits = vec![Edit {
        start: root.start,
        end: root.open_end,
        text: rules.headers.after.clone(),
    }];
    for (id, parent) in nodes.iter().enumerate() {
        let defaults: Vec<_> = rules
            .defaults
            .iter()
            .filter(|r| context(&nodes, id, &r.context))
            .collect();
        if !defaults.is_empty() {
            for rule in &defaults {
                if parent
                    .children
                    .iter()
                    .any(|i| nodes.get(*i).is_some_and(|n| n.name == rule.name))
                {
                    return Err(err(
                        path,
                        format!("already contains 2.21 property {}", rule.name),
                    ));
                }
            }
            let mut inserted = BTreeSet::new();
            for child in &parent.children {
                let node = element(&nodes, *child, path)?;
                let line = source
                    .get(..node.start)
                    .and_then(|s| s.rfind('\n'))
                    .map_or(0, |n| n + 1);
                let indent = source
                    .get(line..node.start)
                    .ok_or_else(|| err(path, "translation indentation"))?;
                if !indent.bytes().all(|c| c == b'\t') {
                    return Err(err(path, "unqualified 2.20 indentation"));
                }
                let mut anchor = node.name.as_str();
                let mut text = String::new();
                while let Some(rule) = defaults.iter().find(|r| r.after == anchor) {
                    if !inserted.insert(rule.name.as_str()) {
                        return Err(err(path, "cyclic translation defaults"));
                    }
                    text.push_str("\r\n");
                    text.push_str(indent);
                    text.push_str(&rule.xml);
                    anchor = &rule.name;
                }
                if !text.is_empty() {
                    if source.get(node.end..node.end.saturating_add(2)) != Some("\r\n")
                        || source.get(line.saturating_sub(2)..line) != Some("\r\n")
                    {
                        return Err(err(path, "unqualified 2.20 layout"));
                    }
                    edits.push(Edit {
                        start: node.end,
                        end: node.end,
                        text,
                    });
                }
            }
            if inserted.len() != defaults.len() {
                return Err(err(path, "missing 2.20 default anchor"));
            }
        }
        for rule in &rules.changes {
            if context(&nodes, id, &rule.context) {
                if !parent.children.is_empty()
                    || source.get(parent.open_end..parent.close_start) != Some(&rule.before)
                {
                    return Err(err(path, "unqualified 2.20 property value"));
                }
                edits.push(Edit {
                    start: parent.open_end,
                    end: parent.close_start,
                    text: rule.after.clone(),
                });
            }
        }
    }
    edits.sort_by_key(|e| e.start);
    Ok(edits)
}
fn dump_info_header(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
) -> Result<Vec<Edit>> {
    let nodes = scan(bytes, path, o, work)?;
    let root = element(&nodes, 0, path)?;
    if root.name != "ConfigDumpInfo" {
        return Err(err(path, "wrong ConfigDumpInfo root"));
    }
    let opening = std::str::from_utf8(
        bytes
            .get(root.start..root.open_end)
            .ok_or_else(|| err(path, "ConfigDumpInfo header"))?,
    )
    .map_err(|e| err(path, e.to_string()))?;
    if opening.matches(" version=\"2.20\"").count() != 1 {
        return Err(err(path, "unqualified ConfigDumpInfo format"));
    }
    Ok(vec![Edit {
        start: root.start,
        end: root.open_end,
        text: opening.replacen(" version=\"2.20\"", " version=\"2.21\"", 1),
    }])
}

pub fn translate_2_20(dump: PartsDump, o: &PartsOptions) -> Result<PartsDump> {
    let origin = dump
        .configuration
        .xml_origin
        .as_ref()
        .ok_or_else(|| err(Path::new(""), "missing XML origin"))?;
    if origin.format() != XmlFormat::V2_20 {
        return Err(err(Path::new(""), "translation requires XML 2.20"));
    }
    for (path, file) in origin.files() {
        if !matches!(
            file.part(),
            md_model::xml_origin::XmlPart::Objects
                | md_model::xml_origin::XmlPart::Configuration
                | md_model::xml_origin::XmlPart::Modules
        ) {
            return Err(err(
                path,
                "unqualified 2.20 part requires exact translation rules",
            ));
        }
        if file.bytes().len() as u64 > o.max_retained_file_bytes {
            return Err(err(path, "retained XML byte limit"));
        }
    }
    let mut plans = Vec::new();
    let mut total = 0u64;
    let mut work = 0;
    for (path, file) in origin.files() {
        let edits = if path == Path::new("ConfigDumpInfo.xml") {
            dump_info_header(file.bytes(), path, o, &mut work)?
        } else if path.extension().is_some_and(|e| e == "xml") {
            metadata(file.bytes(), path, o, &mut work)?
        } else {
            Vec::new()
        };
        let mut size = file.bytes().len() as u64;
        let mut end = 0;
        for edit in &edits {
            if edit.start < end || edit.end < edit.start {
                return Err(err(path, "overlapping translation edits"));
            }
            size = size
                .checked_sub((edit.end - edit.start) as u64)
                .and_then(|n| n.checked_add(edit.text.len() as u64))
                .ok_or_else(|| err(path, "translated XML byte limit"))?;
            end = edit.end;
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| err(path, "translated XML byte limit"))?;
        if size
            > o.max_retained_file_bytes
                .min(o.objects.shells.max_file_bytes)
            || total > o.max_retained_bytes.min(o.objects.shells.max_total_bytes)
        {
            return Err(err(path, "translated XML byte limit"));
        }
        plans.push((path, file, edits));
    }
    let temp = tempfile::tempdir().map_err(|e| err(Path::new(""), e.to_string()))?;
    for (path, file, edits) in &plans {
        let destination = temp.path().join(path);
        let parent = destination
            .parent()
            .ok_or_else(|| err(path, "translation parent"))?;
        std::fs::create_dir_all(parent).map_err(|e| err(path, e.to_string()))?;
        let mut out = std::fs::File::create(&destination).map_err(|e| err(path, e.to_string()))?;
        let mut start = 0;
        for edit in edits {
            out.write_all(
                file.bytes()
                    .get(start..edit.start)
                    .ok_or_else(|| err(path, "translation span"))?,
            )
            .map_err(|e| err(path, e.to_string()))?;
            out.write_all(edit.text.as_bytes())
                .map_err(|e| err(path, e.to_string()))?;
            start = edit.end;
        }
        out.write_all(
            file.bytes()
                .get(start..)
                .ok_or_else(|| err(path, "translation tail"))?,
        )
        .map_err(|e| err(path, e.to_string()))?;
    }
    let mut translated = load_parts(temp.path(), o).map_err(|e| match e {
        Error::Xml { path, what } => Error::Xml {
            path: path
                .strip_prefix(temp.path())
                .map_or_else(|_| path.clone(), Path::to_path_buf),
            what,
        },
        other => other,
    })?;
    if let Some(info) = translated
        .configuration
        .xml_origin
        .as_ref()
        .and_then(|origin| origin.get(Path::new("ConfigDumpInfo.xml")))
    {
        let versions = super::read_named_versions(&translated.configuration, info.bytes(), o)?;
        let names = md_xmlout::codec::parts::configdump::Names::new(&translated.configuration)
            .map_err(|_| err(Path::new("ConfigDumpInfo.xml"), "version name index"))?;
        let configuration_name = names
            .element(&translated.configuration.id.to_string())
            .map_err(|_| err(Path::new("ConfigDumpInfo.xml"), "version name index"))?;
        let modules = module_names(&translated.configuration, &configuration_name);
        for (id, _) in &versions.versions {
            if id.contains('.')
                && names
                    .element(id)
                    .ok()
                    .is_none_or(|name| !modules.contains(&name))
            {
                return Err(err(
                    Path::new("ConfigDumpInfo.xml"),
                    "unqualified 2.20 version part requires an exact omission/translation rule",
                ));
            }
        }
        translated.configuration.parts.dump_info = Some(versions);
    }
    Ok(translated)
}

fn module_names(cfg: &crate::Configuration, configuration_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for m in &cfg.modules {
        names.insert(format!("{configuration_name}.{:?}", m.kind));
    }
    fn objects(
        cfg: &crate::Configuration,
        items: &[crate::MdObject],
        names: &mut BTreeSet<String>,
    ) {
        for object in items {
            if let Some(full) = cfg.index.names.get(&object.id) {
                for module in &object.modules {
                    names.insert(format!("{full}.{:?}", module.kind));
                }
            }
            for (id, module) in object
                .children
                .iter()
                .filter_map(|child| child.module.as_ref().map(|module| (child.id, module)))
                .chain(object.recalculations.iter().filter_map(|recalculation| {
                    recalculation
                        .module
                        .as_ref()
                        .map(|module| (recalculation.id, module))
                }))
            {
                if let Some(full) = cfg.index.names.get(&id) {
                    names.insert(format!("{full}.{:?}", module.kind));
                }
            }
            objects(cfg, &object.subsystems, names);
        }
    }
    objects(cfg, &cfg.objects, &mut names);
    names
}
