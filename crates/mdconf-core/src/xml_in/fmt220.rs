use super::{PartsDump, PartsOptions, XmlFormat, load_parts, parse_part, shell_xml, walk::err};
use crate::{
    Error, Result,
    allocation::{Allocation, Owned},
};
use memory_budget::{BudgetedText, BudgetedVec, Category, Limits, MemoryBudget};
use std::{collections::BTreeSet, io::Write, path::Path};

struct Element<'s> {
    name: &'s str,
    parent: Option<usize>,
    children: BudgetedVec<usize>,
    start: usize,
    open_end: usize,
    close_start: usize,
    end: usize,
    metadata_ns: bool,
    form_ns: bool,
}
struct Edit {
    start: usize,
    end: usize,
    text: Owned<String>,
}
fn element<'a, 's>(nodes: &'a [Element<'s>], id: usize, path: &Path) -> Result<&'a Element<'s>> {
    nodes
        .get(id)
        .ok_or_else(|| err(path, "translation element index"))
}
fn vector<T>(budget: &MemoryBudget, path: &Path) -> Result<BudgetedVec<T>> {
    BudgetedVec::new(budget, Category::Metadata).map_err(|e| err(path, e.to_string()))
}
fn owned_text(budget: &MemoryBudget, path: &Path, parts: &[&str]) -> Result<Owned<String>> {
    Allocation::Managed(budget)
        .text(parts)
        .map_err(|e| err(path, e.to_string()))
}
fn scan<'s>(
    bytes: &'s [u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Element<'s>>> {
    let arena = parse_part(bytes, path, &o.objects.shells, budget, work)?;
    let mut nodes = scan_spans(bytes, path, budget)?;
    let root = arena
        .root()
        .ok_or_else(|| err(path, "missing translation root"))?;
    let mut pending = vector(budget, path)?;
    pending
        .try_push((root, 0usize))
        .map_err(|e| err(path, e.to_string()))?;
    while let Some((node, id)) = pending.pop() {
        let record = nodes
            .get_mut(id)
            .ok_or_else(|| err(path, "translation node mismatch"))?;
        if node.children().count() != record.children.len()
            || record.name.rsplit(':').next() != Some(node.local())
        {
            return Err(err(path, "translation node mismatch"));
        }
        record.metadata_ns = node.ns() == Some(shell_xml::MD);
        record.form_ns = node.ns() == Some("http://v8.1c.ru/8.3/xcf/logform");
        if record.metadata_ns && record.name != node.local() {
            return Err(err(path, "unqualified 2.20 namespace prefix"));
        }
        for child in node.children().zip(record.children.iter().copied()) {
            pending
                .try_push(child)
                .map_err(|e| err(path, e.to_string()))?;
        }
    }
    Ok(nodes)
}

fn scan_spans<'s>(
    bytes: &'s [u8],
    path: &Path,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Element<'s>>> {
    let source = std::str::from_utf8(bytes).map_err(|e| err(path, e.to_string()))?;
    let mut nodes: BudgetedVec<Element<'s>> = vector(budget, path)?;
    let mut stack: BudgetedVec<usize> = vector(budget, path)?;
    let mut position = 0;
    while let Some(relative) = bytes
        .get(position..)
        .and_then(|tail| tail.iter().position(|b| *b == b'<'))
    {
        let start = position + relative;
        let tail = bytes
            .get(start..)
            .ok_or_else(|| err(path, "translation markup span"))?;
        let terminator: Option<&[u8]> = if tail.starts_with(b"<!--") {
            Some(b"-->")
        } else if tail.starts_with(b"<![CDATA[") {
            Some(b"]]>")
        } else if tail.starts_with(b"<?") {
            Some(b"?>")
        } else {
            None
        };
        if let Some(terminator) = terminator {
            let length = tail
                .windows(terminator.len())
                .position(|w| w == terminator)
                .ok_or_else(|| err(path, "unterminated translation markup"))?;
            position = start + length + terminator.len();
            continue;
        }
        if tail.starts_with(b"<!") {
            return Err(err(path, "unsupported translation declaration"));
        }
        let mut quote = None;
        let mut closing = None;
        for (offset, &byte) in tail.iter().enumerate().skip(1) {
            match (quote, byte) {
                (Some(open), byte) if open == byte => quote = None,
                (None, b'\'' | b'"') => quote = Some(byte),
                (None, b'>') => {
                    closing = Some(offset);
                    break;
                }
                _ => {}
            }
        }
        let end = start + closing.ok_or_else(|| err(path, "unterminated translation tag"))? + 1;
        let is_end = tail.starts_with(b"</");
        let name_start = start + if is_end { 2 } else { 1 };
        let name_len = bytes
            .get(name_start..end)
            .and_then(|name| {
                name.iter()
                    .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
            })
            .ok_or_else(|| err(path, "translation name span"))?;
        let name = source
            .get(name_start..name_start + name_len)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| err(path, "translation name span"))?;
        if is_end {
            let id = stack.pop().ok_or_else(|| err(path, "translation end"))?;
            let node = nodes
                .get_mut(id)
                .ok_or_else(|| err(path, "translation end index"))?;
            if node.name != name {
                return Err(err(path, "translation end name"));
            }
            node.close_start = start;
            node.end = end;
        } else {
            let id = nodes.len();
            let parent = stack.last().copied();
            if let Some(parent) = parent {
                nodes
                    .get_mut(parent)
                    .ok_or_else(|| err(path, "translation parent"))?
                    .children
                    .try_push(id)
                    .map_err(|e| err(path, e.to_string()))?;
            }
            nodes
                .try_push(Element {
                    name,
                    parent,
                    children: vector(budget, path)?,
                    start,
                    open_end: end,
                    close_start: end,
                    end,
                    metadata_ns: false,
                    form_ns: false,
                })
                .map_err(|e| err(path, e.to_string()))?;
            if bytes.get(end.saturating_sub(2)..end) != Some(b"/>") {
                stack.try_push(id).map_err(|e| err(path, e.to_string()))?;
            }
        }
        position = end;
    }
    if !stack.is_empty() {
        return Err(err(path, "unclosed translation element"));
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
fn metadata(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Edit>> {
    let source = std::str::from_utf8(bytes).map_err(|e| err(path, e.to_string()))?;
    let nodes = scan(bytes, path, o, work, budget)?;
    let root = element(&nodes, 0, path)?;
    let rules = mdschema::fmt220::rules()?;
    if source.get(root.start..root.open_end) != Some(&rules.headers.before) {
        return Err(err(path, "unqualified 2.20 metadata header"));
    }
    let mut edits = vector(budget, path)?;
    edits
        .try_push(Edit {
            start: root.start,
            end: root.open_end,
            text: owned_text(budget, path, &[&rules.headers.after])?,
        })
        .map_err(|e| err(path, e.to_string()))?;
    for (id, parent) in nodes.iter().enumerate() {
        let defaults = rules
            .defaults
            .iter()
            .filter(|r| context(&nodes, id, &r.context));
        if defaults.clone().next().is_some() {
            for rule in defaults.clone() {
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
            let mut inserted = vector(budget, path)?;
            for child in parent.children.iter() {
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
                let mut anchor = node.name;
                let mut text =
                    BudgetedText::new(budget, Category::Metadata, o.max_retained_file_bytes)
                        .map_err(|e| err(path, e.to_string()))?;
                while let Some(rule) = defaults.clone().find(|r| r.after == anchor) {
                    if inserted.contains(&rule.name.as_str()) {
                        return Err(err(path, "cyclic translation defaults"));
                    }
                    inserted
                        .try_push(rule.name.as_str())
                        .map_err(|e| err(path, e.to_string()))?;
                    for part in ["\r\n", indent, rule.xml.as_str()] {
                        text.try_push_str(part)
                            .map_err(|e| err(path, e.to_string()))?;
                    }
                    anchor = &rule.name;
                }
                if !text.as_str().is_empty() {
                    if source.get(node.end..node.end.saturating_add(2)) != Some("\r\n")
                        || source.get(line.saturating_sub(2)..line) != Some("\r\n")
                    {
                        return Err(err(path, "unqualified 2.20 layout"));
                    }
                    edits
                        .try_push(Edit {
                            start: node.end,
                            end: node.end,
                            text: Owned::Managed(
                                text.into_string().map_err(|e| err(path, e.to_string()))?,
                            ),
                        })
                        .map_err(|e| err(path, e.to_string()))?;
                }
            }
            if inserted.len() != defaults.count() {
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
                edits
                    .try_push(Edit {
                        start: parent.open_end,
                        end: parent.close_start,
                        text: owned_text(budget, path, &[&rule.after])?,
                    })
                    .map_err(|e| err(path, e.to_string()))?;
            }
        }
    }
    edits.sort_unstable_by_key(|e| e.start);
    Ok(edits)
}
fn header_edit(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
    rules: &mdschema::fmt220::Header,
) -> Result<BudgetedVec<Edit>> {
    let nodes = scan(bytes, path, o, work, budget)?;
    let root = element(&nodes, 0, path)?;
    if bytes.get(root.start..root.open_end) != Some(rules.before.as_bytes()) {
        return Err(err(path, "unqualified 2.20 part header"));
    }
    let mut edits = vector(budget, path)?;
    edits
        .try_push(Edit {
            start: root.start,
            end: root.open_end,
            text: owned_text(budget, path, &[&rules.after])?,
        })
        .map_err(|e| err(path, e.to_string()))?;
    Ok(edits)
}
fn nonvisual_form(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Edit>> {
    const SECTIONS: &[&str] = &[
        "AutoCommandBar",
        "Attributes",
        "Commands",
        "Parameters",
        "Events",
        "AutoTitle",
        "Title",
        "SaveWindowSettings",
        "UseForFoldersAndItems",
        "CommandSet",
        "MobileDeviceCommandBarContent",
        "Customizable",
        "SaveDataInSettings",
        "AutoSaveDataInSettings",
        "Width",
    ];
    let nodes = scan(bytes, path, o, work, budget)?;
    let root = element(&nodes, 0, path)?;
    let rules = &mdschema::fmt220::rules()?.forms_nonvisual;
    if bytes.get(root.start..root.open_end) != Some(rules.before.as_bytes()) {
        return Err(err(path, "unqualified 2.20 part header"));
    }
    let mut seen = 0u16;
    for id in root.children.iter() {
        let node = element(&nodes, *id, path)?;
        if !node.form_ns {
            return Err(err(path, "unqualified 2.20 form namespace"));
        }
        let local = node.name.rsplit(':').next().unwrap_or_default();
        let section = SECTIONS
            .iter()
            .position(|name| *name == local)
            .ok_or_else(|| err(path, "unqualified 2.20 nonvisual form section"))?;
        let flag = 1u16 << section;
        if seen & flag != 0 {
            return Err(err(path, "duplicate 2.20 form section"));
        }
        seen |= flag;
        if local == "AutoCommandBar" && !node.children.is_empty() {
            return Err(err(path, "unqualified 2.20 nonvisual command bar"));
        }
    }
    if seen & 3 != 3 {
        return Err(err(path, "missing 2.20 nonvisual form section"));
    }
    let mut edits = vector(budget, path)?;
    edits
        .try_push(Edit {
            start: root.start,
            end: root.open_end,
            text: owned_text(budget, path, &[&rules.after])?,
        })
        .map_err(|e| err(path, e.to_string()))?;
    Ok(edits)
}

fn qualified_form<'a>(
    cfg: &'a crate::Configuration,
    path: &Path,
    part: md_model::xml_origin::XmlPart,
) -> Option<&'a str> {
    if part != md_model::xml_origin::XmlPart::Forms {
        return None;
    }
    let mut components = path.iter();
    let folder = components.next()?.to_str()?;
    let name = components.next()?.to_str()?;
    let object = cfg
        .objects
        .iter()
        .find(|o| o.kind.folder() == Some(folder) && o.name == name)?;
    let id = match components.next()?.to_str()? {
        "Ext" if object.kind == crate::MdKind::CommonForm => object.id,
        "Forms" => {
            let form_name = components.next()?.to_str()?;
            let form = object.forms.iter().find(|f| f.name == form_name)?;
            if components.next()? != "Ext" {
                return None;
            }
            form.id
        }
        _ => return None,
    };
    if components.next()? != "Form.xml" || components.next().is_some() {
        return None;
    }
    cfg.index.names.get(&id).map(String::as_str)
}

fn rights(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Edit>> {
    header_edit(
        bytes,
        path,
        o,
        work,
        budget,
        &mdschema::fmt220::rules()?.rights,
    )
    .map_err(|e| match e {
        Error::Xml { path, what } if what == "unqualified 2.20 part header" => {
            err(&path, "unqualified 2.20 rights header")
        }
        other => other,
    })
}
fn dump_info_header(
    bytes: &[u8],
    path: &Path,
    o: &PartsOptions,
    work: &mut u64,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Edit>> {
    let nodes = scan(bytes, path, o, work, budget)?;
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
    let (prefix, suffix) = opening
        .split_once(" version=\"2.20\"")
        .ok_or_else(|| err(path, "ConfigDumpInfo version span"))?;
    let mut edits = vector(budget, path)?;
    edits
        .try_push(Edit {
            start: root.start,
            end: root.open_end,
            text: owned_text(budget, path, &[prefix, " version=\"2.21\"", suffix])?,
        })
        .map_err(|e| err(path, e.to_string()))?;
    Ok(edits)
}

#[derive(Clone, Copy)]
enum MiscKind {
    Schedule,
    Content,
}
fn qualified_misc(
    cfg: &crate::Configuration,
    path: &Path,
    part: md_model::xml_origin::XmlPart,
) -> Option<MiscKind> {
    if part != md_model::xml_origin::XmlPart::Misc {
        return None;
    }
    let ext = path.parent()?;
    if ext.file_name()? != "Ext" {
        return None;
    }
    let owner = ext.parent()?;
    let object = cfg.objects.iter().find(|object| {
        object
            .kind
            .folder()
            .is_some_and(|folder| owner == Path::new(folder).join(&object.name))
    })?;
    match path.file_name()?.to_str()? {
        "Schedule.xml" if object.parts.schedule.is_some() => Some(MiscKind::Schedule),
        "Content.xml" if object.parts.exchange_content.is_some() => Some(MiscKind::Content),
        _ => None,
    }
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
                | md_model::xml_origin::XmlPart::Rights
        ) && qualified_misc(&dump.configuration, path, file.part()).is_none()
            && qualified_form(&dump.configuration, path, file.part()).is_none()
        {
            return Err(err(
                path,
                "unqualified 2.20 part requires exact translation rules",
            ));
        }
        if file.bytes().len() as u64 > o.max_retained_file_bytes {
            return Err(err(path, "retained XML byte limit"));
        }
    }
    let budget = MemoryBudget::new(Limits::new(o.objects.shells.max_xml_memory));
    let mut plans = vector(&budget, Path::new(""))?;
    let mut admitted_forms = vector(&budget, Path::new(""))?;
    let mut total = 0u64;
    let mut work = 0;
    for (path, file) in origin.files() {
        let edits = if path == Path::new("ConfigDumpInfo.xml") {
            dump_info_header(file.bytes(), path, o, &mut work, &budget)?
        } else if file.part() == md_model::xml_origin::XmlPart::Rights {
            rights(file.bytes(), path, o, &mut work, &budget)?
        } else if let Some(kind) = qualified_misc(&dump.configuration, path, file.part()) {
            let rules = mdschema::fmt220::rules()?;
            let header = match kind {
                MiscKind::Schedule => &rules.schedule,
                MiscKind::Content => &rules.exchange_content,
            };
            header_edit(file.bytes(), path, o, &mut work, &budget, header)?
        } else if let Some(name) = qualified_form(&dump.configuration, path, file.part()) {
            let edits = nonvisual_form(file.bytes(), path, o, &mut work, &budget)?;
            admitted_forms
                .try_push(name)
                .map_err(|e| err(path, e.to_string()))?;
            edits
        } else if path.extension().is_some_and(|e| e == "xml") {
            metadata(file.bytes(), path, o, &mut work, &budget)?
        } else {
            vector(&budget, path)?
        };
        let mut size = file.bytes().len() as u64;
        let mut end = 0;
        for edit in edits.iter() {
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
        plans
            .try_push((path, file, edits))
            .map_err(|e| err(path, e.to_string()))?;
    }
    let temp = tempfile::tempdir().map_err(|e| err(Path::new(""), e.to_string()))?;
    for (path, file, edits) in plans.iter() {
        let destination = temp.path().join(path);
        let parent = destination
            .parent()
            .ok_or_else(|| err(path, "translation parent"))?;
        std::fs::create_dir_all(parent).map_err(|e| err(path, e.to_string()))?;
        let mut out = std::fs::File::create(&destination).map_err(|e| err(path, e.to_string()))?;
        let mut start = 0;
        for edit in edits.iter() {
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
    drop(plans);
    admitted_forms.sort_unstable();
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
        let parts = present_part_names(&translated.configuration, &configuration_name);
        for (id, _) in &versions.versions {
            if id.contains('.')
                && names.element(id).ok().is_none_or(|name| {
                    !parts.contains(&name)
                        && !name
                            .strip_suffix(".Form")
                            .is_some_and(|owner| admitted_forms.binary_search(&owner).is_ok())
                })
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

fn present_part_names(cfg: &crate::Configuration, configuration_name: &str) -> BTreeSet<String> {
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
                if object.parts.schedule.is_some() {
                    names.insert(format!("{full}.Schedule"));
                }
                if object.parts.exchange_content.is_some() {
                    names.insert(format!("{full}.Content"));
                }
                if object.parts.rights.is_some() {
                    names.insert(format!("{full}.Rights"));
                }
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
