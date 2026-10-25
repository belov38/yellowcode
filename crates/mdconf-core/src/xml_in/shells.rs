use super::{
    shell_index::{Identities, guid_text},
    shell_xml::*,
    walk::{component, err, nfc},
    *,
};
use crate::{
    index::{Index, TypeOwner},
    model::*,
    objects::{decode::child_tag, overlay},
};
use brace::Guid;
use mdschema::{ChildSpec, MdKind, Schema};
use memory_budget::{Limits, MemoryBudget};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

pub(super) fn load(dir: &Path, opts: &ShellOptions) -> Result<ShellDump> {
    load_with_values(dir, opts, None).map(|(dump, _)| dump)
}
pub(super) fn load_with_values(
    dir: &Path,
    opts: &ShellOptions,
    assembly: Option<objects::Assembly>,
) -> Result<(ShellDump, ObjectReport)> {
    let files = walk::inventory(dir, opts)?;
    let mut loader = Loader {
        files,
        opts,
        s: mdschema::schema()?,
        used: BTreeSet::new(),
        bytes: 0,
        text_work: 0,
        ids: Identities::new(opts),
        ix: Index::default(),
        diag: Diagnostics::default(),
        report: ShellReport::default(),
        budget: MemoryBudget::new(Limits::new(opts.max_xml_memory)),
        format: None,
        assembly,
    };
    let rel = Path::new("Configuration.xml");
    let bytes = loader.read(rel)?;
    let path = loader.real(rel)?;
    let budget = loader.budget.clone();
    let arena = parse(&bytes, &path, opts, &budget, &mut loader.text_work)?;
    let (root, fmt) = description(&arena, "Configuration", &path, &mut loader.diag)?;
    loader.format = Some(fmt);
    structure(root, &path)?;
    let name = object_name(root, &path)?;
    let full = loader.ids.full_name(&["Configuration.", &name], &path)?;
    let id = loader.ids.identity(root, &full, &path)?;
    let header = loader.configuration_header(root, &path)?;
    loader.ix.cfg_id = id;
    loader.ix.header_id = header;
    schema::properties(
        loader.s,
        root,
        &[],
        "Configuration",
        "ConfigurationProperties",
        &path,
        &mut loader.report,
        &mut loader.diag,
    )?;
    let mut objects = Vec::new();
    if let Some(children) = block(root, MD, "ChildObjects", &path)? {
        for c in children.children() {
            if c.ns() != Some(MD) {
                return Err(err(&path, "unknown configuration child namespace"));
            }
            let kind = MdKind::from_name(c.local())
                .filter(|k| k.folder().is_some())
                .ok_or_else(|| err(&path, "unknown metadata kind"))?;
            let name = component(&path, leaf(c, &path)?)?;
            let folder = overlay::kind(loader.s, kind.as_str())
                .and_then(|s| s.folder.as_deref())
                .or_else(|| kind.folder())
                .ok_or_else(|| err(&path, "missing kind folder"))?;
            let rel = Path::new(folder).join(format!("{name}.xml"));
            let full = loader.ids.full_name(&[kind.as_str(), ".", &name], &path)?;
            if let Some(obj) = loader.object(kind, &name, &rel, &full, None, 0)? {
                objects.push(obj);
            }
        }
    }
    loader.report.identities = loader.ids.count;
    loader.report.unread_files = loader
        .files
        .keys()
        .filter(|p| !loader.used.contains(*p))
        .cloned()
        .collect();
    loader.report.unreferenced_descriptions = loader
        .report
        .unread_files
        .iter()
        .filter(|p| {
            p.extension().is_some_and(|e| e == "xml")
                && p.file_name().is_some_and(|n| n != "ConfigDumpInfo.xml")
                && !p.components().any(|c| c.as_os_str() == "Ext")
        })
        .cloned()
        .collect();
    let (report, message) = if let Some(assembly) = loader.assembly {
        let diagnostics = assembly.reader.into_diagnostics();
        for (key, count) in diagnostics.misses {
            *loader.diag.misses.entry(key).or_default() += count;
        }
        loader.diag.unsupported.extend(diagnostics.unsupported);
        (
            assembly.report,
            "XML objects: parts and configuration settings are not loaded; unsuitable for full dumping",
        )
    } else {
        (
            ObjectReport::default(),
            "XML shells only: properties, modules and parts are not loaded; unsuitable for queries or dumping",
        )
    };
    loader.diag.unsupported.push(message.into());
    let configuration = Configuration {
        id,
        header_id: header,
        objects,
        index: loader.ix,
        diagnostics: loader.diag,
        profile: crate::LoadProfile::Query,
        ..Default::default()
    };
    Ok((
        ShellDump {
            configuration,
            format: fmt,
            files: loader.files,
            report: loader.report,
        },
        report,
    ))
}
struct Loader<'o> {
    files: BTreeMap<PathBuf, PathBuf>,
    opts: &'o ShellOptions,
    s: &'static Schema,
    used: BTreeSet<PathBuf>,
    bytes: u64,
    text_work: u64,
    ids: Identities,
    ix: Index,
    diag: Diagnostics,
    report: ShellReport,
    budget: MemoryBudget,
    format: Option<XmlFormat>,
    assembly: Option<objects::Assembly>,
}
impl Loader<'_> {
    fn properties(
        &mut self,
        n: N<'_, '_>,
        spec: &[(String, mdschema::PropSpec)],
        tag: &str,
        scope: &str,
        path: &Path,
    ) -> Result<Vec<Prop>> {
        let mut shell_diagnostics = Diagnostics::default();
        schema::properties(
            self.s,
            n,
            spec,
            tag,
            scope,
            path,
            &mut self.report,
            if self.assembly.is_some() {
                &mut shell_diagnostics
            } else {
                &mut self.diag
            },
        )?;
        match &mut self.assembly {
            Some(a) => a.properties(n, spec, tag, scope, path),
            None => Ok(Vec::new()),
        }
    }
    fn real(&self, rel: &Path) -> Result<PathBuf> {
        self.files
            .get(rel)
            .cloned()
            .ok_or_else(|| err(rel, "missing description"))
    }
    fn read(&mut self, rel: &Path) -> Result<Vec<u8>> {
        let path = self.real(rel)?;
        if !self.used.insert(rel.into()) {
            return Err(err(&path, "description referenced twice"));
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|e| err(&path, e.to_string()))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(err(&path, "description is not a regular file"));
        }
        let cap = self
            .opts
            .max_file_bytes
            .min(self.opts.max_total_bytes.saturating_sub(self.bytes));
        if meta.len() > cap || cap == u64::MAX {
            return Err(err(&path, "description byte limit"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|e| err(&path, e.to_string()))?
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| err(&path, e.to_string()))?;
        if bytes.len() as u64 > cap {
            return Err(err(&path, "description byte limit"));
        }
        self.bytes += bytes.len() as u64;
        self.report.descriptions += 1;
        Ok(bytes)
    }
    fn configuration_header(&mut self, root: N<'_, '_>, path: &Path) -> Result<Guid> {
        let ii = required(root, MD, "InternalInfo", path)?;
        let mut found = None;
        let mut classes = BTreeSet::new();
        for c in ii.children() {
            if c.ns() != Some(XR) || c.local() != "ContainedObject" {
                return Err(err(path, "unknown configuration InternalInfo element"));
            }
            attributes(c, &[], path)?;
            let class = guid_text(required(c, XR, "ClassId", path)?, path)?;
            let object = guid_text(required(c, XR, "ObjectId", path)?, path)?;
            let text = class.to_string();
            if !self.s.cfg_classes().contains(&text)
                || !classes.insert(class)
                || c.children().count() != 2
            {
                return Err(err(path, "unknown or duplicate configuration part"));
            }
            self.ids.part(object, path)?;
            if self.s.cfg_classes().first() == Some(&text) {
                found = Some(object);
            }
        }
        found.ok_or_else(|| err(path, "missing configuration header part"))
    }
    fn object(
        &mut self,
        kind: MdKind,
        name: &str,
        rel: &Path,
        full: &str,
        parent: Option<Guid>,
        depth: usize,
    ) -> Result<Option<MdObject>> {
        if depth > self.opts.max_metadata_depth {
            return Err(err(rel, "metadata depth limit"));
        }
        let spec = overlay::kind(self.s, kind.as_str());
        let Some(spec) = spec else {
            self.unsupported_description(kind.as_str(), name, rel, full, depth)?;
            return Ok(None);
        };
        let bytes = self.read(rel)?;
        let path = self.real(rel)?;
        let budget = self.budget.clone();
        let arena = parse(&bytes, &path, self.opts, &budget, &mut self.text_work)?;
        let (n, fmt) = description(&arena, kind.as_str(), &path, &mut self.diag)?;
        self.version(fmt, &path)?;
        structure(n, &path)?;
        check_name(n, name, &path)?;
        let id = self.ids.identity(n, full, &path)?;
        self.ix.names.insert(id, full.into());
        self.ix.objs.insert(id, (kind, Some(name.into())));
        if let Some(parent) = parent {
            self.ix.nested.push((id, parent));
        } else {
            self.ix.order.push(id);
        }
        let generated_types = self.ids.generated(
            n,
            &spec.internal_info,
            kind.as_str(),
            name,
            owner(id, None, None),
            self.s,
            &mut self.ix,
            &mut self.diag,
            &path,
        )?;
        let this_node = block(n, MD, "InternalInfo", &path)?
            .map(|ii| block(ii, XR, "ThisNode", &path))
            .transpose()?
            .flatten()
            .map(|n| guid_text(n, &path))
            .transpose()?;
        let props = self.properties(
            n,
            &spec.props,
            kind.as_str(),
            &format!("{}Properties", kind.as_str()),
            &path,
        )?;
        let mut obj = MdObject {
            kind,
            id,
            name: name.into(),
            synonym: Default::default(),
            comment: String::new(),
            generated_types,
            this_node,
            props,
            children: vec![],
            forms: vec![],
            templates: vec![],
            recalculations: vec![],
            subsystems: vec![],
            modules: vec![],
            parts: Default::default(),
            form: None,
            template: None,
            flowchart: None,
        };
        objects::headers(&obj.props, &mut obj.synonym, &mut obj.comment);
        if let Some(ch) = block(n, MD, "ChildObjects", &path)? {
            for c in ch.children() {
                if c.ns() != Some(MD) {
                    return Err(err(&path, "unknown child namespace"));
                }
                if let Some(cs) = spec
                    .children
                    .iter()
                    .find(|(_, cs)| cs.tag == c.local())
                    .map(|(_, cs)| cs)
                {
                    if let Some(child) =
                        self.child(c, cs, id, name, kind.as_str(), full, &path, depth + 1)?
                    {
                        obj.children.push(child);
                    }
                } else if spec.subobjects.iter().any(|(tag, _)| tag == c.local()) {
                    let sn = component(&path, leaf(c, &path)?)?;
                    let child_full = self
                        .ids
                        .full_name(&[full, ".", c.local(), ".", &sn], &path)?;
                    let folder = if c.local() == kind.as_str() {
                        spec.folder
                            .as_deref()
                            .ok_or_else(|| err(&path, "missing nested folder"))?
                    } else {
                        match c.local() {
                            "Form" => "Forms",
                            "Template" => "Templates",
                            _ => "",
                        }
                    };
                    if folder.is_empty() {
                        let folder = self
                            .s
                            .folder(c.local())
                            .map(String::from)
                            .unwrap_or_else(|| format!("{}s", c.local()));
                        let sr = rel
                            .with_extension("")
                            .join(folder)
                            .join(format!("{sn}.xml"));
                        self.unsupported_description(c.local(), &sn, &sr, &child_full, depth + 1)?;
                    } else {
                        let sr = rel
                            .with_extension("")
                            .join(folder)
                            .join(format!("{sn}.xml"));
                        if c.local() == kind.as_str() {
                            if let Some(s) =
                                self.object(kind, &sn, &sr, &child_full, Some(id), depth + 1)?
                            {
                                obj.subsystems.push(s);
                            }
                        } else {
                            let sub = self.subobject(c.local(), &sn, &sr, &child_full, id)?;
                            if sub.kind == SubKind::Form {
                                obj.forms.push(sub);
                            } else {
                                obj.templates.push(sub);
                            }
                        }
                    }
                } else if c.local() == "Recalculation" && kind == MdKind::CalculationRegister {
                    let sn = component(&path, leaf(c, &path)?)?;
                    let sr = rel
                        .with_extension("")
                        .join("Recalculations")
                        .join(format!("{sn}.xml"));
                    let recalc_full = self.ids.full_name(&[full, ".Recalculation.", &sn], &path)?;
                    obj.recalculations.push(self.recalculation(
                        &sn,
                        &sr,
                        &recalc_full,
                        id,
                        name,
                        depth + 1,
                    )?);
                } else {
                    return Err(err(&path, format!("unknown child {}", c.local())));
                }
            }
        }
        Ok(Some(obj))
    }
    fn version(&self, fmt: XmlFormat, path: &Path) -> Result<()> {
        if self.format != Some(fmt) {
            return Err(err(path, "mixed dump versions"));
        }
        Ok(())
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Schema extraction carries the source, owner and destination diagnostics explicitly"
    )]
    fn child(
        &mut self,
        n: N<'_, '_>,
        spec: &ChildSpec,
        object: Guid,
        objname: &str,
        kind: &str,
        full: &str,
        path: &Path,
        depth: usize,
    ) -> Result<Option<Child>> {
        if depth > self.opts.max_metadata_depth {
            return Err(err(path, "metadata depth limit"));
        }
        structure(n, path)?;
        let name = object_name(n, path)?;
        let full = self
            .ids
            .full_name(&[full, ".", &spec.tag, ".", &name], path)?;
        let id = self.ids.identity(n, &full, path)?;
        let tag = child_tag(&spec.tag);
        let generated_types = self.ids.generated(
            n,
            &spec.internal_info,
            md_xmlout::codec::objects::child_generated_kind(kind, &spec.tag),
            &format!("{objname}.{name}"),
            owner(object, Some(id), None),
            self.s,
            &mut self.ix,
            &mut self.diag,
            path,
        )?;
        let skipped_assembly = if tag.is_none() {
            self.assembly.take()
        } else {
            None
        };
        let props = self.properties(
            n,
            &spec.props,
            &spec.tag,
            &format!("{}Properties", spec.tag),
            path,
        )?;
        let mut children = Vec::new();
        if let Some(ch) = block(n, MD, "ChildObjects", path)? {
            for c in ch.children() {
                let cs = spec
                    .children
                    .iter()
                    .find(|(_, cs)| cs.tag == c.local())
                    .map(|(_, cs)| cs)
                    .ok_or_else(|| err(path, "unknown child collection"))?;
                if let Some(c) = self.child(c, cs, object, objname, kind, &full, path, depth + 1)? {
                    children.push(c);
                }
            }
        }
        let Some(tag) = tag else {
            self.assembly = skipped_assembly;
            self.report.unsupported.push(UnsupportedShell {
                id,
                name: full,
                path: path.into(),
            });
            return Ok(None);
        };
        self.ix.names.insert(id, full);
        let (mut synonym, mut comment) = Default::default();
        objects::headers(&props, &mut synonym, &mut comment);
        Ok(Some(Child {
            tag,
            id,
            name,
            synonym,
            comment,
            generated_types,
            props,
            children,
            module: None,
        }))
    }
}
fn owner(object: Guid, child: Option<Guid>, recalc: Option<Guid>) -> TypeOwner {
    TypeOwner {
        object,
        child,
        recalc,
        category: String::new(),
    }
}
fn structure(n: N<'_, '_>, path: &Path) -> Result<()> {
    if n.ns() != Some(MD) {
        return Err(err(path, "wrong metadata namespace"));
    }
    if n.attrs().any(|a| a.ns().is_some() || a.local() != "uuid") {
        return Err(err(path, "unknown metadata attribute"));
    }
    for c in n.children() {
        if c.ns() != Some(MD)
            || !matches!(c.local(), "Properties" | "InternalInfo" | "ChildObjects")
        {
            return Err(err(
                path,
                format!("unknown structural element {}", c.local()),
            ));
        }
    }
    for tag in ["Properties", "InternalInfo", "ChildObjects"] {
        if let Some(c) = block(n, MD, tag, path)? {
            attributes(c, &[], path)?;
        }
    }
    Ok(())
}
fn object_name(n: N<'_, '_>, path: &Path) -> Result<String> {
    let p = required(n, MD, "Properties", path)?;
    let name = required(p, MD, "Name", path)?;
    component(path, leaf(name, path)?)
}
fn check_name(n: N<'_, '_>, name: &str, path: &Path) -> Result<()> {
    if object_name(n, path)? != nfc(name) {
        return Err(err(path, "description name differs from listed name"));
    }
    Ok(())
}

#[path = "shell_subordinates.rs"]
mod subordinates;
