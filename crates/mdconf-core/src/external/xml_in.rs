use super::{ExternalDocument, ExternalKind};
use crate::xml_in::{
    ObjectOptions, ObjectReport, ValueReader,
    objects::Assembly,
    schema::descriptor,
    shell_xml::{self, MD, N, XR, attr, block, leaf, required},
    walk::{component, err, inventory_with, nfc},
};
use crate::{
    Child, ChildTag, GeneratedType, MdObject, Module, ModuleKind, Result, SubKind, SubObject, Value,
};
use brace::Guid;
use memory_budget::{Category, MemoryBudget, Reservation};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, Default)]
pub struct XmlLoadOptions {
    pub objects: ObjectOptions,
    pub cancelled: Option<Arc<AtomicBool>>,
    pub resolution: md_model::index::Index,
}

#[derive(Debug)]
pub struct LoadedXmlExternal {
    document: ExternalDocument,
    _charges: Vec<Reservation>,
    _codec_holds: Vec<memory_budget::Shared<()>>,
}
impl std::ops::Deref for LoadedXmlExternal {
    type Target = ExternalDocument;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

#[doc(hidden)]
pub trait XmlCodecs {
    fn form(
        &self,
        _bytes: &[u8],
        path: &Path,
        _index: &md_model::index::Index,
        _options: &XmlLoadOptions,
        _budget: &MemoryBudget,
    ) -> Result<md_model::forms::Form> {
        Err(err(path, "form XML requires the forms codec"))
    }
    fn form_document(
        &self,
        input: FormXmlInput<'_>,
        document: &ExternalDocument,
        options: &XmlLoadOptions,
        budget: &MemoryBudget,
    ) -> Result<md_model::forms::Form> {
        self.form(input.xml, input.path, &document.index, options, budget)
    }
}
pub struct FormXmlInput<'a> {
    pub id: Guid,
    pub xml: &'a [u8],
    pub path: &'a Path,
    pub module: &'a str,
    pub files: &'a [(String, Vec<u8>)],
    pub owner: &'a str,
    pub retain: &'a mut dyn FnMut(memory_budget::Shared<()>),
}
pub struct NoXmlCodecs;
impl XmlCodecs for NoXmlCodecs {}
pub(crate) fn check(cancelled: &Option<Arc<AtomicBool>>) -> Result<()> {
    if cancelled
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
    {
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "external conversion cancelled",
        )));
    }
    Ok(())
}
pub fn load_xml(
    root: &Path,
    options: &XmlLoadOptions,
    budget: &MemoryBudget,
) -> Result<LoadedXmlExternal> {
    load_xml_with(root, options, budget, &NoXmlCodecs)
}

struct Input<'a> {
    options: &'a XmlLoadOptions,
    budget: &'a MemoryBudget,
    charges: Vec<Reservation>,
    fragment_charge: Reservation,
    files: BTreeMap<PathBuf, PathBuf>,
    used: HashSet<PathBuf>,
    total: u64,
    module_total: u64,
    work: u64,
    ids: HashSet<Guid>,
    generated: std::collections::HashMap<Guid, Vec<GeneratedType>>,
    name_bytes: usize,
    codec_holds: Vec<memory_budget::Shared<()>>,
}
impl Input<'_> {
    fn parse<'a>(
        &mut self,
        bytes: &'a [u8],
        path: &Path,
    ) -> Result<crate::xml::view::XmlArena<'a>> {
        let cancelled = &self.options.cancelled;
        crate::xml_in::parse_part_checked(
            bytes,
            path,
            &self.options.objects.shells,
            self.budget,
            &mut self.work,
            &|| {
                cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
            },
        )
        .map_err(part_error)
    }
    fn declare(&mut self, id: Guid, path: &Path) -> Result<()> {
        if id.is_nil() || self.ids.contains(&id) {
            return Err(err(path, "duplicate or nil declaration UUID"));
        }
        if self.ids.len() >= self.options.objects.shells.max_identities {
            return Err(err(path, "identity limit"));
        }
        self.charge(256)?;
        self.ids
            .try_reserve(1)
            .map_err(|_| err(path, "identity allocation failed"))?;
        self.ids.insert(id);
        Ok(())
    }
    fn resolution(&mut self) -> Result<md_model::index::Index> {
        let mut projected = md_model::index::Index::default();
        let options = self.options;
        for (id, name) in &options.resolution.names {
            self.charge((name.len() as u64).saturating_add(512).saturating_mul(8))?;
            projected.names.insert(*id, name.clone());
        }
        for (id, name) in &options.resolution.types {
            self.charge((name.len() as u64).saturating_add(512).saturating_mul(8))?;
            projected.types.insert(*id, name.clone());
            if let Some(owner) = options.resolution.type_owners.get(id) {
                self.charge((owner.category.len() as u64).saturating_add(512))?;
                projected.type_owners.insert(*id, owner.clone());
                if !projected.objs.contains_key(&owner.object)
                    && let Some((kind, name)) = options.resolution.objs.get(&owner.object)
                {
                    self.charge((name.as_ref().map_or(0, String::len) as u64).saturating_add(512))?;
                    projected.objs.insert(owner.object, (*kind, name.clone()));
                }
            }
        }
        for (id, header) in &options.resolution.sub_hdr {
            self.charge(
                (header.len() as u64)
                    .saturating_mul(std::mem::size_of::<usize>() as u64)
                    .saturating_add(512),
            )?;
            projected.sub_hdr.insert(*id, header.clone());
        }
        Ok(projected)
    }
    fn admit_name(&mut self, name: &str, path: &Path) -> Result<()> {
        check(&self.options.cancelled)?;
        let o = &self.options.objects.shells;
        self.name_bytes = self
            .name_bytes
            .checked_add(name.len())
            .filter(|n| *n <= o.max_total_name_bytes)
            .ok_or_else(|| err(path, "total name byte limit"))?;
        if name.len() > o.max_full_name_bytes {
            return Err(err(path, "full name byte limit"));
        }
        Ok(())
    }
    fn charge(&mut self, bytes: u64) -> Result<()> {
        check(&self.options.cancelled)?;
        self.charges.push(
            self.budget
                .try_reserve(Category::Metadata, bytes)
                .map_err(crate::Error::Memory)?,
        );
        Ok(())
    }
    fn read(&mut self, path: &Path, metadata: bool) -> Result<Vec<u8>> {
        check(&self.options.cancelled)?;
        let meta = std::fs::symlink_metadata(path).map_err(|e| err(path, e.to_string()))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(err(path, "expected regular file, not a symlink"));
        }
        let o = &self.options.objects.shells;
        let size = meta.len();
        if size > o.max_file_bytes || size > o.max_total_bytes.saturating_sub(self.total) {
            return Err(err(path, "external input byte limit"));
        }
        self.charge(
            size.checked_mul(if metadata { 64 } else { 2 })
                .and_then(|n| n.checked_add(4096))
                .ok_or_else(|| err(path, "input accounting overflow"))?,
        )?;
        let cap = usize::try_from(size).map_err(|_| err(path, "input size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(cap)
            .map_err(|_| err(path, "file allocation failed"))?;
        std::fs::File::open(path)
            .map_err(|e| err(path, e.to_string()))?
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| err(path, e.to_string()))?;
        if bytes.len() as u64 != size {
            return Err(err(path, "file changed during load"));
        }
        check(&self.options.cancelled)?;
        self.total += size;
        Ok(bytes)
    }
    fn asset(
        &mut self,
        rel: &Path,
        required_asset: bool,
        metadata: bool,
    ) -> Result<Option<Vec<u8>>> {
        let key = PathBuf::from(nfc(&rel.to_string_lossy()));
        let Some(real) = self.files.get(&key).cloned() else {
            return if required_asset {
                Err(err(rel, "missing external asset"))
            } else {
                Ok(None)
            };
        };
        if !self.used.insert(key) {
            return Err(err(rel, "asset referenced twice"));
        }
        let module = rel.extension().is_some_and(|e| e == "bsl");
        if module {
            let len = std::fs::symlink_metadata(&real)?.len();
            if len > self.options.objects.max_module_bytes
                || len
                    > self
                        .options
                        .objects
                        .max_total_module_bytes
                        .saturating_sub(self.module_total)
            {
                return Err(err(rel, "module byte limit"));
            }
        }
        let bytes = self.read(&real, metadata)?;
        if module {
            std::str::from_utf8(&bytes).map_err(|_| err(rel, "module is not UTF-8"))?;
            self.module_total += bytes.len() as u64;
        }
        Ok(Some(bytes))
    }
    fn id(&mut self, n: N<'_, '_>, path: &Path) -> Result<Guid> {
        check(&self.options.cancelled)?;
        let id = attr(n, None, "uuid")
            .and_then(Guid::parse)
            .filter(|id| !id.is_nil())
            .ok_or_else(|| err(path, "invalid or missing UUID"))?;
        self.declare(id, path)?;
        Ok(id)
    }
    fn generated(
        &mut self,
        n: N<'_, '_>,
        tag: &str,
        name: &str,
        index: &mut md_model::index::Index,
        path: &Path,
    ) -> Result<Vec<GeneratedType>> {
        let mut out = Vec::new();
        let Some(info) = block(n, MD, "InternalInfo", path)? else {
            return Ok(out);
        };
        structural(
            info,
            &[],
            XR,
            if tag.starts_with("External") {
                &["ContainedObject", "GeneratedType"]
            } else {
                &["GeneratedType"]
            },
            path,
        )?;
        for g in info.children() {
            if g.ns() == Some(XR) && g.local() == "ContainedObject" {
                continue;
            }
            if g.ns() != Some(XR) || g.local() != "GeneratedType" {
                return Err(err(path, "unknown InternalInfo element"));
            }
            structural(g, &["category", "name"], XR, &["TypeId", "ValueId"], path)?;
            let category =
                attr(g, None, "category").ok_or_else(|| err(path, "missing generated category"))?;
            if !(if tag.starts_with("External") {
                category == "Object"
            } else {
                matches!(category, "TabularSection" | "TabularSectionRow")
            }) || out.iter().any(|x: &GeneratedType| x.category == category)
            {
                return Err(err(path, "unsupported or duplicate generated category"));
            }
            let full = format!("{tag}{category}.{name}");
            self.admit_name(&full, path)?;
            if attr(g, None, "name").map(nfc).as_deref() != Some(&full) {
                return Err(err(path, format!("wrong generated name, expected {full}")));
            }
            let tid = guid(required(g, XR, "TypeId", path)?, path)?;
            let vid = guid(required(g, XR, "ValueId", path)?, path)?;
            self.declare(tid, path)?;
            self.declare(vid, path)?;
            if index.types.insert(tid, full).is_some() {
                return Err(err(path, "duplicate generated identity"));
            }
            out.push(GeneratedType {
                category: category.into(),
                type_id: tid,
                value_id: vid,
            });
        }
        Ok(out)
    }
}
fn structural(n: N<'_, '_>, attrs: &[&str], ns: &str, tags: &[&str], path: &Path) -> Result<()> {
    shell_xml::attributes(n, attrs, path)?;
    if n.text().is_some_and(|t| !shell_xml::xml_whitespace(t))
        || n.children()
            .any(|c| c.ns() != Some(ns) || !tags.contains(&c.local()))
    {
        return Err(err(path, "unknown structural content"));
    }
    Ok(())
}
fn guid(n: N<'_, '_>, path: &Path) -> Result<Guid> {
    Guid::parse(leaf(n, path)?)
        .filter(|g| !g.is_nil())
        .ok_or_else(|| err(path, "invalid UUID text"))
}
fn name(n: N<'_, '_>, path: &Path) -> Result<String> {
    let props = required(n, MD, "Properties", path)?;
    component(path, leaf(required(props, MD, "Name", path)?, path)?)
}
fn assembly(index: &md_model::index::Index, o: &XmlLoadOptions) -> Result<Assembly> {
    Ok(Assembly {
        reader: ValueReader::with_index(index)?,
        report: ObjectReport::default(),
        max_value_bytes: o.objects.max_value_bytes,
        namespace_envelopes: Default::default(),
    })
}
fn properties(
    a: &mut Assembly,
    n: N<'_, '_>,
    spec: &[(String, mdschema::PropSpec)],
    tag: &str,
    path: &Path,
    input: &Input<'_>,
) -> Result<Vec<crate::Prop>> {
    let mut seen = HashSet::new();
    let properties = required(n, MD, "Properties", path)?;
    shell_xml::attributes(properties, &[], path)?;
    for p in properties.children() {
        if p.ns() != Some(MD)
            || !seen.insert(p.local())
            || descriptor(
                mdschema::schema()?,
                spec,
                tag,
                &format!("{tag}Properties"),
                p.local(),
            )
            .is_none()
        {
            return Err(err(
                path,
                format!("unknown, duplicate or foreign property {}", p.local()),
            ));
        }
    }
    let mut props = a.properties_admitted(
        n,
        spec,
        tag,
        &format!("{tag}Properties"),
        path,
        &crate::xml_in::objects::FragmentAdmission {
            pay: &|bytes| {
                input
                    .fragment_charge
                    .try_grow(bytes)
                    .map_err(crate::Error::Memory)
            },
            check: &|| check(&input.options.cancelled),
        },
    )?;
    for p in &mut props {
        a.reader
            .qualified_external_empty_collection(&format!("{tag}Properties"), p);
        if let Value::Ref(r) = &mut p.value
            && r.id.is_none()
            && let Some(id) = Guid::parse(&r.name)
        {
            r.id = Some(id);
            a.reader
                .resolved_uuid_reference(&format!("{tag}Properties"), &p.name);
        }
    }
    Ok(props)
}
fn empty_object(kind: ExternalKind, id: Guid, name: String) -> MdObject {
    MdObject {
        kind: kind.base_kind(),
        id,
        name,
        synonym: Default::default(),
        comment: String::new(),
        generated_types: Vec::new(),
        this_node: None,
        props: Vec::new(),
        children: Vec::new(),
        forms: Vec::new(),
        templates: Vec::new(),
        recalculations: Vec::new(),
        external: Vec::new(),
        subsystems: Vec::new(),
        modules: Vec::new(),
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    }
}

#[doc(hidden)]
pub fn load_xml_with(
    root: &Path,
    o: &XmlLoadOptions,
    budget: &MemoryBudget,
    codecs: &dyn XmlCodecs,
) -> Result<LoadedXmlExternal> {
    check(&o.cancelled)?;
    if o.objects.shells.max_metadata_depth > 64 || o.objects.shells.max_xml_depth > 256 {
        return Err(err(root, "depth option exceeds safe limit"));
    }
    let mut input = Input {
        options: o,
        budget,
        charges: Vec::new(),
        fragment_charge: budget
            .try_reserve(Category::Metadata, 0)
            .map_err(crate::Error::Memory)?,
        files: BTreeMap::new(),
        used: HashSet::new(),
        total: 0,
        module_total: 0,
        work: 0,
        ids: HashSet::new(),
        generated: Default::default(),
        name_bytes: 0,
        codec_holds: Vec::new(),
    };
    let raw = input.read(root, true)?;
    let arena = input.parse(&raw, root)?;
    let envelope = arena.root().ok_or_else(|| err(root, "missing root"))?;
    structural(
        envelope,
        &["version"],
        MD,
        &["ExternalDataProcessor", "ExternalReport"],
        root,
    )?;
    if envelope.ns() != Some(MD)
        || envelope.local() != "MetaDataObject"
        || attr(envelope, None, "version") != Some("2.21")
    {
        return Err(err(root, "expected native MetaDataObject version 2.21"));
    }
    let mut nodes = envelope.children();
    let node = nodes
        .next()
        .ok_or_else(|| err(root, "missing external object"))?;
    if nodes.next().is_some() || node.ns() != Some(MD) {
        return Err(err(root, "expected exactly one external object"));
    }
    let kind = match node.local() {
        "ExternalDataProcessor" => ExternalKind::DataProcessor,
        "ExternalReport" => ExternalKind::Report,
        _ => return Err(err(root, "unknown external class")),
    };
    structural(
        node,
        &["uuid"],
        MD,
        &["InternalInfo", "Properties", "ChildObjects"],
        root,
    )?;
    let wrapper = input.id(node, root)?;
    let info = required(node, MD, "InternalInfo", root)?;
    let contained = required(info, XR, "ContainedObject", root)?;
    structural(contained, &[], XR, &["ClassId", "ObjectId"], root)?;
    if guid(required(contained, XR, "ClassId", root)?, root)? != kind.class_id() {
        return Err(err(root, "ContainedObject class differs from XML kind"));
    }
    let id = guid(required(contained, XR, "ObjectId", root)?, root)?;
    if id == wrapper {
        return Err(err(root, "wrapper and internal UUID must differ"));
    }
    input.declare(id, root)?;
    let object_name = name(node, root)?;
    input.admit_name(&format!("{}.{}", kind.xml_name(), object_name), root)?;
    let mut index = input.resolution()?;
    index
        .names
        .insert(id, format!("{}.{}", kind.xml_name(), object_name));
    let mut object = empty_object(kind, id, object_name);
    object.generated_types =
        input.generated(node, kind.xml_name(), &object.name, &mut index, root)?;
    if object.generated_types.len() != 1 {
        return Err(err(root, "missing external Object generated type"));
    }
    let spec = crate::objects::overlay::kind(mdschema::schema()?, kind.base_kind().as_str())
        .ok_or_else(|| err(root, "missing base kind schema"))?;
    let children = required(node, MD, "ChildObjects", root)?;
    index_children(
        &mut input,
        children,
        kind,
        &format!("{}.{}", kind.xml_name(), object.name),
        &mut index,
        root,
        0,
    )?;
    let stem = root
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| err(root, "invalid root XML filename"))?;
    let assets = root.with_file_name(stem);
    if assets.try_exists()? {
        input.files = inventory_with(&assets, &o.objects.shells, &mut |bytes| input.charge(bytes))?;
    }
    let mut subnodes = Vec::new();
    for c in children
        .children()
        .filter(|n| matches!(n.local(), "Form" | "Template"))
    {
        let subname = component(root, leaf(c, root)?)?;
        let folder = if c.local() == "Form" {
            "Forms"
        } else {
            "Templates"
        };
        let rel = PathBuf::from(folder).join(format!("{subname}.xml"));
        let bytes = input
            .asset(&rel, true, true)?
            .ok_or_else(|| err(&rel, "missing subobject"))?;
        let arena = input.parse(&bytes, &rel)?;
        let env = arena
            .root()
            .ok_or_else(|| err(&rel, "missing subobject root"))?;
        structural(env, &["version"], MD, &[c.local()], &rel)?;
        if env.local() != "MetaDataObject"
            || env.ns() != Some(MD)
            || attr(env, None, "version") != Some("2.21")
        {
            return Err(err(&rel, "unsupported subobject XML version"));
        }
        let mut descriptions = env.children();
        let n = descriptions
            .next()
            .filter(|n| n.local() == c.local() && n.ns() == Some(MD))
            .ok_or_else(|| err(&rel, "wrong subobject description"))?;
        if descriptions.next().is_some() {
            return Err(err(&rel, "expected exactly one subobject description"));
        }
        drop(descriptions);
        structural(n, &["uuid"], MD, &["Properties"], &rel)?;
        let sid = input.id(n, &rel)?;
        if name(n, &rel)? != subname {
            return Err(err(&rel, "subobject name differs from ChildObjects"));
        }
        let full = format!(
            "{}.{}.{}.{subname}",
            kind.xml_name(),
            object.name,
            c.local()
        );
        input.admit_name(&full, &rel)?;
        index.names.insert(sid, full);
        index.sub_hdr.insert(
            sid,
            if c.local() == "Form" {
                vec![1, 1, 1, 1]
            } else {
                vec![1, 2]
            },
        );
        subnodes.push((c.local().to_string(), subname, sid, rel, bytes));
    }
    let mut a = assembly(&index, o)?;
    let root_specs = super::build::root_specs(kind);
    object.props = properties(&mut a, node, &root_specs, kind.xml_name(), root, &input)?;
    crate::xml_in::objects::headers(&object.props, &mut object.synonym, &mut object.comment);
    object.children = read_children(&mut input, children, &spec.children, &mut a, root, 0)?;
    let mut pending_forms = Vec::new();
    for (tag, subname, sid, rel, bytes) in subnodes {
        let arena = input.parse(&bytes, &rel)?;
        let n = arena
            .root()
            .and_then(|n| n.children().next())
            .ok_or_else(|| err(&rel, "missing subobject"))?;
        let specs = super::build::sub_specs(&tag)?;
        let props = properties(&mut a, n, &specs, &tag, &rel, &input)?;
        let mut sub = SubObject {
            kind: if tag == "Form" {
                SubKind::Form
            } else {
                SubKind::Template
            },
            id: sid,
            name: subname.clone(),
            props,
            module: None,
            form: None,
            template: None,
        };
        let ext = PathBuf::from(if tag == "Form" { "Forms" } else { "Templates" })
            .join(&subname)
            .join("Ext");
        if tag == "Form" {
            let path = ext.join("Form.xml");
            let bytes = input
                .asset(&path, true, true)?
                .ok_or_else(|| err(&path, "missing form"))?;
            sub.module = input
                .asset(&ext.join("Form/Module.bsl"), false, false)?
                .map(|source| Module {
                    kind: ModuleKind::FormModule,
                    source,
                });
            let mut files = Vec::new();
            for key in input
                .files
                .keys()
                .filter(|p| p.starts_with(ext.join("Form")) && !input.used.contains(*p))
                .cloned()
                .collect::<Vec<_>>()
            {
                let rel = key
                    .strip_prefix(ext.join("Form"))
                    .map_err(|e| err(&key, e.to_string()))?
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let bytes = input
                    .asset(&key, true, false)?
                    .ok_or_else(|| err(&key, "missing form asset"))?;
                files.push((rel, bytes));
            }
            pending_forms.push((sid, path, bytes, files));
            if let Some(h) = read_help(&mut input, &ext)? {
                object.parts.form_help.push((sid, h));
            }
            object.forms.push(sub);
        } else {
            let kind = sub
                .props
                .iter()
                .find(|p| p.name == "TemplateType")
                .and_then(|p| {
                    if let Value::Text(s) = &p.value {
                        md_model::templates::TemplateType::from_name(s)
                    } else {
                        None
                    }
                })
                .ok_or_else(|| err(&rel, "missing or unsupported TemplateType"))?;
            let mut files = Vec::new();
            for key in input
                .files
                .keys()
                .filter(|p| p.starts_with(&ext))
                .cloned()
                .collect::<Vec<_>>()
            {
                let name = key
                    .strip_prefix(&ext)
                    .map_err(|e| err(&key, e.to_string()))?
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let data = input
                    .asset(&key, true, false)?
                    .ok_or_else(|| err(&key, "missing template asset"))?;
                files.push((name, data));
            }
            if files.is_empty() && kind != md_model::templates::TemplateType::SpreadsheetDocument {
                return Err(err(&ext, "missing template payload"));
            }
            sub.template = Some(md_model::templates::Template {
                kind: Some(kind),
                files,
            });
            object.templates.push(sub);
        }
    }
    if let Some(source) = input.asset(Path::new("Ext/ObjectModule.bsl"), false, false)? {
        object.modules.push(Module {
            kind: ModuleKind::ObjectModule,
            source,
        });
    }
    object.parts.help = read_help(&mut input, Path::new("Ext"))?;
    if let Some(path) = input.files.keys().find(|p| !input.used.contains(*p)) {
        return Err(err(path, "unconsumed external asset"));
    }
    let mut document = ExternalDocument::new(kind, wrapper, object);
    document.index = index;
    document.diagnostics = a.reader.into_diagnostics();
    let owner = format!("{}.{}", kind.xml_name(), document.object.name);
    for (id, path, xml, files) in pending_forms {
        check(&o.cancelled)?;
        let at = document
            .object
            .forms
            .iter()
            .position(|f| f.id == id)
            .ok_or_else(|| err(&path, "missing form metadata"))?;
        let module = document
            .object
            .forms
            .get(at)
            .and_then(|f| f.module.as_ref())
            .map(|m| m.source.as_slice())
            .unwrap_or_default();
        let module = std::str::from_utf8(module.strip_prefix(b"\xef\xbb\xbf").unwrap_or(module))
            .map_err(|_| err(&path, "form module is not UTF-8"))?;
        let form = codecs.form_document(
            FormXmlInput {
                id,
                xml: &xml,
                path: &path,
                module,
                files: &files,
                owner: &owner,
                retain: &mut |hold| input.codec_holds.push(hold),
            },
            &document,
            o,
            budget,
        )?;
        check(&o.cancelled)?;
        document
            .object
            .forms
            .get_mut(at)
            .ok_or_else(|| err(&path, "missing form metadata"))?
            .form = Some(form);
    }
    check(&o.cancelled)?;
    input
        .fragment_charge
        .try_grow(std::mem::size_of::<Reservation>() as u64)
        .map_err(crate::Error::Memory)?;
    input
        .charges
        .try_reserve_exact(1)
        .map_err(|_| crate::Error::Io(std::io::Error::from(std::io::ErrorKind::OutOfMemory)))?;
    input.charges.push(input.fragment_charge);
    Ok(LoadedXmlExternal {
        document,
        _charges: input.charges,
        _codec_holds: input.codec_holds,
    })
}

fn index_children(
    input: &mut Input<'_>,
    nodes: N<'_, '_>,
    kind: ExternalKind,
    owner: &str,
    index: &mut md_model::index::Index,
    path: &Path,
    depth: usize,
) -> Result<()> {
    if depth > input.options.objects.shells.max_metadata_depth {
        return Err(err(path, "metadata depth limit"));
    }
    structural(
        nodes,
        &[],
        MD,
        &["Attribute", "TabularSection", "Form", "Template"],
        path,
    )?;
    for n in nodes.children() {
        if n.ns() != Some(MD) {
            return Err(err(path, "foreign ChildObjects namespace"));
        }
        if matches!(n.local(), "Form" | "Template") {
            if depth != 0 {
                return Err(err(path, "subobject is not allowed inside child metadata"));
            }
            continue;
        }
        if !matches!(n.local(), "Attribute" | "TabularSection") {
            return Err(err(path, format!("unsupported child {}", n.local())));
        }
        structural(
            n,
            &["uuid"],
            MD,
            &["InternalInfo", "Properties", "ChildObjects"],
            path,
        )?;
        let id = input.id(n, path)?;
        let full = format!("{owner}.{}.{}", n.local(), name(n, path)?);
        input.admit_name(&full, path)?;
        if index
            .names
            .values()
            .any(|v| v.to_lowercase() == full.to_lowercase())
        {
            return Err(err(path, "duplicate child name"));
        }
        index.names.insert(id, full.clone());
        let base = kind.base_kind().as_str();
        let owner_name = owner.split_once('.').map(|(_, n)| n).unwrap_or(owner);
        let generated = input.generated(
            n,
            base,
            &format!("{owner_name}.{}", name(n, path)?),
            index,
            path,
        )?;
        input.generated.insert(id, generated);
        if let Some(c) = block(n, MD, "ChildObjects", path)? {
            index_children(input, c, kind, &full, index, path, depth + 1)?;
        }
    }
    Ok(())
}
fn read_children(
    input: &mut Input<'_>,
    nodes: N<'_, '_>,
    specs: &[(String, mdschema::ChildSpec)],
    a: &mut Assembly,
    path: &Path,
    depth: usize,
) -> Result<Vec<Child>> {
    if depth > input.options.objects.shells.max_metadata_depth {
        return Err(err(path, "metadata depth limit"));
    }
    let mut children = Vec::new();
    for n in nodes
        .children()
        .filter(|n| depth != 0 || !matches!(n.local(), "Form" | "Template"))
    {
        let spec = specs
            .iter()
            .find(|(_, s)| s.tag == n.local())
            .map(|(_, s)| s)
            .ok_or_else(|| err(path, "unsupported child kind"))?;
        let id = attr(n, None, "uuid")
            .and_then(Guid::parse)
            .ok_or_else(|| err(path, "missing child UUID"))?;
        let name = name(n, path)?;
        let generated_types = input.generated.remove(&id).unwrap_or_default();
        let props = properties(a, n, &spec.props, n.local(), path, input)?;
        let mut synonym = Default::default();
        let mut comment = String::new();
        crate::xml_in::objects::headers(&props, &mut synonym, &mut comment);
        let nested = if let Some(c) = block(n, MD, "ChildObjects", path)? {
            read_children(input, c, &spec.children, a, path, depth + 1)?
        } else {
            Vec::new()
        };
        children.push(Child {
            tag: if n.local() == "Attribute" {
                ChildTag::Attribute
            } else {
                ChildTag::TabularSection
            },
            id,
            name,
            synonym,
            comment,
            generated_types,
            props,
            children: nested,
            module: None,
        });
    }
    Ok(children)
}
fn read_help(input: &mut Input<'_>, ext: &Path) -> Result<Option<md_model::parts::Help>> {
    let rel = ext.join("Help.xml");
    let Some(raw) = input.asset(&rel, false, true)? else {
        return Ok(None);
    };
    let arena = input.parse(&raw, &rel)?;
    let root = arena
        .root()
        .filter(|n| n.local() == "Help" && n.ns() == Some("http://v8.1c.ru/8.3/xcf/extrnprops"))
        .ok_or_else(|| err(&rel, "invalid Help.xml"))?;
    let mut help = md_model::parts::Help::default();
    let mut seen = HashSet::new();
    for page in root.children() {
        if page.local() != "Page" {
            return Err(err(&rel, "unknown help element"));
        }
        let language = component(&rel, leaf(page, &rel)?)?;
        if !seen.insert(language.clone()) {
            return Err(err(&rel, "duplicate help language"));
        }
        let p = ext.join("Help").join(format!("{language}.html"));
        help.pages.push((
            language,
            input
                .asset(&p, true, false)?
                .ok_or_else(|| err(&p, "missing help page"))?,
        ));
    }
    let attachments = ext.join("Help/_files");
    for key in input
        .files
        .keys()
        .filter(|p| p.starts_with(&attachments))
        .cloned()
        .collect::<Vec<_>>()
    {
        let name = key
            .strip_prefix(&attachments)
            .map_err(|e| err(&key, e.to_string()))?
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        help.files.push((
            name,
            input
                .asset(&key, true, false)?
                .ok_or_else(|| err(&key, "missing help attachment"))?,
        ));
    }
    Ok(Some(help))
}

fn part_error(error: crate::xml_in::CheckedPartError) -> crate::Error {
    use crate::xml_in::CheckedPartError;
    use memory_budget::BudgetError;
    match error {
        CheckedPartError::Part(error) => error,
        CheckedPartError::Memory(BudgetError::Limit(limit)) => crate::Error::Memory(limit),
        CheckedPartError::Memory(BudgetError::AllocationFailed | BudgetError::Overflow) => {
            std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
        }
        CheckedPartError::Cancelled => std::io::Error::from(std::io::ErrorKind::Interrupted).into(),
    }
}
