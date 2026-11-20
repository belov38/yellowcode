use super::xml_in::check;
use super::{ExternalDocument, ExternalKind};
use crate::objects::{
    encode_reference::ReferenceContext, encode_scalar::encode_scalar,
    encode_type_context::TypeContext,
};
use crate::{
    Child, ChildTag, Codec, EntryMeta, EntrySink, Error, GeneratedType, LocalString, Prop, Result,
    Value,
};
use brace::{Guid, Node};
use mdschema::PropSpec;
use memory_budget::{Category, MemoryBudget, Reservation};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Debug, Clone)]
pub struct ExternalBuildOptions {
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
    pub cancelled: Option<Arc<AtomicBool>>,
}
impl Default for ExternalBuildOptions {
    fn default() -> Self {
        Self {
            max_entry_bytes: 64 << 20,
            max_total_bytes: 512 << 20,
            cancelled: None,
        }
    }
}
#[doc(hidden)]
pub trait BuildCodecs {
    fn help(
        &self,
        help: &md_model::parts::Help,
        _: &ExternalDocument,
        memory: &MemoryBudget,
        _: &ExternalBuildOptions,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        brace::write_doc_budgeted(
            &brace::Doc {
                root: help_node(help)?,
                bom: true,
                lead: String::new(),
                trailer: String::new(),
            },
            memory,
            Category::Metadata,
        )
        .map_err(memory_error)
    }
    fn form(
        &self,
        sub: &crate::SubObject,
        document: &ExternalDocument,
        memory: &MemoryBudget,
        options: &ExternalBuildOptions,
    ) -> Result<memory_budget::Charged<Vec<u8>>>;
    fn template(
        &self,
        template: &md_model::templates::Template,
        document: &ExternalDocument,
        memory: &MemoryBudget,
        options: &ExternalBuildOptions,
    ) -> Result<memory_budget::Charged<Vec<u8>>>;
}
pub struct NoBuildCodecs;
impl BuildCodecs for NoBuildCodecs {
    fn form(
        &self,
        _: &crate::SubObject,
        _: &ExternalDocument,
        _: &MemoryBudget,
        _: &ExternalBuildOptions,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        Err(bad("form", "form builder requires the facade codec"))
    }
    fn template(
        &self,
        _: &md_model::templates::Template,
        _: &ExternalDocument,
        _: &MemoryBudget,
        _: &ExternalBuildOptions,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        Err(bad(
            "template",
            "template builder requires the facade codec",
        ))
    }
}
fn bad(path: &str, what: impl Into<String>) -> Error {
    Error::Build {
        object: "external document".into(),
        path: path.into(),
        what: what.into(),
    }
}
fn memory_error(error: memory_budget::BudgetError) -> Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => Error::Memory(limit),
        other => Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, other)),
    }
}
fn parse(text: &str) -> Result<Node> {
    brace::parse(text).map_err(|e| bad("constructor", format!("{e:?}")))
}
fn put(node: &mut Node, path: &[usize], value: Node) -> Result<()> {
    *node
        .get_mut(path)
        .ok_or_else(|| bad("schema", format!("missing path {path:?}")))? = value;
    Ok(())
}
fn list(nodes: Vec<Node>) -> Node {
    Node::list(nodes)
}
fn zero() -> Node {
    Node::guid(Guid::default())
}
fn count(n: usize) -> Result<Node> {
    Ok(Node::int(
        i64::try_from(n).map_err(|_| bad("count", "count overflow"))?,
    ))
}
fn header(id: Guid, name: &str, synonym: &LocalString, comment: &str) -> Result<Node> {
    let spec = PropSpec {
        codec: Codec::LStr,
        ..Default::default()
    };
    let syn = encode_scalar(&spec, &Value::LStr(synonym.clone()), None)
        .map_err(|e| bad("Synonym", format!("{e:?}")))?;
    Ok(list(vec![
        Node::int(3),
        list(vec![Node::int(1), Node::int(0), Node::guid(id)]),
        Node::str(name),
        syn,
        Node::str(comment),
        Node::int(0),
        Node::int(0),
        zero(),
        Node::int(0),
    ]))
}

#[doc(hidden)]
pub fn can_restore_reference(document: &ExternalDocument, id: Guid) -> Result<bool> {
    let Some(name) = document.name(id) else {
        return Ok(true);
    };
    if id.is_nil() {
        return Ok(false);
    }
    fn child(children: &[Child], id: Guid) -> bool {
        children
            .iter()
            .any(|c| c.id == id || child(&c.children, id))
    }
    let object = &document.object;
    if id == object.id
        || child(&object.children, id)
        || object
            .forms
            .iter()
            .chain(&object.templates)
            .any(|s| s.id == id)
    {
        return Ok(true);
    }
    let schema = mdschema::schema()?;
    if let Some(copied) = document.copy_info.objects.iter().find(|o| o.id == id) {
        return Ok(schema.classes_top().any(|(class, kind)| {
            Guid::parse(class) == Some(copied.class_id)
                && name.strip_prefix(kind).and_then(|n| n.strip_prefix('.'))
                    == Some(copied.name.as_str())
        }));
    }
    let Some((kind, local)) = name.split_once('.') else {
        return Ok(false);
    };
    if local.contains('.') {
        return Ok(false);
    }
    let Some(class) = schema.kind(kind).and_then(|k| k.class.as_deref()) else {
        return Ok(false);
    };
    Ok(schema
        .classes_top()
        .any(|(candidate, k)| candidate == class && k == kind))
}
fn require_reference(document: &ExternalDocument, id: Guid, path: &str) -> Result<()> {
    if can_restore_reference(document, id)? {
        return Ok(());
    }
    Err(bad(
        path,
        format!("copyinfo cannot restore metadata reference {id}"),
    ))
}
fn typed_references(
    document: &ExternalDocument,
    value: &md_model::TypedValue,
    depth: usize,
) -> Result<()> {
    if depth > 128 {
        return Err(bad("typed reference", "value depth limit"));
    }
    match value {
        md_model::TypedValue::DesignTimeRef(name) => {
            for (id, _) in document.names().filter(|(_, n)| *n == name) {
                require_reference(document, id, "typed reference")?;
            }
        }
        md_model::TypedValue::FixedArray(values) => {
            for v in values {
                typed_references(document, v, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) fn root_specs(kind: ExternalKind) -> Vec<(String, PropSpec)> {
    let mut out = vec![
        (
            "Name".into(),
            PropSpec {
                codec: Codec::Str,
                path: Some(vec![1, 3, 1, 2]),
                ..Default::default()
            },
        ),
        (
            "Synonym".into(),
            PropSpec {
                codec: Codec::LStr,
                path: Some(vec![1, 3, 1, 3]),
                ..Default::default()
            },
        ),
        (
            "Comment".into(),
            PropSpec {
                codec: Codec::Str,
                path: Some(vec![1, 3, 1, 4]),
                ..Default::default()
            },
        ),
    ];
    out.extend(
        super::schema::properties(kind)
            .iter()
            .map(|(name, position)| {
                (
                    (*name).into(),
                    PropSpec {
                        codec: Codec::Ref,
                        path: Some(vec![1, *position]),
                        ..Default::default()
                    },
                )
            }),
    );
    out
}
pub(crate) fn sub_specs(tag: &str) -> Result<Vec<(String, PropSpec)>> {
    Ok(crate::objects::overlay::kind(mdschema::schema()?, tag)
        .ok_or_else(|| bad(tag, "missing subobject schema"))?
        .props
        .clone())
}

struct Encoder<'a> {
    document: &'a ExternalDocument,
    cancelled: Option<&'a AtomicBool>,
    refs: ReferenceContext,
    types: TypeContext,
}
impl Encoder<'_> {
    fn checkpoint(&self) -> Result<()> {
        if self
            .cancelled
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "external build cancelled",
            )))
        } else {
            Ok(())
        }
    }
    fn value(&self, spec: &PropSpec, p: &Prop, current: &Node) -> Result<Node> {
        let refused = |e: String| bad(&p.name, e);
        match spec.codec {
            Codec::Bool | Codec::Int | Codec::Str | Codec::Enum | Codec::LStr | Codec::Event => {
                encode_scalar(spec, &p.value, Some(current)).map_err(|e| refused(format!("{e:?}")))
            }
            Codec::Ref => {
                let node = self.refs.encode(&p.value).map_err(|e| {
                    refused(format!(
                        "unresolved metadata reference: {:?}: {e:?}",
                        p.value
                    ))
                })?;
                let id = node
                    .as_guid()
                    .ok_or_else(|| refused("invalid reference node".into()))?;
                let explicit_uuid = matches!(&p.value, Value::Ref(reference)
                    if reference.name.is_empty() || Guid::parse(&reference.name) == Some(id));
                if !explicit_uuid {
                    require_reference(self.document, id, &p.name)?;
                }
                Ok(node)
            }
            Codec::Type => self
                .types
                .encode(&p.value)
                .map_err(|e| refused(format!("unresolved type: {:?}: {e:?}", p.value))),
            Codec::TValue => {
                if let Value::Typed(value) = &p.value {
                    typed_references(self.document, value, 0)?;
                }
                crate::objects::encode_typed::encode_typed_with_index(
                    &self.document.index,
                    &p.value,
                )
                .map_err(|e| refused(format!("{e:?}")))
            }
            Codec::Purposes => crate::objects::encode_purposes::encode_purposes(&p.name, &p.value)
                .map_err(|e| refused(format!("{e:?}"))),
            Codec::CpLinks | Codec::CParams | Codec::LinkByType => match &p.value {
                Value::Xml(e)
                    if e.children.is_empty()
                        && e.attrs.is_empty()
                        && crate::xml_in::shell_xml::xml_whitespace(
                            e.text.as_deref().unwrap_or_default(),
                        ) =>
                {
                    Ok(current.clone())
                }
                _ => Err(refused(
                    "nonempty structured property requires an inverse codec".into(),
                )),
            },
            _ => Err(refused(format!(
                "unsupported property inverse {:?}",
                spec.codec
            ))),
        }
    }
    fn properties(
        &self,
        node: &mut Node,
        props: &[Prop],
        specs: &[(String, PropSpec)],
        hdr: &[usize],
        tag: &str,
    ) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        let schema = mdschema::schema()?;
        for p in props {
            self.checkpoint()?;
            if !seen.insert(&p.name) {
                return Err(bad(&p.name, "duplicate property"));
            }
            let spec = crate::xml_in::schema::descriptor(
                schema,
                specs,
                tag,
                &format!("{tag}Properties"),
                &p.name,
            )
            .ok_or_else(|| bad(&p.name, "unknown property"))?;
            let path = match p.name.as_str() {
                "Name" => [hdr, &[2]].concat(),
                "Synonym" => [hdr, &[3]].concat(),
                "Comment" => [hdr, &[4]].concat(),
                _ => spec
                    .fd
                    .as_ref()
                    .map(|fd| {
                        [
                            hdr.get(..hdr.len().saturating_sub(2)).unwrap_or_default(),
                            fd,
                        ]
                        .concat()
                    })
                    .or_else(|| crate::objects::path::prop_path(&spec, Some(hdr)))
                    .ok_or_else(|| bad(&p.name, "property has no native position"))?,
            };
            let current = node
                .get(&path)
                .ok_or_else(|| bad(&p.name, format!("invalid schema path {path:?}")))?;
            let value = self.value(&spec, p, current)?;
            put(node, &path, value)?;
        }
        Ok(())
    }
    fn child(&self, c: &Child, spec: &mdschema::ChildSpec) -> Result<Node> {
        self.checkpoint()?;
        if c.generated_types.len() != spec.internal_info.len()
            || c.generated_types.iter().any(|g| {
                !spec
                    .internal_info
                    .iter()
                    .any(|expected| expected.category == g.category)
            })
        {
            return Err(bad(
                "InternalInfo",
                format!("{}: unsupported extra GeneratedType", c.name),
            ));
        }
        let mut node = match c.tag {
            ChildTag::Attribute => parse(
                "{{0,{27,{2,{3,{1,0,00000000-0000-0000-0000-000000000000},\"\",{0},\"\",0,0,00000000-0000-0000-0000-000000000000,0},{\"Pattern\"}},0,{0},{0},0,\"\",0,{\"U\"},{\"U\"},0,00000000-0000-0000-0000-000000000000,2,0,{5006,0},{3,0,0},{0,0},0,{0},{\"S\",\"\"},0,0,0}},0}",
            )?,
            ChildTag::TabularSection => parse(
                "{{0,{11,00000000-0000-0000-0000-000000000000,00000000-0000-0000-0000-000000000000,00000000-0000-0000-0000-000000000000,00000000-0000-0000-0000-000000000000,{0,{3,{1,0,00000000-0000-0000-0000-000000000000},\"\",{0},\"\",0,0,00000000-0000-0000-0000-000000000000,0}},0,{0},{0}}},0}",
            )?,
            _ => return Err(bad(&c.name, "unsupported external child kind")),
        };
        let hdr = spec
            .header
            .as_deref()
            .ok_or_else(|| bad(&c.name, "missing child header schema"))?;
        put(
            &mut node,
            hdr,
            header(c.id, &c.name, &c.synonym, &c.comment)?,
        )?;
        self.properties(&mut node, &c.props, &spec.props, hdr, &spec.tag)?;
        for gt in &spec.internal_info {
            let actual = generated(&c.generated_types, &gt.category, &c.name)?;
            put(&mut node, &gt.type_id, Node::guid(actual.type_id))?;
            put(&mut node, &gt.value_id, Node::guid(actual.value_id))?;
        }
        let collections = self.children(&c.children, &spec.children)?;
        if !collections.is_empty() {
            let native = node
                .as_list_mut()
                .ok_or_else(|| bad(&c.name, "child constructor"))?;
            *native
                .get_mut(1)
                .ok_or_else(|| bad(&c.name, "child collection count"))? = count(collections.len())?;
            native.extend(collections);
        }
        Ok(node)
    }
    fn children(
        &self,
        children: &[Child],
        specs: &[(String, mdschema::ChildSpec)],
    ) -> Result<Vec<Node>> {
        for c in children {
            if !specs.iter().any(|(_, s)| s.tag == format!("{:?}", c.tag)) {
                return Err(bad(&c.name, "child absent from external schema"));
            }
        }
        specs
            .iter()
            .filter(|(_, s)| matches!(s.tag.as_str(), "Attribute" | "TabularSection"))
            .map(|(class, spec)| {
                let selected = children
                    .iter()
                    .filter(|c| format!("{:?}", c.tag) == spec.tag)
                    .collect::<Vec<_>>();
                let mut collection = vec![Node::atom(class), count(selected.len())?];
                for c in selected {
                    collection.push(self.child(c, spec)?);
                }
                Ok(list(collection))
            })
            .collect()
    }
}
fn generated<'a>(
    types: &'a [GeneratedType],
    category: &str,
    name: &str,
) -> Result<&'a GeneratedType> {
    let mut matches = types.iter().filter(|g| g.category == category);
    let gt = matches
        .next()
        .ok_or_else(|| bad(name, format!("missing generated {category}")))?;
    if gt.type_id.is_nil() || gt.value_id.is_nil() || matches.next().is_some() {
        return Err(bad(name, "invalid generated type identity"));
    }
    Ok(gt)
}

#[doc(hidden)]
pub struct EncodedObject {
    tree: Node,
    _charge: Reservation,
}
impl EncodedObject {
    pub fn as_tree(&self) -> &Node {
        &self.tree
    }
}
#[doc(hidden)]
pub fn encode_object_tree(d: &ExternalDocument, budget: &MemoryBudget) -> Result<EncodedObject> {
    let charge = budget
        .try_reserve(Category::Metadata, estimate(d)?)
        .map_err(Error::Memory)?;
    let encoder = Encoder {
        document: d,
        cancelled: None,
        refs: ReferenceContext::new(&d.index).map_err(|e| bad("resolution", format!("{e:?}")))?,
        types: TypeContext::new(&d.index, mdschema::schema()?)
            .map_err(|e| bad("types", format!("{e:?}")))?,
    };
    Ok(EncodedObject {
        tree: encoder.internal()?,
        _charge: charge,
    })
}
impl Encoder<'_> {
    fn internal(&self) -> Result<Node> {
        let d = self.document;
        let object = &d.object;
        let s = mdschema::schema()?;
        let gt = generated(&object.generated_types, "Object", &object.name)?;
        let mut body = vec![
            Node::int(super::schema::body_version(d.kind).into()),
            Node::guid(gt.type_id),
            Node::guid(gt.value_id),
            list(vec![
                Node::int(0),
                header(object.id, &object.name, &object.synonym, &object.comment)?,
            ]),
        ];
        let last = if d.kind == ExternalKind::Report {
            13
        } else {
            6
        };
        for position in 4..=last {
            body.push(
                if position
                    == if d.kind == ExternalKind::Report {
                        10
                    } else {
                        5
                    }
                {
                    Node::str("")
                } else {
                    zero()
                },
            );
        }
        let mut internal = list(vec![Node::int(1), list(body), Node::int(0)]);
        self.properties(
            &mut internal,
            &object.props,
            &root_specs(d.kind),
            super::schema::HEADER,
            d.kind.xml_name(),
        )?;
        let spec = crate::objects::overlay::kind(s, d.kind.base_kind().as_str())
            .ok_or_else(|| bad("schema", "missing external kind"))?;
        let mut collections = self.children(&object.children, &spec.children)?;
        for (tag, class) in &spec.subobjects {
            let subs = match tag.as_str() {
                "Form" => &object.forms,
                "Template" => &object.templates,
                _ => return Err(bad(tag, "unknown external subobject collection")),
            };
            let mut items = vec![Node::atom(class), count(subs.len())?];
            items.extend(subs.iter().map(|sub| Node::guid(sub.id)));
            collections.push(list(items));
        }
        put(&mut internal, &[2], count(collections.len())?)?;
        internal
            .as_list_mut()
            .ok_or_else(|| bad("internal", "constructor"))?
            .extend(collections);
        Ok(internal)
    }
}

pub struct PreparedExternal {
    entries: Vec<(EntryMeta, Vec<u8>)>,
    _charges: Vec<Reservation>,
    cancelled: Option<Arc<AtomicBool>>,
    budget: MemoryBudget,
}
impl PreparedExternal {
    pub fn write_to(&self, sink: &mut dyn EntrySink) -> Result<()> {
        for (meta, data) in &self.entries {
            check(&self.cancelled)?;
            sink.put(meta, data)?;
        }
        check(&self.cancelled)?;
        sink.finish()
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        check(&self.cancelled)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let layout_bytes = self.entries.iter().try_fold(4096u64, |n, (m, _)| {
            n.checked_add((m.name.len() as u64).saturating_mul(16))
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| bad("write", "container layout size overflow"))
        })?;
        let _layout_charge = self
            .budget
            .try_reserve(Category::Metadata, layout_bytes)
            .map_err(Error::Memory)?;
        let mut specs = memory_budget::BudgetedVec::try_with_capacity(
            &self.budget,
            Category::Metadata,
            self.entries.len(),
        )
        .map_err(memory_error)?;
        for (m, b) in &self.entries {
            check(&self.cancelled)?;
            specs
                .try_push(v8cont::EntrySpec {
                    name: m.name.clone(),
                    ctime: m.ctime,
                    mtime: m.mtime,
                    attrs: m.attrs,
                    data_len: Some(b.len() as u64),
                })
                .map_err(memory_error)?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        struct Data<'a>(&'a [(EntryMeta, Vec<u8>)], &'a Option<Arc<AtomicBool>>);
        impl v8cont::DataSource for Data<'_> {
            fn write_data(
                &mut self,
                index: usize,
                out: &mut dyn std::io::Write,
            ) -> v8cont::Result<()> {
                let bytes = &self
                    .0
                    .get(index)
                    .ok_or_else(|| v8cont::Error::LayoutMismatch("entry index".into()))?
                    .1;
                for chunk in bytes.chunks(64 << 10) {
                    if self
                        .1
                        .as_ref()
                        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "external write cancelled",
                        )
                        .into());
                    }
                    out.write_all(chunk)?;
                }
                Ok(())
            }
        }
        v8cont::write_container(
            tmp.as_file_mut(),
            specs.as_slice(),
            &mut Data(&self.entries, &self.cancelled),
            &v8cont::WriteOptions::top_level(v8cont::Format::F15),
        )?;
        check(&self.cancelled)?;
        tmp.persist_noclobber(path).map_err(|e| e.error)?;
        Ok(())
    }
}
struct Records<'a> {
    options: &'a ExternalBuildOptions,
    budget: &'a MemoryBudget,
    prepared: PreparedExternal,
    total: u64,
}
impl Records<'_> {
    fn bytes(&mut self, name: String, raw: &[u8]) -> Result<()> {
        check(&self.options.cancelled)?;
        if raw.len() as u64 > self.options.max_entry_bytes
            || raw.len() as u64 > self.options.max_total_bytes.saturating_sub(self.total)
        {
            return Err(bad(&name, "external output byte limit"));
        }
        self.prepared._charges.push(
            self.budget
                .try_reserve(
                    Category::Metadata,
                    (raw.len() as u64).saturating_mul(3).saturating_add(4096),
                )
                .map_err(Error::Memory)?,
        );
        let stored = zraw::deflate(raw, zraw::LEVEL_1C).map_err(|source| Error::Deflate {
            entry: name.clone(),
            source,
        })?;
        self.total += raw.len() as u64;
        self.prepared.entries.push((
            EntryMeta {
                name,
                ctime: Default::default(),
                mtime: Default::default(),
                attrs: 0,
            },
            stored,
        ));
        Ok(())
    }
    fn node(&mut self, name: String, node: Node) -> Result<()> {
        let bytes = brace::write_doc_budgeted(
            &brace::Doc {
                root: node,
                bom: true,
                lead: String::new(),
                trailer: String::new(),
            },
            self.budget,
            Category::Metadata,
        )
        .map_err(memory_error)?;
        self.bytes(name, &bytes)
    }
}
pub fn prepare(
    document: &ExternalDocument,
    options: &ExternalBuildOptions,
    budget: &MemoryBudget,
) -> Result<PreparedExternal> {
    prepare_with(document, options, budget, &NoBuildCodecs)
}
#[doc(hidden)]
pub fn prepare_with(
    d: &ExternalDocument,
    o: &ExternalBuildOptions,
    budget: &MemoryBudget,
    codecs: &dyn BuildCodecs,
) -> Result<PreparedExternal> {
    check(&o.cancelled)?;
    if d.wrapper_id.is_nil()
        || d.object.id.is_nil()
        || d.wrapper_id == d.object.id
        || d.object.kind != d.kind.base_kind()
    {
        return Err(bad("identity", "invalid wrapper/internal identity or kind"));
    }
    let reserve = budget
        .try_reserve(Category::Metadata, estimate(d)?)
        .map_err(Error::Memory)?;
    validate(d)?;
    let mut records = Records {
        options: o,
        budget,
        prepared: PreparedExternal {
            entries: Vec::new(),
            _charges: vec![reserve],
            cancelled: o.cancelled.clone(),
            budget: budget.clone(),
        },
        total: 0,
    };
    let s = mdschema::schema()?;
    let encoder = Encoder {
        document: d,
        cancelled: o.cancelled.as_deref(),
        refs: ReferenceContext::new(&d.index).map_err(|e| bad("resolution", format!("{e:?}")))?,
        types: TypeContext::new(&d.index, s).map_err(|e| bad("types", format!("{e:?}")))?,
    };
    let object = &d.object;
    let internal = encoder.internal()?;
    let wrapper = list(vec![
        Node::int(1),
        list(vec![Node::guid(d.wrapper_id)]),
        Node::int(1),
        list(vec![Node::guid(d.kind.class_id()), internal]),
    ]);
    records.node(
        "root".into(),
        list(vec![Node::int(2), Node::guid(d.wrapper_id), Node::empty()]),
    )?;
    records.node(d.wrapper_id.to_string(), wrapper)?;
    for module in &object.modules {
        if module.kind != crate::ModuleKind::ObjectModule {
            return Err(bad("module", "unsupported external module slot"));
        }
        let entries = [
            v8cont::NewEntry {
                name: "info".into(),
                ctime: Default::default(),
                mtime: Default::default(),
                attrs: 0,
                data: Some(b"\xef\xbb\xbf{3,1,0,\"\",0}".to_vec()),
            },
            v8cont::NewEntry {
                name: "text".into(),
                ctime: Default::default(),
                mtime: Default::default(),
                attrs: 0,
                data: Some(module.source.clone()),
            },
        ];
        let nested = v8cont::build_container(&entries, &v8cont::WriteOptions::nested())?;
        records.bytes(format!("{}.0", object.id), &nested)?;
    }
    if let Some(help) = &object.parts.help {
        records.bytes(
            format!("{}.1", object.id),
            &codecs.help(help, d, budget, o)?,
        )?;
    }
    for sub in object.forms.iter().chain(&object.templates) {
        let form = sub.kind == crate::SubKind::Form;
        let tag = if form { "Form" } else { "Template" };
        let mut native = if form {
            parse(
                "{1,{1,{0,{14,{3,{1,0,00000000-0000-0000-0000-000000000000},\"\",{0},\"\",0,0,00000000-0000-0000-0000-000000000000,0},0,1,{0},0},{0}}},0}",
            )?
        } else {
            parse(
                "{1,{2,0,{3,{1,0,00000000-0000-0000-0000-000000000000},\"\",{0},\"\",0,0,00000000-0000-0000-0000-000000000000,0}},0}",
            )?
        };
        let hdr = if form { &[1, 1, 1, 1][..] } else { &[1, 2][..] };
        put(
            &mut native,
            hdr,
            header(sub.id, &sub.name, &LocalString::default(), "")?,
        )?;
        encoder.properties(&mut native, &sub.props, &sub_specs(tag)?, hdr, tag)?;
        records.node(sub.id.to_string(), native)?;
        if !form
            && sub.template.as_ref().is_some_and(|t| {
                t.kind == Some(md_model::templates::TemplateType::SpreadsheetDocument)
                    && t.files.is_empty()
            })
        {
            continue;
        }
        let payload = if form {
            codecs.form(sub, d, budget, o)?
        } else {
            codecs.template(
                sub.template
                    .as_ref()
                    .ok_or_else(|| bad(&sub.name, "missing template content"))?,
                d,
                budget,
                o,
            )?
        };
        records.bytes(format!("{}.0", sub.id), &payload)?;
        if form && let Some((_, help)) = object.parts.form_help.iter().find(|(id, _)| *id == sub.id)
        {
            records.bytes(format!("{}.1", sub.id), &codecs.help(help, d, budget, o)?)?;
        }
    }
    records.node("copyinfo".into(), copy_info(d)?)?;
    records.node(
        "version".into(),
        list(vec![list(vec![
            Node::int(216),
            Node::int(0),
            list(vec![
                Node::int(d.compat.unwrap_or(80501).into()),
                Node::int(0),
            ]),
        ])]),
    )?;
    let mut versions = vec![
        Node::int(1),
        count(records.prepared.entries.len() + 1)?,
        Node::str(""),
        Node::guid(d.wrapper_id),
    ];
    for (meta, data) in &records.prepared.entries {
        versions.push(Node::str(&meta.name));
        versions.push(Node::guid(content_id(data)));
    }
    records.node("versions".into(), list(versions))?;
    Ok(records.prepared)
}
fn copy_info(d: &ExternalDocument) -> Result<Node> {
    let mut info = d.copy_info.clone();
    let schema = mdschema::schema()?;
    let spec = crate::objects::overlay::kind(schema, d.kind.base_kind().as_str())
        .ok_or_else(|| bad("copyinfo", "missing external schema"))?;
    let mut own = vec![(d.object.id, d.kind.class_id(), d.object.name.as_str())];
    for (tag, class) in &spec.subobjects {
        let class = Guid::parse(class).ok_or_else(|| bad("copyinfo", "invalid class table"))?;
        let subs = if tag == "Form" {
            &d.object.forms
        } else {
            &d.object.templates
        };
        for sub in subs {
            own.push((sub.id, class, sub.name.as_str()));
        }
    }
    for (id, class_id, name) in own {
        if !info.objects.iter().any(|o| o.id == id) {
            info.objects.push(super::CopyObject {
                id,
                source_id: id,
                class_id,
                name: name.into(),
            });
        }
    }
    for (id, full) in d.names() {
        if info.objects.iter().any(|o| o.id == id) {
            continue;
        }
        let Some((kind, name)) = full.split_once('.') else {
            continue;
        };
        if name.contains('.') {
            continue;
        }
        if let Some(class_id) = schema
            .kind(kind)
            .and_then(|s| s.class.as_deref())
            .and_then(Guid::parse)
        {
            info.objects.push(super::CopyObject {
                id,
                source_id: id,
                class_id,
                name: name.into(),
            });
        }
    }
    let gt = generated(&d.object.generated_types, "Object", &d.object.name)?;
    if !info.types.iter().any(|t| t.type_id == gt.type_id) {
        info.types.push(super::CopyType {
            type_id: gt.type_id,
            owner: d.object.id,
            category: 0,
        });
    }
    let mut own_types = std::collections::HashSet::new();
    own_types.extend(d.object.generated_types.iter().map(|g| g.type_id));
    fn own_child_types(children: &[Child], types: &mut std::collections::HashSet<Guid>) {
        for child in children {
            types.extend(child.generated_types.iter().map(|g| g.type_id));
            own_child_types(&child.children, types);
        }
    }
    own_child_types(&d.object.children, &mut own_types);
    for id in d.index.types.keys() {
        if info.types.iter().any(|t| t.type_id == *id) {
            continue;
        }
        if own_types.contains(id) {
            continue;
        }
        let owner = d
            .index
            .type_owners
            .get(id)
            .ok_or_else(|| bad("copyinfo", format!("missing type owner for {id}")))?;
        let (kind, _) = d
            .index
            .objs
            .get(&owner.object)
            .ok_or_else(|| bad("copyinfo", format!("missing object class for type {id}")))?;
        if !info.objects.iter().any(|o| o.id == owner.object) {
            return Err(bad(
                "copyinfo",
                format!("missing type owner object for {id}"),
            ));
        }
        let category = schema
            .gen_types(kind.as_str())
            .iter()
            .position(|c| c == &owner.category)
            .ok_or_else(|| {
                bad(
                    "copyinfo",
                    format!("unsupported type owner category for {id}"),
                )
            })?;
        info.types.push(super::CopyType {
            type_id: *id,
            owner: owner.object,
            category: u32::try_from(category).map_err(|_| bad("copyinfo", "category overflow"))?,
        });
    }
    for ty in &info.types {
        if own_types.contains(&ty.type_id) {
            continue;
        }
        let Some(full_name) = d.index.types.get(&ty.type_id) else {
            continue;
        };
        let object = info
            .objects
            .iter()
            .find(|object| object.id == ty.owner)
            .ok_or_else(|| bad("copyinfo", "missing type owner object"))?;
        let kind = schema
            .classes_top()
            .find(|(id, _)| Guid::parse(id) == Some(object.class_id))
            .map(|(_, kind)| kind)
            .ok_or_else(|| bad("copyinfo", "unrestorable type owner class"))?;
        let category = schema
            .gen_types(kind)
            .get(ty.category as usize)
            .ok_or_else(|| bad("copyinfo", "unrestorable type category"))?;
        let (prefix, name) = full_name
            .split_once('.')
            .ok_or_else(|| bad("copyinfo", "unrestorable type name"))?;
        let prefix_matches = match schema.gt_name(category) {
            Some(expected) => prefix == expected,
            None => prefix
                .strip_prefix(kind)
                .is_some_and(|suffix| suffix == category),
        };
        if !prefix_matches || name != object.name {
            return Err(bad("copyinfo", "copyinfo cannot restore exact type name"));
        }
        if let Some(owner) = d.index.type_owners.get(&ty.type_id)
            && (owner.object != ty.owner
                || owner.category != *category
                || owner.child.is_some()
                || owner.recalc.is_some()
                || owner.external.is_some())
        {
            return Err(bad("copyinfo", "copyinfo cannot restore nested type owner"));
        }
    }
    let mut objects = vec![count(info.objects.len())?];
    for o in &info.objects {
        objects.push(list(vec![
            Node::guid(o.id),
            Node::guid(o.source_id),
            Node::int(1),
            list(vec![Node::guid(o.class_id), Node::str(&o.name)]),
        ]));
    }
    let mut types = vec![count(info.types.len())?];
    for t in &info.types {
        types.push(list(vec![
            Node::guid(t.type_id),
            Node::guid(t.owner),
            Node::int(t.category.into()),
        ]));
    }
    let mut paths = vec![count(
        info.data_paths.len() + info.standard_data_paths.len(),
    )?];
    for p in &info.data_paths {
        paths.push(list(vec![
            list(vec![Node::int(0), Node::guid(p.child)]),
            Node::guid(p.owner_type),
            list(vec![Node::int(-1)]),
        ]));
    }
    for p in &info.standard_data_paths {
        paths.push(list(vec![
            list(vec![Node::int(p.index.into())]),
            Node::guid(p.owner_type),
            list(vec![Node::int(p.target_index.into())]),
        ]));
    }
    Ok(list(vec![
        Node::int(4),
        list(objects),
        list(types),
        list(paths),
        list(vec![Node::int(0), Node::int(0)]),
        list(vec![Node::int(0)]),
    ]))
}
fn content_id(bytes: &[u8]) -> Guid {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(bytes);
    let mut id = [0; 16];
    for (a, b) in id.iter_mut().zip(digest.iter()) {
        *a = *b;
    }
    Guid::from_bytes(id)
}
fn validate(d: &ExternalDocument) -> Result<()> {
    use std::collections::HashSet;
    let o = &d.object;
    if !d.diagnostics.unsupported.is_empty() {
        return Err(bad(
            "diagnostics",
            "external document contains unsupported semantic content",
        ));
    }
    if let Some((key, _)) = d.diagnostics.misses.first_key_value() {
        let reason = if key.starts_with("unresolved-type:") {
            "unresolved metadata type"
        } else if key.starts_with("unresolved:") {
            "unresolved metadata reference"
        } else {
            "semantic metadata diagnostic"
        };
        return Err(bad("diagnostics", format!("{reason}: {key}")));
    }
    if o.generated_types.len() != 1
        || o.generated_types
            .first()
            .is_none_or(|g| g.category != "Object")
    {
        return Err(bad(
            "InternalInfo",
            "external document requires exactly one Object GeneratedType",
        ));
    }
    if o.this_node.is_some()
        || !o.recalculations.is_empty()
        || !o.external.is_empty()
        || !o.subsystems.is_empty()
        || o.form.is_some()
        || o.template.is_some()
        || o.flowchart.is_some()
    {
        return Err(bad(&o.name, "unsupported external object content"));
    }
    let parts = &o.parts;
    if parts.rights.is_some()
        || parts.predefined.is_some()
        || parts.command_interface.is_some()
        || parts.picture.is_some()
        || parts.schedule.is_some()
        || parts.exchange_content.is_some()
        || parts.style.is_some()
        || !parts.binary_modules.is_empty()
        || !parts.ws_definition.is_empty()
        || parts.xdto_package.is_some()
    {
        return Err(bad(&o.name, "unsupported external object part"));
    }
    let mut ids = HashSet::from([d.wrapper_id, o.id]);
    fn identity(id: Guid, name: &str, props: &[Prop], ids: &mut HashSet<Guid>) -> Result<()> {
        crate::xml_in::walk::component(Path::new(name), name)?;
        if id.is_nil() || !ids.insert(id) {
            return Err(bad(name, "duplicate or nil metadata UUID"));
        }
        if props
            .iter()
            .any(|p| p.name == "Name" && p.value != Value::Text(name.into()))
        {
            return Err(bad(name, "Name property differs from model name"));
        }
        Ok(())
    }
    fn generated_ids(g: &[GeneratedType], ids: &mut HashSet<Guid>) -> Result<()> {
        for g in g {
            if g.type_id.is_nil()
                || g.value_id.is_nil()
                || !ids.insert(g.type_id)
                || !ids.insert(g.value_id)
            {
                return Err(bad("InternalInfo", "duplicate or nil generated UUID"));
            }
        }
        Ok(())
    }
    fn children(c: &[Child], ids: &mut HashSet<Guid>, depth: usize) -> Result<()> {
        if depth > 2 && !c.is_empty() {
            return Err(bad("children", "child depth limit"));
        }
        let mut names = HashSet::new();
        for c in c {
            if !names.insert((c.tag, c.name.to_lowercase())) {
                return Err(bad(&c.name, "duplicate child name"));
            }
            identity(c.id, &c.name, &c.props, ids)?;
            generated_ids(&c.generated_types, ids)?;
            if c.module.is_some() {
                return Err(bad(&c.name, "child module has no external slot"));
            }
            children(&c.children, ids, depth + 1)?;
        }
        Ok(())
    }
    crate::xml_in::walk::component(Path::new(&o.name), &o.name)?;
    if o.props.iter().any(|p| match p.name.as_str() {
        "Name" => p.value != Value::Text(o.name.clone()),
        "Synonym" => p.value != Value::LStr(o.synonym.clone()),
        "Comment" => p.value != Value::Text(o.comment.clone()),
        _ => false,
    }) {
        return Err(bad(&o.name, "header property differs from model field"));
    }
    generated_ids(&o.generated_types, &mut ids)?;
    children(&o.children, &mut ids, 0)?;
    for (expected, subs) in [
        (crate::SubKind::Form, &o.forms),
        (crate::SubKind::Template, &o.templates),
    ] {
        let mut names = HashSet::new();
        for sub in subs {
            identity(sub.id, &sub.name, &sub.props, &mut ids)?;
            if sub.kind != expected || !names.insert(sub.name.to_lowercase()) {
                return Err(bad(&sub.name, "subobject kind or duplicate name"));
            }
            if let Some(template) = &sub.template {
                let mut paths = HashSet::new();
                for (path, _) in &template.files {
                    super::xml_out::check_path(path)?;
                    if !paths.insert(crate::xml_in::walk::portable_fs_collision_key(path)) {
                        return Err(bad(path, "NFC/case collision in template assets"));
                    }
                }
                if sub.props.iter().any(|p| {
                    p.name == "TemplateType"
                        && template
                            .kind
                            .is_none_or(|kind| p.value != Value::Text(kind.name().into()))
                }) {
                    return Err(bad(&sub.name, "TemplateType differs from template content"));
                }
            }
        }
    }
    if o.modules.len() > 1 {
        return Err(bad("ObjectModule", "duplicate module slot"));
    }
    for m in &o.modules {
        std::str::from_utf8(&m.source).map_err(|_| bad("ObjectModule", "module is not UTF-8"))?;
    }
    for (id, _) in &o.parts.form_help {
        if !o.forms.iter().any(|s| s.id == *id) {
            return Err(bad("Help", "unknown form help owner"));
        }
    }
    Ok(())
}
pub fn help_node(help: &md_model::parts::Help) -> Result<Node> {
    use base64::Engine;
    let base64 = |b: &[u8]| {
        list(vec![Node::atom(format!(
            "#base64:{}",
            base64::engine::general_purpose::STANDARD.encode(b)
        ))])
    };
    let mut nodes = vec![Node::int(5), count(help.pages.len())?];
    for (language, html) in &help.pages {
        nodes.extend([Node::str(language), base64(html)]);
    }
    nodes.push(count(help.files.len())?);
    for (name, data) in &help.files {
        nodes.extend([Node::str(name), Node::int(1), base64(data)]);
    }
    Ok(list(nodes))
}
fn value_size(v: &Value, depth: usize) -> Result<u64> {
    if depth > 128 {
        return Err(bad("properties", "value depth limit"));
    }
    let size = match v {
        Value::Text(s) => s.len() as u64,
        Value::Bool(_) => 1,
        Value::LStr(s) => {
            s.0.iter()
                .map(|(a, b)| (a.len() + b.len() + 64) as u64)
                .sum()
        }
        Value::Ref(r) => (r.name.len() + 64) as u64,
        Value::Type(t) => t
            .types
            .iter()
            .map(|t| ((t.name.local.len() + t.name.namespace_uri().len()) + 256) as u64)
            .sum(),
        Value::Typed(v) => typed_size(v, depth + 1)?,
        Value::RefList(r) => r.iter().map(|r| (r.name.len() + 128) as u64).sum(),
        Value::FieldList(r) => r.iter().map(|r| (r.len() + 64) as u64).sum(),
        Value::StandardAttributes(a) => {
            let mut n = 0u64;
            for a in a {
                n = n.saturating_add(a.name.len() as u64 + props_size(&a.props, depth + 1)?);
            }
            n
        }
        Value::Xml(e) => xml_size(e, depth + 1)?,
    };
    Ok(size.saturating_add(256))
}
fn typed_size(v: &crate::TypedValue, depth: usize) -> Result<u64> {
    if depth > 128 {
        return Err(bad("properties", "typed value depth limit"));
    }
    Ok(match v {
        crate::TypedValue::Str(s)
        | crate::TypedValue::Number(s)
        | crate::TypedValue::Date(s)
        | crate::TypedValue::DesignTimeRef(s) => s.len() as u64,
        crate::TypedValue::FixedArray(a) => {
            let mut n = 0u64;
            for v in a {
                n = n.saturating_add(128 + typed_size(v, depth + 1)?);
            }
            n
        }
        _ => 64,
    })
}
fn xml_size(e: &crate::XmlElem, depth: usize) -> Result<u64> {
    if depth > 128 {
        return Err(bad("properties", "XML value depth limit"));
    }
    let mut n = (e.name.len() + e.text.as_ref().map_or(0, String::len) + 256) as u64;
    for (a, b) in &e.attrs {
        n = n.saturating_add((a.len() + b.len() + 128) as u64);
    }
    for c in &e.children {
        n = n.saturating_add(xml_size(c, depth + 1)?);
    }
    Ok(n)
}
fn props_size(props: &[Prop], depth: usize) -> Result<u64> {
    let mut n = 0u64;
    for p in props {
        n = n.saturating_add(p.name.len() as u64 + value_size(&p.value, depth)?);
    }
    Ok(n)
}
fn children_size(children: &[Child], depth: usize) -> Result<u64> {
    if depth > 2 && !children.is_empty() {
        return Err(bad("children", "external child depth exceeds schema"));
    }
    let mut n = 0u64;
    for c in children {
        n = n.saturating_add(
            8192 + props_size(&c.props, 0)? + children_size(&c.children, depth + 1)?,
        );
    }
    Ok(n)
}
fn estimate(d: &ExternalDocument) -> Result<u64> {
    let mut n =
        (64u64 << 10) + props_size(&d.object.props, 0)? + children_size(&d.object.children, 0)?;
    for (id, name) in d.names().chain(d.type_names()) {
        let _ = id;
        n = n.saturating_add(name.len() as u64 + 512);
    }
    for sub in d.object.forms.iter().chain(&d.object.templates) {
        n = n.saturating_add(16384 + props_size(&sub.props, 0)?);
        if let Some(t) = &sub.template {
            for (path, data) in &t.files {
                n = n.saturating_add((path.len() + data.len()) as u64);
            }
        }
        if let Some(f) = &sub.form
            && let Some(source) = &f.source
        {
            if let Some(bytes) = source.bytes() {
                n = n.saturating_add(bytes.len() as u64);
            }
            if let Some(crate::EntryData::Brace(doc)) = source.entry() {
                n = n.saturating_add(node_size(&doc.root, 0)?);
            }
        }
        if let Some(m) = &sub.module {
            n = n.saturating_add(m.source.len() as u64);
        }
    }
    for m in &d.object.modules {
        n = n.saturating_add(m.source.len() as u64);
    }
    for entry in &d.copy_info.objects {
        n = n.saturating_add(entry.name.len() as u64 + 512);
    }
    n = n.saturating_add(
        ((d.copy_info.types.len()
            + d.copy_info.data_paths.len()
            + d.copy_info.standard_data_paths.len()) as u64)
            .saturating_mul(512),
    );
    for help in d
        .object
        .parts
        .help
        .iter()
        .chain(d.object.parts.form_help.iter().map(|(_, h)| h))
    {
        for (name, data) in help.pages.iter().chain(&help.files) {
            n = n.saturating_add(name.len() as u64 + data.len() as u64);
        }
    }
    n.checked_mul(16)
        .ok_or_else(|| bad("memory", "size overflow"))
}
fn node_size(node: &Node, depth: usize) -> Result<u64> {
    if depth > 128 {
        return Err(bad("form", "native form depth limit"));
    }
    let mut n = std::mem::size_of::<Node>() as u64;
    match node {
        Node::Atom(s) | Node::Str(s) => n = n.saturating_add(s.len() as u64),
        Node::List(items) => {
            for item in items {
                n = n.saturating_add(node_size(item, depth + 1)?);
            }
        }
    }
    Ok(n)
}
pub fn build(
    d: &ExternalDocument,
    sink: &mut dyn EntrySink,
    o: &ExternalBuildOptions,
    budget: &MemoryBudget,
) -> Result<()> {
    prepare(d, o, budget)?.write_to(sink)
}
pub fn write(
    d: &ExternalDocument,
    path: &Path,
    o: &ExternalBuildOptions,
    budget: &MemoryBudget,
) -> Result<()> {
    prepare(d, o, budget)?.write(path)
}
