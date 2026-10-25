use super::compare_scalar::{ComparisonError, Encoded, encode_property};
use super::decode::{self, Provenance, ResolvedProperty, TreeDomain};
use super::encode_predefined_dtref::PredefinedContext;
use super::encode_reference::ReferenceContext;
use super::encode_type_context::TypeContext;
use crate::EntrySource;
use crate::entries::{Entries, EntryData};
use crate::index::Index;
use crate::model::{Child, Configuration, LocalString, MdObject, Prop, RecalcDimension, Value};
use brace::{Doc, Guid, Node};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Strict,
    KeepEqual,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PatchReport {
    pub entries: usize,
    pub written: usize,
    pub kept_equal: usize,
    pub kept: BTreeMap<String, u32>,
    pub unvisited_model: usize,
    pub edited: BTreeMap<Guid, Vec<(Guid, String)>>,
}

#[derive(Debug)]
pub(crate) enum PatchError {
    Comparison(ComparisonError),
    Entries(crate::Error),
    AmbiguousModel(Guid),
    Unwritable {
        object: String,
        path: String,
        what: String,
    },
    Internal(String),
}

type Result<T> = std::result::Result<T, PatchError>;

pub(crate) type Sink<'s> = dyn FnMut(Guid, &Doc, Option<Node>) -> Result<()> + 's;

type ModelProps<'m> = HashMap<Guid, &'m [Prop]>;

pub(crate) struct Overlay<'a> {
    pub base: &'a dyn EntrySource,
    pub patched: HashMap<String, Vec<u8>>,
}

impl EntrySource for Overlay<'_> {
    fn list(&self) -> crate::Result<Vec<crate::EntryMeta>> {
        self.base.list()
    }
    fn stored(&self, name: &str) -> crate::Result<Option<Vec<u8>>> {
        match self.patched.get(name) {
            Some(bytes) => Ok(Some(bytes.clone())),
            None => self.base.stored(name),
        }
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> crate::Result<Option<Vec<u8>>> {
        match self.patched.get(name) {
            Some(bytes) if bytes.len() > limit => Err(crate::Error::Format {
                entry: name.into(),
                what: "patched entry exceeds the read limit".into(),
            }),
            Some(bytes) => Ok(Some(bytes.clone())),
            None => self.base.stored_bounded(name, limit),
        }
    }
}

fn stored_bytes(doc: &Doc, root: Node) -> Result<Vec<u8>> {
    let text = brace::write_doc(&Doc {
        root,
        bom: doc.bom,
        lead: doc.lead.clone(),
        trailer: doc.trailer.clone(),
    });
    zraw::deflate(&text, zraw::LEVEL_1C).map_err(|e| PatchError::Internal(format!("deflate: {e}")))
}

fn insert<'m>(map: &mut ModelProps<'m>, id: Guid, props: &'m [Prop]) -> Result<()> {
    if map.insert(id, props).is_some() {
        return Err(PatchError::AmbiguousModel(id));
    }
    Ok(())
}

fn model_props(cfg: &Configuration) -> Result<ModelProps<'_>> {
    let mut map = HashMap::new();
    let mut objects: Vec<&MdObject> = cfg.objects.iter().collect();
    let mut children: Vec<&Child> = Vec::new();
    while let Some(o) = objects.pop() {
        header(o.id, &o.props, &o.name, Some(&o.synonym), Some(&o.comment))?;
        insert(&mut map, o.id, &o.props)?;
        children.extend(&o.children);
        for s in o.forms.iter().chain(&o.templates) {
            header(s.id, &s.props, &s.name, None, None)?;
            insert(&mut map, s.id, &s.props)?;
        }
        for r in &o.recalculations {
            header(r.id, &r.props, &r.name, Some(&r.synonym), Some(&r.comment))?;
            insert(&mut map, r.id, &r.props)?;
            for d in &r.dimensions {
                header(d.id, &d.props, &d.name, Some(&d.synonym), Some(&d.comment))?;
                recalc_dimension(d)?;
                insert(&mut map, d.id, &d.props)?;
            }
        }
        objects.extend(&o.subsystems);
    }
    while let Some(c) = children.pop() {
        header(c.id, &c.props, &c.name, Some(&c.synonym), Some(&c.comment))?;
        insert(&mut map, c.id, &c.props)?;
        children.extend(&c.children);
    }
    Ok(map)
}

fn header(
    id: Guid,
    props: &[Prop],
    name: &str,
    synonym: Option<&LocalString>,
    comment: Option<&str>,
) -> Result<()> {
    for p in props {
        let (field, same) = match (p.name.as_str(), &p.value) {
            ("Name", Value::Text(t)) => ("name", t == name),
            ("Synonym", Value::LStr(s)) => ("synonym", synonym.is_none_or(|x| x == s)),
            ("Comment", Value::Text(t)) => ("comment", comment.is_none_or(|x| x == t)),
            _ => continue,
        };
        if !same {
            return Err(PatchError::Unwritable {
                object: format!("{id} {name}"),
                path: field.to_string(),
                what: "field differs from props".to_string(),
            });
        }
    }
    Ok(())
}

fn recalc_dimension(d: &RecalcDimension) -> Result<()> {
    for p in &d.props {
        let (field, same) = match (p.name.as_str(), &p.value) {
            ("RegisterDimension", Value::Ref(r)) => {
                ("register_dimension", r == &d.register_dimension)
            }
            ("LeadingRegisterData", Value::RefList(l)) => {
                ("leading_register_data", l == &d.leading_register_data)
            }
            _ => continue,
        };
        if !same {
            return Err(PatchError::Unwritable {
                object: format!("{} {}", d.id, d.name),
                path: field.to_string(),
                what: "field differs from props".to_string(),
            });
        }
    }
    Ok(())
}

fn model_value<'m>(props: &'m [Prop], id: Guid, name: &str) -> Result<Option<&'m Value>> {
    let mut found = props.iter().filter(|p| p.name == name);
    let first = found.next();
    if found.next().is_some() {
        return Err(PatchError::AmbiguousModel(id));
    }
    Ok(first.map(|p| &p.value))
}

fn offsets(root: &Node) -> HashMap<*const Node, Vec<usize>> {
    let mut out = HashMap::new();
    let mut stack = vec![(root, Vec::new())];
    while let Some((node, path)) = stack.pop() {
        if let Some(items) = node.as_list() {
            for (i, item) in items.iter().enumerate() {
                if item.as_list().is_some() {
                    let mut p = path.clone();
                    p.push(i);
                    stack.push((item, p));
                }
            }
        }
        out.insert(std::ptr::from_ref(node), path);
    }
    out
}

fn blocked(v: &ResolvedProperty<'_>) -> Option<&'static str> {
    if v.masked {
        return Some("masked");
    }
    if v.type_filtered {
        return Some("type_filtered");
    }
    match v.provenance {
        Provenance::Derived => return Some("derived"),
        Provenance::Default => return Some("default"),
        Provenance::Unavailable => return Some("unavailable"),
        Provenance::Selected => {}
    }
    if v.raw.is_none() || v.path.is_none() {
        return Some("unavailable");
    }
    if v.domain == TreeDomain::Normalized {
        return Some("normalized");
    }
    None
}

struct Contexts<'c> {
    references: ReferenceContext,
    types: TypeContext,
    index: &'c Index,
    predefined: PredefinedContext<'c>,
}

struct Open {
    entry: Guid,
    doc: Arc<EntryData>,
    offsets: Option<HashMap<*const Node, Vec<usize>>>,
    writes: Vec<(Vec<usize>, Node)>,
    edited: bool,
    edited_props: Vec<(Guid, String)>,
}

struct State<'m> {
    mode: Mode,
    model: ModelProps<'m>,
    report: PatchReport,
    open: Option<Open>,
    closed: HashSet<Guid>,
    seen: HashSet<Guid>,
    redecode: HashMap<String, Vec<u8>>,
    edited: HashSet<(Guid, String)>,
}

impl State<'_> {
    fn keep(&mut self, v: &ResolvedProperty<'_>, reason: &str) {
        *self
            .report
            .kept
            .entry(format!("{reason}:{}.{}", v.tag, v.prop.name))
            .or_default() += 1;
    }

    fn unwritable(v: &ResolvedProperty<'_>, what: &str) -> PatchError {
        PatchError::Unwritable {
            object: v.scope.map_or_else(|| v.id.to_string(), str::to_string),
            path: format!("{}.{}", v.tag, v.prop.name),
            what: what.to_string(),
        }
    }

    fn close(&mut self, sink: &mut Sink<'_>) -> Result<()> {
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        let EntryData::Brace(doc) = &*open.doc else {
            return Err(PatchError::Internal(format!(
                "{}: not a brace entry",
                open.entry
            )));
        };
        if open.writes.is_empty() {
            return sink(open.entry, doc, None);
        }
        let mut writes = open.writes;
        writes.sort_by(|a, b| a.0.cmp(&b.0));
        let mut root = doc.root.clone();
        for (path, node) in &writes {
            let slot = root.get_mut(path).ok_or_else(|| {
                PatchError::Internal(format!("{}: write path {path:?} not in tree", open.entry))
            })?;
            *slot = node.clone();
        }
        for (i, (path, node)) in writes.iter().enumerate() {
            let mut expected = node.clone();
            for (inner, value) in writes
                .get(i + 1..)
                .unwrap_or_default()
                .iter()
                .take_while(|(q, _)| q.starts_with(path))
            {
                if inner.len() == path.len() {
                    if value != node {
                        return Err(PatchError::Internal(format!(
                            "{}: two writes at {path:?} disagree",
                            open.entry
                        )));
                    }
                    continue;
                }
                let slot = inner
                    .get(path.len()..)
                    .and_then(|rest| expected.get_mut(rest))
                    .ok_or_else(|| {
                        PatchError::Internal(format!(
                            "{}: overlapping writes disagree at {inner:?}",
                            open.entry
                        ))
                    })?;
                *slot = value.clone();
            }
            if root.get(path) != Some(&expected) {
                return Err(PatchError::Internal(format!(
                    "{}: overlapping writes disagree at {path:?}",
                    open.entry
                )));
            }
        }
        if !open.edited_props.is_empty() {
            self.report
                .edited
                .insert(open.entry, open.edited_props.clone());
        }
        if open.edited {
            self.redecode
                .insert(open.entry.to_string(), stored_bytes(doc, root.clone())?);
        }
        sink(open.entry, doc, Some(root))
    }

    fn write(
        &mut self,
        v: &ResolvedProperty<'_>,
        rel: &[usize],
        node: Node,
        edited: bool,
    ) -> Result<()> {
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| PatchError::Internal("write outside an entry".into()))?;
        let EntryData::Brace(doc) = &*open.doc else {
            return Err(PatchError::Internal(format!(
                "{}: not a brace entry",
                open.entry
            )));
        };
        let offsets = open.offsets.get_or_insert_with(|| offsets(&doc.root));
        let base = offsets.get(&std::ptr::from_ref(v.root)).ok_or_else(|| {
            PatchError::Internal(format!(
                "{}: {}.{}: emission root not in the entry tree",
                open.entry, v.tag, v.prop.name
            ))
        })?;
        let mut path = base.clone();
        path.extend_from_slice(rel);
        open.writes.push((path, node));
        if edited {
            open.edited = true;
            let key = (v.id, v.prop.name.clone());
            if !open.edited_props.contains(&key) {
                open.edited_props.push(key.clone());
            }
            self.edited.insert(key);
        }
        self.report.written += 1;
        Ok(())
    }

    fn accept(
        &mut self,
        v: &ResolvedProperty<'_>,
        cx: &Contexts<'_>,
        entries: &Entries,
        sink: &mut Sink<'_>,
    ) -> Result<()> {
        if self.open.as_ref().map(|o| o.entry) != Some(v.entry) {
            self.close(sink)?;
            if !self.closed.insert(v.entry) {
                return Err(PatchError::Internal(format!(
                    "{}: entry reappeared after it was closed",
                    v.entry
                )));
            }
            let doc = entries
                .doc(&v.entry.to_string())
                .map_err(PatchError::Entries)?
                .ok_or_else(|| PatchError::Internal(format!("{}: no brace entry", v.entry)))?;
            self.open = Some(Open {
                entry: v.entry,
                doc,
                offsets: None,
                writes: Vec::new(),
                edited: false,
                edited_props: Vec::new(),
            });
            self.report.entries += 1;
        }
        self.seen.insert(v.id);
        let value = match self.model.get(&v.id) {
            Some(props) => model_value(props, v.id, &v.prop.name)?,
            None => None,
        };
        let Some(value) = value else {
            return Err(Self::unwritable(v, "nomodel"));
        };
        let edited = value != &v.prop.value;
        if self.mode == Mode::KeepEqual && !edited {
            self.report.kept_equal += 1;
            return Ok(());
        }
        if let Some(reason) = blocked(v) {
            return self.kept_or_refused(v, reason, edited);
        }
        let (Some(raw), Some(path)) = (v.raw, v.path) else {
            return Err(PatchError::Internal(
                "unblocked property without a source".into(),
            ));
        };
        let encoded = encode_property(
            v,
            value,
            raw,
            &cx.references,
            &cx.types,
            cx.index,
            &cx.predefined,
        )
        .map_err(PatchError::Comparison)?;
        let reason = match encoded {
            Encoded::Node(node) => return self.write(v, path, node, edited),
            Encoded::Pair { module, method } => {
                let path2 = v
                    .path2
                    .ok_or_else(|| PatchError::Internal("Method pair without path2".into()))?;
                self.write(v, path, module, edited)?;
                return self.write(v, path2, method, edited);
            }
            Encoded::Refused => "refused",
            Encoded::NoInverse => "no_inverse",
            Encoded::Secondary => "secondary",
        };
        self.kept_or_refused(v, reason, edited)
    }

    fn kept_or_refused(
        &mut self,
        v: &ResolvedProperty<'_>,
        reason: &str,
        edited: bool,
    ) -> Result<()> {
        if edited {
            return Err(Self::unwritable(v, reason));
        }
        self.keep(v, reason);
        Ok(())
    }
}

pub(crate) fn patch_entries(
    cfg: &Configuration,
    src: &dyn EntrySource,
    mode: Mode,
    sink: &mut Sink<'_>,
) -> Result<PatchReport> {
    let entries = &Entries::new(src).map_err(PatchError::Entries)?;
    let schema = mdschema::schema()
        .map_err(|_| PatchError::Comparison(ComparisonError::InvalidTypeContext))?;
    let references = ReferenceContext::new(&cfg.index)
        .map_err(|_| PatchError::Comparison(ComparisonError::AmbiguousReferenceIndex))?;
    let types = TypeContext::new(&cfg.index, schema)
        .map_err(|_| PatchError::Comparison(ComparisonError::InvalidTypeContext))?;
    let tables = crate::predefined::predefined_tables(entries, &cfg.index);
    let cx = Contexts {
        references,
        types,
        index: &cfg.index,
        predefined: PredefinedContext::new(&cfg.predef, &tables),
    };
    let mut state = State {
        mode,
        model: model_props(cfg)?,
        report: PatchReport::default(),
        open: None,
        closed: HashSet::new(),
        seen: HashSet::new(),
        redecode: HashMap::new(),
        edited: HashSet::new(),
    };
    let mut fatal = None;
    decode::visit_properties(cfg, entries, &mut |v| {
        if fatal.is_none()
            && let Err(error) = state.accept(&v, &cx, entries, sink)
        {
            fatal = Some(error);
        }
    })
    .map_err(PatchError::Entries)?;
    if let Some(error) = fatal {
        return Err(error);
    }
    state.close(sink)?;
    if !state.redecode.is_empty() {
        verify_redecode(cfg, src, std::mem::take(&mut state.redecode), &state.edited)?;
    }
    state.report.unvisited_model = state
        .model
        .iter()
        .filter(|(id, props)| !props.is_empty() && !state.seen.contains(id))
        .count();
    Ok(state.report)
}

pub(crate) fn verify_redecode(
    cfg: &Configuration,
    src: &dyn EntrySource,
    patched: HashMap<String, Vec<u8>>,
    edited: &HashSet<(Guid, String)>,
) -> Result<()> {
    let model = model_props(cfg)?;
    let overlay = Overlay { base: src, patched };
    let entries = Entries::new(&overlay).map_err(PatchError::Entries)?;
    let mut missing = edited.clone();
    let mut fatal = None;
    decode::visit_properties(cfg, &entries, &mut |v| {
        if fatal.is_some() || !overlay.patched.contains_key(&v.entry.to_string()) {
            return;
        }
        missing.remove(&(v.id, v.prop.name.clone()));
        let value = match model.get(&v.id) {
            Some(props) => model_value(props, v.id, &v.prop.name),
            None => Ok(None),
        };
        match value {
            Ok(Some(value)) if value != &v.prop.value => {
                fatal = Some(State::unwritable(&v, "re-decode differs"));
            }
            Ok(_) => {}
            Err(error) => fatal = Some(error),
        }
    })
    .map_err(PatchError::Entries)?;
    if let Some(error) = fatal {
        return Err(error);
    }
    if let Some((id, name)) = missing.into_iter().next() {
        return Err(PatchError::Unwritable {
            object: id.to_string(),
            path: name,
            what: "re-decode lost".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CheckReport {
    pub patch: PatchReport,
    pub equal_tree: usize,
    pub equal_inflated: usize,
    pub equal_stored: usize,
    pub first_differences: Vec<String>,
    pub per_entry: HashMap<Guid, Option<String>>,
}

impl CheckReport {
    pub(crate) fn differ(&self) -> usize {
        let n = self.patch.entries;
        (n - self.equal_tree) + (n - self.equal_inflated) + (n - self.equal_stored)
    }

    pub(crate) fn table(&self) -> String {
        let mut out = format!(
            "entries={} equal_tree={} equal_inflated={} equal_stored={} written={} kept_equal={} unvisited_model={}\n",
            self.patch.entries,
            self.equal_tree,
            self.equal_inflated,
            self.equal_stored,
            self.patch.written,
            self.patch.kept_equal,
            self.patch.unvisited_model,
        );
        for (key, count) in &self.patch.kept {
            out.push_str(&format!("kept {key} {count}\n"));
        }
        out
    }
}

pub(crate) fn first_difference(a: &Node, b: &Node) -> Vec<usize> {
    let mut path = Vec::new();
    let (mut a, mut b) = (a, b);
    while let (Some(x), Some(y)) = (a.as_list(), b.as_list()) {
        if x.len() != y.len() {
            break;
        }
        let Some((i, (p, q))) = x.iter().zip(y).enumerate().find(|(_, (p, q))| p != q) else {
            break;
        };
        path.push(i);
        (a, b) = (p, q);
    }
    path
}

const FIRST_DIFFERENCES: usize = 20;

pub(crate) fn check_object_entries(
    cfg: &Configuration,
    src: &dyn EntrySource,
) -> Result<CheckReport> {
    let mut out = CheckReport::default();
    let note = |out: &mut CheckReport, s: String| {
        if out.first_differences.len() < FIRST_DIFFERENCES {
            out.first_differences.push(s);
        }
    };
    let patch = patch_entries(cfg, src, Mode::Strict, &mut |entry, doc, patched| {
        let name = entry.to_string();
        let mut first: Option<String> = None;
        let patched_doc;
        let current = match patched {
            Some(root) => {
                if root == doc.root {
                    out.equal_tree += 1;
                } else {
                    let at = first_difference(&doc.root, &root);
                    first.get_or_insert_with(|| format!("tree differs at {at:?}"));
                    note(&mut out, format!("{name}: tree differs at {at:?}"));
                }
                patched_doc = Doc {
                    root,
                    bom: doc.bom,
                    lead: doc.lead.clone(),
                    trailer: doc.trailer.clone(),
                };
                &patched_doc
            }
            None => {
                out.equal_tree += 1;
                doc
            }
        };
        let stored = src
            .stored(&name)
            .map_err(PatchError::Entries)?
            .ok_or_else(|| PatchError::Internal(format!("{name}: no stored bytes")))?;
        let inflated = zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT)
            .ok_or_else(|| PatchError::Internal(format!("{name}: stored bytes do not inflate")))?;
        let text = brace::write_doc(current);
        if text == inflated {
            out.equal_inflated += 1;
        } else {
            first.get_or_insert_with(|| "text differs from the inflated bytes".into());
            note(
                &mut out,
                format!("{name}: text differs from the inflated bytes"),
            );
        }
        let deflated = zraw::deflate(&text, zraw::LEVEL_1C)
            .map_err(|e| PatchError::Internal(format!("{name}: deflate: {e}")))?;
        if deflated == stored {
            out.equal_stored += 1;
        } else {
            first.get_or_insert_with(|| "deflate differs from the stored bytes".into());
            note(
                &mut out,
                format!("{name}: deflate differs from the stored bytes"),
            );
        }
        out.per_entry.insert(entry, first);
        Ok(())
    })?;
    out.patch = patch;
    Ok(out)
}
