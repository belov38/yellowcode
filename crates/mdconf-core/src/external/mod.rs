pub use md_model::external::*;
pub mod build;
pub mod xml_in;
pub use build::{ExternalBuildOptions, PreparedExternal, build, prepare, write};
pub use xml_in::{LoadedXmlExternal, XmlLoadOptions, load_xml};
mod copy_info;
mod depth;
mod reader;
mod reference_uses;
mod type_uses;
pub(crate) use reference_uses::reference_use_bytes;
#[doc(hidden)]
pub use type_uses::{TypeUse, type_use_bytes};
pub mod schema;
pub mod xml_out;
use crate::entries::Entries;
use crate::{EntryData, Error, Result};
use brace::{Guid, Node};
pub use reader::{ContentCodecs, LoadedExternal, load_with};
use std::sync::Arc;

pub(super) fn fail(entry: &str, what: &str) -> Error {
    Error::Format {
        entry: entry.into(),
        what: what.into(),
    }
}
fn uint(node: &Node) -> Option<usize> {
    node.as_atom()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}
fn required(entries: &Entries<'_>, name: &str) -> Result<Arc<EntryData>> {
    entries
        .get(name)?
        .ok_or_else(|| Error::MissingEntry(name.into()))
}
fn document(entries: &Entries<'_>, name: &str) -> Result<Arc<EntryData>> {
    let data = required(entries, name)?;
    tree(&data, name)?;
    Ok(data)
}
fn tree<'a>(data: &'a EntryData, name: &str) -> Result<&'a Node> {
    match data {
        EntryData::Brace(doc) => Ok(&doc.root),
        _ => Err(fail(name, "expected brace document")),
    }
}
fn required_guid(node: &Node, path: &[usize], entry: &str) -> Result<Guid> {
    node.get(path)
        .and_then(Node::as_guid)
        .ok_or_else(|| fail(entry, "required generated-type GUID"))
}
struct Header<'a> {
    id: Guid,
    name: &'a str,
    source: Option<SourceHeader>,
}
fn read_header<'a>(node: &'a Node, entry: &str) -> Result<Header<'a>> {
    let Some(
        [
            version,
            identity,
            name,
            synonym,
            comment,
            belonging,
            count,
            target,
            trailer,
            tail @ ..,
        ],
    ) = node.as_list()
    else {
        return Err(fail(entry, "external header envelope"));
    };
    let source = match (version.as_atom(), tail) {
        (Some("3"), []) => None,
        (Some("4"), [zero, three])
            if zero.as_atom() == Some("0") && three.as_atom() == Some("3") =>
        {
            Some([0, 3])
        }
        _ => return Err(fail(entry, "unsupported external header version/tail")),
    };
    let Some([one, zero, id]) = identity.as_list() else {
        return Err(fail(entry, "external header identity"));
    };
    if one.as_atom() != Some("1") || zero.as_atom() != Some("0") {
        return Err(fail(entry, "external header identity"));
    }
    let id = id
        .as_guid()
        .ok_or_else(|| fail(entry, "external header identity"))?;
    let name = name
        .as_str()
        .ok_or_else(|| fail(entry, "quoted external name"))?;
    if comment.as_str().is_none() {
        return Err(fail(entry, "quoted external comment"));
    }
    if belonging.as_atom() != Some("0")
        || count.as_atom() != Some("0")
        || target.as_guid() != Some(Guid::default())
        || trailer.as_atom() != Some("0")
    {
        return Err(fail(entry, "unsupported external metadata state"));
    }
    let Some([languages, values @ ..]) = synonym.as_list() else {
        return Err(fail(entry, "external synonym"));
    };
    if uint(languages).and_then(|count| count.checked_mul(2)) != Some(values.len())
        || values.iter().any(|value| value.as_str().is_none())
    {
        return Err(fail(entry, "external synonym"));
    }
    Ok(Header {
        id,
        name,
        source: source.map(|extra_flags| SourceHeader {
            id,
            version: 4,
            extra_flags,
            metadata: None,
        }),
    })
}
fn metadata_shape(
    node: &Node,
    path: &[usize],
    kind: crate::SubKind,
    entry: &str,
) -> Result<(bool, Option<SourceMetadata>)> {
    let tag = match kind {
        crate::SubKind::Form => "Form",
        crate::SubKind::Template => "Template",
    };
    let envelope_error = || fail(entry, &format!("unsupported {tag} outer envelope"));
    let Some([one, outer_body, trailer]) = node.as_list() else {
        return Err(envelope_error());
    };
    if one.as_atom() != Some("1") || trailer.as_atom() != Some("0") {
        return Err(envelope_error());
    }
    match kind {
        crate::SubKind::Form => {
            let Some([one, inner]) = outer_body.as_list() else {
                return Err(envelope_error());
            };
            let Some([zero, _, tail]) = inner.as_list() else {
                return Err(envelope_error());
            };
            if path != [1, 1, 1, 1]
                || one.as_atom() != Some("1")
                || zero.as_atom() != Some("0")
                || tail
                    .as_list()
                    .is_none_or(|v| v.len() != 1 || v.first().and_then(Node::as_atom) != Some("0"))
            {
                return Err(envelope_error());
            }
        }
        crate::SubKind::Template => {
            if path != [1, 2] || outer_body.as_list().is_none_or(|body| body.len() < 3) {
                return Err(envelope_error());
            }
        }
    }
    let malformed = || fail(entry, &format!("unsupported {tag} metadata version/shape"));
    let parent = path.split_last().ok_or_else(malformed)?.1;
    let body = node
        .get(parent)
        .and_then(Node::as_list)
        .ok_or_else(malformed)?;
    let version = body.first().and_then(uint).ok_or_else(malformed)?;
    match (kind, version, body.len()) {
        (crate::SubKind::Form, 13, 5) => Ok((true, None)),
        (crate::SubKind::Form, 14, 6) | (crate::SubKind::Template, 2, 3) => Ok((false, None)),
        (crate::SubKind::Form, 15, 7) | (crate::SubKind::Template, 3, 4)
            if body.last().and_then(Node::as_atom) == Some("1") =>
        {
            Ok((
                false,
                Some(SourceMetadata {
                    kind,
                    version: version as u32,
                    extra_flag: 1,
                }),
            ))
        }
        _ => Err(malformed()),
    }
}

fn normalize_form_defaults(
    entries: &Entries<'_>,
    index: &crate::index::Index,
    object: &mut crate::MdObject,
    diagnostics: &mut crate::Diagnostics,
) -> Result<()> {
    const KEY: &str = "fail:FormProperties.UseInInterfaceCompatibilityMode";
    for form in &mut object.forms {
        let name = form.id.to_string();
        let data = document(entries, &name)?;
        let node = tree(&data, &name)?;
        let path = index
            .sub_hdr
            .get(&form.id)
            .ok_or_else(|| fail(&name, "missing form header path"))?;
        if metadata_shape(node, path, crate::SubKind::Form, &name)?.0 {
            form.props.push(crate::Prop {
                name: "UseInInterfaceCompatibilityMode".into(),
                codec: mdschema::Codec::Enum,
                value: crate::Value::Text("Any".into()),
            });
            let remove = if let Some(count) = diagnostics.misses.get_mut(KEY) {
                *count = count.saturating_sub(1);
                *count == 0
            } else {
                false
            };
            if remove {
                diagnostics.misses.remove(KEY);
            }
        }
    }
    Ok(())
}

fn validate_root<'a>(node: &'a Node, kind: ExternalKind, entry: &str) -> Result<Header<'a>> {
    let Some([one, body, count, groups @ ..]) = node.as_list() else {
        return Err(fail(entry, "internal object envelope"));
    };
    if one.as_atom() != Some("1") || uint(count) != Some(groups.len()) {
        return Err(fail(entry, "internal object version/collection count"));
    }
    let body = body.as_list().ok_or_else(|| fail(entry, "internal body"))?;
    let length = match kind {
        ExternalKind::DataProcessor => 7,
        ExternalKind::Report => 14,
    };
    if body.len() != length
        || body.first().and_then(uint) != Some(schema::body_version(kind) as usize)
    {
        return Err(fail(entry, "unsupported external body version/length"));
    }
    let reserved = match kind {
        ExternalKind::DataProcessor => 5,
        ExternalKind::Report => 10,
    };
    if body.get(reserved).and_then(Node::as_str) != Some("") {
        return Err(fail(entry, "unsupported reserved external property"));
    }
    for (_, pos) in schema::properties(kind) {
        if body.get(*pos).and_then(Node::as_guid).is_none() {
            return Err(fail(entry, "required property reference GUID"));
        }
    }
    let Some([marker, _]) = node.get(&[1, 3]).and_then(Node::as_list) else {
        return Err(fail(entry, "internal header envelope"));
    };
    if marker.as_atom() != Some("0") {
        return Err(fail(entry, "internal header envelope"));
    }
    let header = node
        .get(schema::HEADER)
        .ok_or_else(|| fail(entry, "internal header"))?;
    read_header(header, entry)
}
fn validate_children(
    node: &Node,
    children: &[(String, mdschema::ChildSpec)],
    subs: &[(String, String)],
    entry: &str,
    source_headers: &mut memory_budget::BudgetedVec<SourceHeader>,
    ids: &mut memory_budget::BudgetedVec<Guid>,
    meter: &dyn crate::LoadMeter,
) -> Result<()> {
    let fields = node
        .as_list()
        .ok_or_else(|| fail(entry, "child collections envelope"))?;
    let offset = if fields.first().is_some_and(Node::is_list) {
        1
    } else {
        2
    };
    let count = fields
        .get(offset)
        .ok_or_else(|| fail(entry, "child collections envelope"))?;
    let groups = fields
        .get(offset + 1..)
        .ok_or_else(|| fail(entry, "child collections envelope"))?;
    if uint(count) != Some(groups.len()) {
        return Err(fail(entry, "child collections count"));
    }
    let mut seen = std::collections::HashSet::new();
    for group in groups {
        meter.checkpoint()?;
        let Some([class, count, items @ ..]) = group.as_list() else {
            return Err(fail(entry, "collection shape"));
        };
        let class = class
            .as_atom()
            .ok_or_else(|| fail(entry, "collection class"))?;
        if uint(count) != Some(items.len()) || !seen.insert(class) {
            return Err(fail(entry, "collection count or duplicate class"));
        }
        if let Some((_, cs)) = children.iter().find(|(c, _)| c == class) {
            let path = cs
                .header
                .as_deref()
                .ok_or_else(|| fail(entry, "child header schema"))?;
            for item in items {
                meter.checkpoint()?;
                let h = read_header(
                    item.get(path).ok_or_else(|| fail(entry, "child header"))?,
                    entry,
                )?;
                if let Some(source) = h.source {
                    source_headers.try_push(source).map_err(memory_error)?;
                }
                ids.try_push(h.id).map_err(memory_error)?;
                for generated in &cs.internal_info {
                    required_guid(item, &generated.type_id, entry)?;
                    required_guid(item, &generated.value_id, entry)?;
                }
                if !cs.children.is_empty() {
                    validate_children(item, &cs.children, &[], entry, source_headers, ids, meter)?;
                }
            }
        } else if subs.iter().any(|(_, c)| c == class) {
            for item in items {
                meter.checkpoint()?;
                ids.try_push(
                    item.as_guid()
                        .ok_or_else(|| fail(entry, "subobject reference UUID"))?,
                )
                .map_err(memory_error)?;
            }
        } else {
            return Err(fail(entry, "unsupported external collection class"));
        }
    }
    Ok(())
}
fn resolve_copy_info(
    index: &mut crate::index::Index,
    info: &CopyInfo,
    schema: &mdschema::Schema,
    meter: &dyn crate::LoadMeter,
) -> Result<()> {
    for object in &info.objects {
        if let Some((_, kind)) = schema
            .classes_top()
            .find(|(id, _)| Guid::parse(id) == Some(object.class_id))
        {
            meter.checkpoint()?;
            if let std::collections::hash_map::Entry::Vacant(slot) = index.names.entry(object.id) {
                reader::admit_strings(meter, &[kind.len(), 1, object.name.len()])?;
                slot.insert(format!("{kind}.{}", object.name));
            }
        }
    }
    for ty in &info.types {
        let Some(object) = info.objects.iter().find(|o| o.id == ty.owner) else {
            continue;
        };
        let Some((_, kind)) = schema
            .classes_top()
            .find(|(id, _)| Guid::parse(id) == Some(object.class_id))
        else {
            continue;
        };
        if let Some(category) = schema.gen_types(kind).get(ty.category as usize) {
            meter.checkpoint()?;
            if let std::collections::hash_map::Entry::Vacant(slot) = index.types.entry(ty.type_id) {
                let prefix_len = schema
                    .gt_name(category)
                    .map(str::len)
                    .unwrap_or(kind.len() + category.len());
                reader::admit_strings(meter, &[prefix_len, prefix_len, 1, object.name.len()])?;
                let prefix = schema
                    .gt_name(category)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{kind}{category}"));
                slot.insert(format!("{prefix}.{}", object.name));
            }
        }
    }
    Ok(())
}

fn load_module(entries: &Entries<'_>, object: &mut crate::MdObject) -> Result<()> {
    let name = format!("{}.0", object.id);
    if entries.contains(&name) {
        let data = required(entries, &name)?;
        let text = crate::modules::separate_module_text(&data)
            .ok_or_else(|| fail(&name, "object module text"))?;
        object.modules.push(crate::Module {
            kind: crate::ModuleKind::ObjectModule,
            source: text.to_vec(),
        });
    }
    for form in &mut object.forms {
        let name = format!("{}.0", form.id);
        let data = required(entries, &name)?;
        let root = tree(&data, &name)?;
        let module = root
            .at(2)
            .and_then(Node::as_str)
            .ok_or_else(|| fail(&name, "form module string"))?;
        if !module.is_empty() {
            let mut source = b"\xef\xbb\xbf".to_vec();
            source.extend_from_slice(module.as_bytes());
            form.module = Some(crate::Module {
                kind: crate::ModuleKind::FormModule,
                source,
            });
        }
    }
    Ok(())
}
fn load_help(entries: &Entries<'_>, id: Guid, help: &mut Option<crate::parts::Help>) -> Result<()> {
    let name = format!("{id}.1");
    if entries.contains(&name) {
        let data = document(entries, &name)?;
        let root = tree(&data, &name)?;
        if root.at(0).and_then(Node::as_atom) != Some("5") {
            return Err(fail(&name, "unsupported help version"));
        }
        if help_shape(root).is_none() {
            return Err(fail(&name, "malformed help shape"));
        }
        *help = Some(crate::parts::help::parse_help(root).map_err(|e| fail(&name, e))?);
    }
    Ok(())
}

fn help_shape(root: &Node) -> Option<()> {
    let fields = root.as_list()?;
    let count = fields.get(1).and_then(uint)?;
    let end = count.checked_mul(2)?.checked_add(2)?;
    let pages = fields.get(2..end)?;
    if pages.as_chunks::<2>().0.iter().any(|page| {
        page.get(1)
            .and_then(Node::as_list)
            .is_none_or(|body| body.len() != 1)
    }) {
        return None;
    }
    let rest = fields.get(end..)?;
    if rest.is_empty() {
        return Some(());
    }
    let [count, files @ ..] = rest else {
        return None;
    };
    if uint(count)?.checked_mul(3)? != files.len() {
        return None;
    }
    if files.as_chunks::<3>().0.iter().any(|file| {
        file.get(1).and_then(Node::as_atom) != Some("1")
            || file
                .get(2)
                .and_then(Node::as_list)
                .is_none_or(|body| body.len() != 1)
    }) {
        return None;
    }
    Some(())
}

fn memory_error(error: memory_budget::BudgetError) -> Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => Error::Memory(limit),
        other => Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, other)),
    }
}
