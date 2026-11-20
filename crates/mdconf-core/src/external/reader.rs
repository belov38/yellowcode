use super::*;
use crate::entries::Entries;
use crate::{
    BudgetedEntries, EntryMeta, EntrySource, Error, Inflated, LoadMeter, LoadOptions, MeterRefusal,
    Result,
};
use brace::Node;
use memory_budget::{BudgetedVec, Category, MemoryBudget, Reservation};
use std::sync::Arc;

pub struct LoadedExternal {
    document: memory_budget::Charged<ExternalDocument>,
    _meter: Arc<Meter>,
}
impl std::ops::Deref for LoadedExternal {
    type Target = ExternalDocument;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}
#[doc(hidden)]
pub trait ContentCodecs {
    fn load(
        &self,
        context: &crate::codec::LoadCtx<'_>,
        tree: &Node,
        document: &mut ExternalDocument,
    ) -> Result<()>;
    fn load_owned(
        &self,
        context: &crate::codec::LoadCtx<'_>,
        tree: &Node,
        document: memory_budget::Charged<ExternalDocument>,
    ) -> Result<memory_budget::Charged<ExternalDocument>> {
        let mut result = Ok(());
        let document = document.map(|mut document| {
            result = self.load(context, tree, &mut document);
            document
        });
        result?;
        Ok(document)
    }
}

struct Meter {
    hold: Reservation,
    upstream: Option<Arc<dyn LoadMeter>>,
}
impl LoadMeter for Meter {
    fn checkpoint(&self) -> Result<()> {
        self.upstream.as_ref().map_or(Ok(()), |m| m.checkpoint())
    }
    fn admit(&self, entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        let amount = bytes
            .checked_mul(32)
            .and_then(|v| v.checked_add(4096))
            .ok_or({
                MeterRefusal(memory_budget::MemoryLimit {
                    category: Category::Metadata,
                    requested: bytes,
                    reason: memory_budget::LimitReason::Overflow,
                })
            })?;
        self.hold.try_grow(amount).map_err(MeterRefusal)?;
        if let Some(m) = &self.upstream {
            m.admit(entry, bytes)?;
        }
        Ok(())
    }
    fn hold(&self, _entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.hold.try_grow(bytes).map_err(MeterRefusal)
    }
    fn release(&self, bytes: u64) {
        let result = self.hold.shrink(bytes);
        debug_assert!(result.is_ok());
    }
}

struct Source<'a> {
    inner: BudgetedEntries<'a>,
    meter: &'a Meter,
    budget: &'a MemoryBudget,
}
impl EntrySource for Source<'_> {
    fn list(&self) -> Result<Vec<EntryMeta>> {
        self.list_checked(&|| Ok(()))
    }
    fn list_owned_checked(&self, check: &dyn Fn() -> Result<()>) -> Result<crate::EntryList> {
        let list = self.inner.list_prepaid_checked(self.budget, check)?;
        let bytes = list.iter().try_fold(0u64, |sum, meta| {
            check()?;
            sum.checked_add(meta.name.len() as u64)
                .and_then(|n| n.checked_add(std::mem::size_of::<EntryMeta>() as u64))
                .ok_or_else(|| fail("entry index", "metadata size overflow"))
        })?;
        self.meter
            .admit("entry name lookup", bytes)
            .map_err(|MeterRefusal(limit)| Error::Memory(limit))?;
        Ok(list)
    }
    fn list_checked(&self, _check: &dyn Fn() -> Result<()>) -> Result<Vec<EntryMeta>> {
        Err(fail(
            "entry index",
            "external reader requires an owned prepaid listing",
        ))
    }
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Err(fail(
            name,
            "external reader requires a bounded inflated read",
        ))
    }
    fn stored_inflated_checked(
        &self,
        name: &str,
        limit: u64,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Inflated>> {
        let Some(data) = self.inner.stored_inflated_checked(name, limit, check)? else {
            return Err(Error::MissingEntry(name.into()));
        };
        if !data.was_deflated {
            return Err(fail(name, "invalid raw-deflate entry"));
        }
        let text = data
            .bytes
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&data.bytes);
        if text.first() == Some(&b'{') {
            super::depth::check_depth(text, name, check)?;
        }
        check()?;
        Ok(Some(data))
    }
}

pub fn load_with(
    source: &dyn EntrySource,
    options: &LoadOptions,
    budget: &MemoryBudget,
    codecs: &dyn ContentCodecs,
) -> Result<LoadedExternal> {
    if let Some(m) = &options.meter {
        m.checkpoint()?;
    }
    let meter = Arc::new(Meter {
        hold: budget
            .try_reserve(Category::Metadata, 0)
            .map_err(Error::Memory)?,
        upstream: options.meter.clone(),
    });
    let source = Source {
        inner: BudgetedEntries::new(source, budget.clone()),
        meter: &meter,
        budget,
    };
    let entries = Entries::with_meter(&source, Some(meter.clone()))?;
    let mut entry_names = std::collections::HashSet::new();
    for meta in entries.list() {
        if !entry_names.insert(meta.name.as_str()) {
            return Err(fail(&meta.name, "duplicate container entry"));
        }
    }
    let root_data = document(&entries, "root")?;
    let root = super::tree(&root_data, "root")?;
    let Some([version, wrapper, tail]) = root.as_list() else {
        return Err(fail("root", "expected {2,wrapper,}"));
    };
    if version.as_atom() != Some("2") || tail.as_atom() != Some("") {
        return Err(fail("root", "unsupported external root version or trailer"));
    }
    let wrapper_id = wrapper
        .as_guid()
        .ok_or_else(|| fail("root", "wrapper GUID"))?;
    let compat = if entries.contains("version") {
        let data = document(&entries, "version")?;
        let version_tree = super::tree(&data, "version")?;
        let supported_shape = version_tree
            .as_list()
            .filter(|v| v.len() == 1)
            .and_then(|v| v.first())
            .and_then(Node::as_list)
            .filter(|v| v.len() == 3)
            .and_then(|v| v.get(2))
            .and_then(Node::as_list)
            .is_some_and(|v| v.len() == 2 && v.get(1).and_then(Node::as_atom) == Some("0"));
        if !supported_shape {
            return Err(fail("version", "version envelope"));
        }
        let (format, compat) = crate::entries::configuration_version(&entries)?;
        mdschema::for_format(format).map_err(|error| fail("version", &error.to_string()))?;
        Some(compat)
    } else {
        None
    };
    let wrapper_name = wrapper_id.to_string();
    let wrapper_data = document(&entries, &wrapper_name)?;
    let wrapper = super::tree(&wrapper_data, &wrapper_name)?;
    let Some([one, identity, count, class_body]) = wrapper.as_list() else {
        return Err(fail(&wrapper_name, "wrapper envelope"));
    };
    if one.as_atom() != Some("1")
        || count.as_atom() != Some("1")
        || identity
            .as_list()
            .is_none_or(|v| v.len() != 1 || v.first().and_then(Node::as_guid) != Some(wrapper_id))
    {
        return Err(fail(&wrapper_name, "wrapper version/count/identity"));
    }
    let Some([class, tree]) = class_body.as_list() else {
        return Err(fail(&wrapper_name, "class envelope"));
    };
    let kind = [ExternalKind::DataProcessor, ExternalKind::Report]
        .into_iter()
        .find(|k| Some(k.class_id()) == class.as_guid())
        .ok_or_else(|| fail(&wrapper_name, "unsupported external class"))?;
    let header = validate_root(tree, kind, &wrapper_name)?;
    let id = header.id;
    let name = header.name;
    let schema = prepare_schema(options)?;
    let base = crate::objects::overlay::kind(schema, kind.base_kind().as_str())
        .ok_or_else(|| fail(&wrapper_name, "missing base schema"))?;
    let mut source_headers =
        memory_budget::BudgetedVec::new(budget, Category::Metadata).map_err(memory_error)?;
    if let Some(source) = header.source {
        source_headers.try_push(source).map_err(memory_error)?;
    }
    let mut ids = BudgetedVec::new(budget, Category::Metadata).map_err(memory_error)?;
    ids.try_push(id).map_err(memory_error)?;
    if wrapper_id != id {
        ids.try_push(wrapper_id).map_err(memory_error)?;
    }
    validate_children(
        tree,
        &base.children,
        &base.subobjects,
        &wrapper_name,
        &mut source_headers,
        &mut ids,
        &*meter,
    )?;
    meter.checkpoint()?;
    ids.as_mut_slice().sort_unstable();
    for pair in ids.windows(2) {
        meter.checkpoint()?;
        if matches!(pair, [first, second] if first == second) {
            return Err(fail(&wrapper_name, "duplicate document UUID"));
        }
    }
    drop(ids);
    let full_len = kind
        .xml_name()
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_add(name.len()))
        .ok_or_else(|| fail(&wrapper_name, "index name length overflow"))?;
    admit_strings(
        &*meter,
        &[
            full_len,
            full_len,
            full_len,
            name.len(),
            kind.xml_name().len(),
            7,
            name.len(),
        ],
    )?;
    admit_child_names(&*meter, tree, &base.children, full_len, kind, name.len())?;
    let mut index = crate::index::Index::default();
    let full = format!("{}.{name}", kind.xml_name());
    index.names.insert(id, full.clone());
    index.names.insert(wrapper_id, full.clone());
    index.order.push(id);
    index.objs.insert(id, (kind.base_kind(), Some(name.into())));
    let type_id = required_guid(tree, schema::TYPE_ID, &wrapper_name)?;
    let value_id = required_guid(tree, schema::VALUE_ID, &wrapper_name)?;
    index
        .types
        .insert(type_id, format!("{}Object.{name}", kind.xml_name()));
    crate::index::index_children(
        &mut index,
        tree,
        &base.children,
        &full,
        (kind.base_kind(), name, id),
    );
    for (tag, class) in &base.subobjects {
        let sk = match tag.as_str() {
            "Form" => crate::SubKind::Form,
            "Template" => crate::SubKind::Template,
            _ => continue,
        };
        for item in crate::objects::path::find_collection(tree, class).unwrap_or_default() {
            let sub_id = item
                .as_guid()
                .ok_or_else(|| fail(&wrapper_name, "subobject UUID"))?;
            meter.checkpoint()?;
            let entry = sub_id.to_string();
            let sub_data = document(&entries, &entry)?;
            let sub = super::tree(&sub_data, &entry)?;
            let path = match sk {
                crate::SubKind::Form => vec![1, 1, 1, 1],
                crate::SubKind::Template => vec![1, 2],
            };
            let retained_metadata = metadata_shape(sub, &path, sk, &entry)?.1;
            let sub_header = read_header(
                sub.get(&path).ok_or_else(|| fail(&entry, "header path"))?,
                &entry,
            )?;
            if sub_header.id != sub_id {
                return Err(fail(&entry, "subobject header identity"));
            }
            admit_strings(&*meter, &[full_len, 1, tag.len(), 1, sub_header.name.len()])?;
            if let Some(mut source) = sub_header.source {
                source.metadata = retained_metadata;
                source_headers.try_push(source).map_err(memory_error)?;
            } else if retained_metadata.is_some() {
                return Err(fail(
                    &entry,
                    "noncanonical metadata requires a version-4 header",
                ));
            }
            index
                .names
                .insert(sub_id, format!("{full}.{tag}.{}", sub_header.name));
            index.sub_hdr.insert(sub_id, path);
            index.subs.entry(id).or_default().push((sk, sub_id));
            if sk == crate::SubKind::Form {
                required(&entries, &format!("{sub_id}.0"))?;
            }
        }
    }
    let copy_info = super::copy_info::read(&entries)?;
    resolve_copy_info(&mut index, &copy_info, schema, &*meter)?;
    let mut object = crate::load::shell_with_subs(&index, kind.base_kind(), id, name);
    object.generated_types.push(crate::GeneratedType {
        category: "Object".into(),
        type_id,
        value_id,
    });
    let mut diagnostics = crate::Diagnostics::default();
    let predef = Default::default();
    let ctx = crate::objects::codecs::Ctx {
        schema,
        index: &index,
        predef: &predef,
    };
    crate::objects::decode::external_object(
        &ctx,
        &entries,
        kind,
        tree,
        &mut object,
        &mut diagnostics,
        &|bytes| {
            meter.checkpoint()?;
            meter
                .hold("resolved metadata uses", bytes)
                .map_err(|MeterRefusal(limit)| Error::Memory(limit))
        },
    )?;
    normalize_form_defaults(&entries, &index, &mut object, &mut diagnostics)?;
    load_module(&entries, &mut object)?;
    load_help(&entries, id, &mut object.parts.help)?;
    for form in &object.forms {
        let mut help = None;
        load_help(&entries, form.id, &mut help)?;
        if let Some(help) = help {
            object.parts.form_help.push((form.id, help));
        }
    }
    let document = ExternalDocument {
        kind,
        wrapper_id,
        object,
        compat,
        copy_info,
        diagnostics,
        source_headers: Vec::new(),
        index,
    };
    let context = crate::codec::LoadCtx {
        schema,
        entries: &entries,
        opts: options,
    };
    let document = source_headers.into_charged_vec().map(|source_headers| {
        let mut document = document;
        document.source_headers = source_headers;
        document
    });
    meter.checkpoint()?;
    let document = codecs.load_owned(&context, tree, document)?;
    entries.check_refused()?;
    meter.checkpoint()?;
    drop(entries);
    drop(source);
    Ok(LoadedExternal {
        document,
        _meter: meter,
    })
}

pub(super) fn admit_strings(meter: &dyn LoadMeter, lengths: &[usize]) -> Result<()> {
    meter.checkpoint()?;
    let amount = lengths
        .iter()
        .try_fold(0u64, |n, &len| n.checked_add(len as u64))
        .and_then(|n| n.checked_mul(2))
        .ok_or(Error::Memory(memory_budget::MemoryLimit {
            category: Category::Metadata,
            requested: u64::MAX,
            reason: memory_budget::LimitReason::Overflow,
        }))?;
    meter
        .hold("expanded index strings", amount)
        .map_err(|MeterRefusal(limit)| Error::Memory(limit))
}
fn admit_child_names(
    meter: &dyn LoadMeter,
    node: &Node,
    children: &[(String, mdschema::ChildSpec)],
    prefix_len: usize,
    kind: ExternalKind,
    owner_len: usize,
) -> Result<()> {
    for (class, spec) in children {
        let Some(header) = &spec.header else { continue };
        for item in crate::objects::path::find_collection(node, class).unwrap_or_default() {
            meter.checkpoint()?;
            let name_len = item
                .get(header)
                .and_then(Node::as_list)
                .and_then(|h| h.get(2))
                .and_then(Node::as_str)
                .ok_or_else(|| fail("external child", "missing name"))?
                .len();
            let full_len = prefix_len
                .checked_add(spec.tag.len())
                .and_then(|n| n.checked_add(2))
                .and_then(|n| n.checked_add(name_len))
                .ok_or_else(|| fail("external child", "index name length overflow"))?;
            admit_strings(meter, &[full_len, full_len, name_len])?;
            for info in &spec.internal_info {
                admit_strings(
                    meter,
                    &[
                        md_xmlout::codec::objects::child_generated_kind(
                            kind.base_kind().as_str(),
                            &spec.tag,
                        )
                        .len(),
                        info.category.len(),
                        1,
                        owner_len,
                        1,
                        name_len,
                        info.category.len(),
                    ],
                )?;
            }
            admit_child_names(meter, item, &spec.children, full_len, kind, owner_len)?;
        }
    }
    Ok(())
}

fn prepare_schema(options: &LoadOptions) -> Result<&'static mdschema::Schema> {
    let resource = mdschema::embedded_resource()?;
    if let Some(meter) = &options.meter {
        meter.checkpoint()?;
        if let Some(root) = meter.schema_budget() {
            resource.prepare_with_memory(mdschema::SchemaGroup::Schema, &root)?;
            match resource
                .prepare_learned_kinds_with_memory_checked(&root, &|| meter.schema_cancelled())
            {
                Err(mdschema::ManagedSchemaError::Cancelled) => {
                    meter.checkpoint()?;
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "schema preparation cancelled",
                    )));
                }
                result => {
                    result?;
                }
            }
            resource.prepare_with_memory(mdschema::SchemaGroup::StdAttrs, &root)?;
            resource.prepare_with_memory(mdschema::SchemaGroup::Templates, &root)?;
            resource.prepare_with_memory(mdschema::SchemaGroup::Upgrade, &root)?;
            meter.checkpoint()?;
        }
    }
    Ok(resource.schema()?)
}

impl std::fmt::Debug for LoadedExternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedExternal")
            .field("document", &self.document)
            .finish_non_exhaustive()
    }
}
