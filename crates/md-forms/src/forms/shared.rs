use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::tables::{PropRef, StringsRef, TableView};
use brace::{Guid, Node};
pub use mdschema::forms::FormCompat;
use mdschema::forms::{FormTables, KindStdAttrs};
use mdschema::{ChildSpec, KindSpec};

use super::query::STD_RU;
use crate::Configuration;
use crate::entries::EntryData;
use crate::error::Result;
use crate::index::MAX_NESTING;
use crate::load::LoadCtx;
use crate::objects::path::{find_collection, find_header};

const V85_MODES: [&str; 2] = ["5", "6"];

const BY_NAME_TAGS: [&str; 7] = [
    "Attribute",
    "TabularSection",
    "Dimension",
    "Resource",
    "AccountingFlag",
    "ExtDimensionAccountingFlag",
    "AddressingAttribute",
];

pub(super) enum TableOwner {
    Native(Arc<FormTables>),
    Embedded(bool, FormCompat),
}

impl TableOwner {
    fn view(&self) -> TableView<'_> {
        match self {
            Self::Native(value) => TableView::native(value),
            Self::Embedded(v85, compat) => TableView::embedded_mode(*v85, *compat),
        }
    }
}

#[doc(hidden)]
pub struct LegacyShared {
    pub v85: bool,
    pub compat: FormCompat,
    pub(super) tables: TableOwner,
    pub names_order: Vec<Guid>,
    pub child_patterns: HashMap<Guid, Node>,
    main_fields: HashMap<String, BTreeSet<String>>,
    common_fields: BTreeSet<String>,
    children: HashMap<String, Vec<Guid>>,
    common_attributes: Vec<Guid>,
    constants: Vec<Guid>,
    by_name: HashMap<String, Vec<Node>>,
    std: &'static [KindStdAttrs],
    props: HashMap<String, HashMap<String, usize>>,
}

impl std::fmt::Debug for LegacyShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("v85", &self.v85)
            .field("compat", &self.compat)
            .field("names", &self.names_order.len())
            .finish_non_exhaustive()
    }
}

impl LegacyShared {
    pub(crate) fn main_table_fields(&self, full: &str) -> BTreeSet<String> {
        let mut r: BTreeSet<String> = self.main_fields.get(full).cloned().unwrap_or_default();
        r.extend(self.std_fields(full));
        r.extend(self.common_fields.iter().cloned());
        r
    }

    pub(crate) fn std_fields(&self, full: &str) -> BTreeSet<String> {
        std_with_ru(&self.std_values(full.split('.').next().unwrap_or_default()))
    }

    fn std_kind(&self, kind: &str) -> &[(String, String)] {
        self.std
            .iter()
            .find(|(k, _)| k == kind)
            .map_or(&[], |(_, m)| m.as_slice())
    }

    pub(crate) fn std_values(&self, kind: &str) -> BTreeSet<String> {
        self.std_kind(kind).iter().map(|(_, n)| n.clone()).collect()
    }

    pub(crate) fn std_ids(&self, kind: &str) -> BTreeSet<String> {
        self.std_kind(kind).iter().map(|(i, _)| i.clone()).collect()
    }

    pub(crate) fn md_children(&self, full: &str) -> &[Guid] {
        self.children.get(full).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn common_attributes(&self) -> &[Guid] {
        &self.common_attributes
    }

    pub(crate) fn constants(&self) -> &[Guid] {
        &self.constants
    }

    pub(crate) fn by_name(&self, name: &str) -> &[Node] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    pub(super) fn tables(&self) -> TableView<'_> {
        self.tables.view()
    }

    pub(super) fn prop(&self, ctx: &str, prop: &str) -> Option<PropRef<'_>> {
        let i = *self.props.get(ctx)?.get(prop)?;
        self.tables().learned.get(ctx)?.props.get(i).map(|(_, e)| e)
    }
}

#[derive(Debug)]
#[doc(hidden)]
pub enum Shared {
    Legacy(LegacyShared),
    Managed(crate::startup::forms_shared::ManagedShared),
}
impl md_model::CodecState for Shared {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same(&self, other: &dyn md_model::CodecState) -> bool {
        other.as_any().is::<Shared>()
    }
}
impl Shared {
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn new(
        v85: bool,
        tables: Arc<FormTables>,
        std: &'static [KindStdAttrs],
        names_order: Vec<Guid>,
        child_patterns: HashMap<Guid, Node>,
        names: &HashMap<Guid, String>,
    ) -> Self {
        Self::with_tables(
            v85,
            TableOwner::Native(tables),
            std,
            names_order,
            child_patterns,
            names,
        )
    }
    fn with_tables(
        v85: bool,
        tables: TableOwner,
        std: &'static [KindStdAttrs],
        names_order: Vec<Guid>,
        child_patterns: HashMap<Guid, Node>,
        names: &HashMap<Guid, String>,
    ) -> Self {
        Self::Legacy(LegacyShared::new(
            v85,
            tables,
            std,
            names_order,
            child_patterns,
            names,
        ))
    }
    pub fn from_resolution(
        v85: bool,
        compat: FormCompat,
        names_order: Vec<Guid>,
        child_patterns: HashMap<Guid, Node>,
        facts: &dyn super::FormSettingsLookup,
    ) -> Result<Self> {
        Ok(Self::Legacy(LegacyShared::with_tables(
            v85,
            compat,
            TableOwner::Embedded(v85, compat),
            mdschema::forms::std_attrs()?,
            names_order,
            child_patterns,
            |id| facts.name(id),
        )))
    }
    pub fn v85(&self) -> bool {
        match self {
            Self::Legacy(v) => v.v85,
            Self::Managed(v) => v.v85(),
        }
    }
    pub fn managed_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        match self {
            Self::Legacy(_) => None,
            Self::Managed(v) => Some(v.memory_budget()),
        }
    }
    pub fn child_patterns(&self) -> impl Iterator<Item = (&Guid, &Node)> {
        let native = match self {
            Self::Legacy(v) => Some(v),
            Self::Managed(_) => None,
        };
        let managed = match self {
            Self::Managed(v) => Some(v),
            Self::Legacy(_) => None,
        };
        native
            .into_iter()
            .flat_map(|v| v.child_patterns.iter())
            .chain(managed.into_iter().flat_map(|v| v.child_patterns()))
    }
    pub fn names_order(&self) -> &[Guid] {
        match self {
            Self::Legacy(v) => &v.names_order,
            Self::Managed(v) => v.names_order(),
        }
    }
    pub(crate) fn child_pattern(&self, id: &Guid) -> Option<&Node> {
        match self {
            Self::Legacy(v) => v.child_patterns.get(id),
            Self::Managed(v) => v.child_pattern(id),
        }
    }
    pub(crate) fn compat(&self) -> FormCompat {
        match self {
            Self::Legacy(v) => v.compat,
            Self::Managed(_) => FormCompat::Base,
        }
    }
    pub(crate) fn tables(&self) -> TableView<'_> {
        match self {
            Self::Legacy(v) => v.tables(),
            Self::Managed(v) => v.tables(),
        }
    }
    pub(crate) fn order(&self, context: &str) -> StringsRef<'_> {
        self.tables()
            .learned
            .get(context)
            .map_or(StringsRef::Native(&[]), |c| c.order)
    }
    pub(crate) fn prop(&self, context: &str, property: &str) -> Option<PropRef<'_>> {
        match self {
            Self::Legacy(v) => v.prop(context, property),
            Self::Managed(v) => v.prop(context, property),
        }
    }
    pub(crate) fn main_table_fields(&self, full: &str) -> BTreeSet<String> {
        match self {
            Self::Legacy(v) => v.main_table_fields(full),
            Self::Managed(v) => v.main_table_fields(full),
        }
    }

    pub(crate) fn std_fields(&self, full: &str) -> BTreeSet<String> {
        match self {
            Self::Legacy(v) => v.std_fields(full),
            Self::Managed(v) => v.std_fields(full),
        }
    }
    pub(crate) fn std_ids(&self, kind: &str) -> BTreeSet<String> {
        match self {
            Self::Legacy(v) => v.std_ids(kind),
            Self::Managed(v) => v.std_ids(kind),
        }
    }
    pub(crate) fn md_children(&self, full: &str) -> &[Guid] {
        match self {
            Self::Legacy(v) => v.md_children(full),
            Self::Managed(v) => v.md_children(full),
        }
    }
    pub(crate) fn common_attributes(&self) -> &[Guid] {
        match self {
            Self::Legacy(v) => v.common_attributes(),
            Self::Managed(v) => v.common_attributes(),
        }
    }
    pub(crate) fn constants(&self) -> &[Guid] {
        match self {
            Self::Legacy(v) => v.constants(),
            Self::Managed(v) => v.constants(),
        }
    }
    pub(crate) fn by_name(&self, name: &str) -> &[Node] {
        match self {
            Self::Legacy(v) => v.by_name(name),
            Self::Managed(v) => v.by_name(name),
        }
    }
}

pub(crate) fn std_with_ru(std: &BTreeSet<String>) -> BTreeSet<String> {
    let mut r = std.clone();
    r.extend(
        STD_RU
            .iter()
            .filter(|(_, en)| std.contains(*en))
            .map(|(ru, _)| ru.to_string()),
    );
    r
}

fn doc_tree(d: &EntryData) -> Option<&Node> {
    match d {
        EntryData::Brace(doc) => Some(&doc.root),
        _ => None,
    }
}

fn source_doc(cx: &LoadCtx, name: &str) -> Result<Option<Arc<EntryData>>> {
    let result = cx.entries.doc(name);
    if !cx.opts.keep_source {
        cx.entries.forget(name);
    }
    result
}

fn interface_mode(cx: &LoadCtx, cfg: &Configuration) -> String {
    let Ok(Some(data)) = source_doc(cx, &cfg.index.cfg_id.to_string()) else {
        return String::new();
    };
    let Some(body) = doc_tree(&data).and_then(|t| t.get(&[3, 1, 1])) else {
        return String::new();
    };
    match body {
        Node::List(l) => l
            .get(62)
            .and_then(Node::text)
            .map(String::from)
            .unwrap_or_default(),
        Node::Str(s) | Node::Atom(s) => s.chars().nth(62).map(String::from).unwrap_or_default(),
    }
}

pub(crate) fn first_pattern(node: &Node, depth: usize) -> Option<&Node> {
    let l = node.as_list()?;
    if l.first().and_then(Node::text) == Some("Pattern") {
        return Some(node);
    }
    if depth < 6 {
        return l.iter().find_map(|e| first_pattern(e, depth + 1));
    }
    None
}

#[derive(Default)]
struct Walk {
    order: Vec<Guid>,
    seen: HashSet<Guid>,
    child_patterns: HashMap<Guid, Node>,
}

impl Walk {
    fn name(&mut self, u: Guid) {
        if self.seen.insert(u) {
            self.order.push(u);
        }
    }

    fn children(&mut self, node: &Node, children: &[(String, ChildSpec)]) {
        for (class, cs) in children {
            if cs.header.is_none() {
                continue;
            }
            for it in find_collection(node, class).unwrap_or_default() {
                let child = cs
                    .uuid
                    .as_ref()
                    .and_then(|p| it.get(p))
                    .and_then(Node::as_guid);
                if let Some(u) = child {
                    self.name(u);
                    match first_pattern(it, 0) {
                        Some(p) => {
                            self.child_patterns.insert(u, p.clone());
                        }
                        None => {
                            self.child_patterns.remove(&u);
                        }
                    }
                }
                self.children(it, &cs.children);
            }
        }
    }

    fn subobjects(&mut self, cx: &LoadCtx, spec: &KindSpec, kind: &str, root: Guid, data: &Node) {
        let pending = |t: &Node| -> Vec<(bool, Guid)> {
            let mut v = Vec::new();
            for (tag, class) in &spec.subobjects {
                let same = tag == kind;
                if !same && tag != "Form" && tag != "Template" {
                    continue;
                }
                for n in find_collection(t, class).unwrap_or_default() {
                    if let Some(g) = n.as_guid() {
                        v.push((same, g));
                    }
                }
            }
            v.reverse();
            v
        };
        let mut seen: HashSet<Guid> = HashSet::from([root]);
        let mut stack: Vec<(usize, Vec<(bool, Guid)>)> = vec![(0, pending(data))];
        while let Some((depth, todo)) = stack.last_mut() {
            let Some((same, su)) = todo.pop() else {
                stack.pop();
                continue;
            };
            let entry = su.to_string();
            let Ok(Some(sdata)) = source_doc(cx, &entry) else {
                continue;
            };
            let Some(st) = doc_tree(&sdata) else { continue };
            if same {
                if !seen.insert(su) {
                    continue;
                }
                let depth = *depth + 1;
                if depth > MAX_NESTING {
                    continue;
                }
                self.name(su);
                let todo = pending(st);
                stack.push((depth, todo));
                continue;
            }
            if find_header(st, &entry).is_some() {
                self.name(su);
            }
        }
    }
}

pub fn form_compat(compatibility_mode: u32, v85: bool) -> FormCompat {
    match (compatibility_mode, v85) {
        (80312, false) => FormCompat::V8_3_12,
        (80501, false) => FormCompat::V8_5_1,
        _ => FormCompat::Base,
    }
}

#[doc(hidden)]
pub fn build(cx: &LoadCtx, cfg: &Configuration) -> Result<Shared> {
    let v85 = V85_MODES.contains(&interface_mode(cx, cfg).as_str());
    let compat = form_compat(cfg.settings.compatibility_mode.0, v85);
    let tables = if compat == FormCompat::Base
        && cx.opts.private_forms.is_none()
        && cx
            .opts
            .meter
            .as_ref()
            .and_then(|meter| meter.schema_budget())
            .is_some()
    {
        cx.resource().require_embedded()?;
        TableOwner::Embedded(v85, compat)
    } else {
        TableOwner::Native(cx.resource().form_tables_mode(
            v85,
            compat,
            cx.opts.private_forms.as_deref(),
        )?)
    };
    let mut w = Walk::default();
    for &u in &cfg.index.order {
        let Some((kind, _)) = cfg.index.objs.get(&u) else {
            continue;
        };
        w.name(u);
        let Some(spec) = crate::objects::overlay::kind(cx.schema, kind.as_str()) else {
            continue;
        };
        let Ok(Some(data)) = source_doc(cx, &u.to_string()) else {
            continue;
        };
        let Some(t) = doc_tree(&data) else { continue };
        w.children(t, &spec.children);
        w.subobjects(cx, spec, kind.as_str(), u, t);
    }
    let names = &cfg.index.names;
    let names_order: Vec<Guid> = w
        .order
        .into_iter()
        .filter(|u| names.contains_key(u))
        .collect();
    let mut shared = Shared::with_tables(
        v85,
        tables,
        cx.resource().std_attrs()?,
        names_order,
        w.child_patterns,
        names,
    );
    if let Shared::Legacy(l) = &mut shared {
        l.compat = compat;
    }
    Ok(shared)
}

impl LegacyShared {
    fn new(
        v85: bool,
        tables: TableOwner,
        std: &'static [KindStdAttrs],
        names_order: Vec<Guid>,
        child_patterns: HashMap<Guid, Node>,
        names: &HashMap<Guid, String>,
    ) -> LegacyShared {
        Self::with_tables(
            v85,
            FormCompat::Base,
            tables,
            std,
            names_order,
            child_patterns,
            |id| names.get(id).map(String::as_str),
        )
    }

    fn with_tables<'n>(
        v85: bool,
        compat: FormCompat,
        tables: TableOwner,
        std: &'static [KindStdAttrs],
        names_order: Vec<Guid>,
        child_patterns: HashMap<Guid, Node>,
        name: impl Fn(&Guid) -> Option<&'n str>,
    ) -> LegacyShared {
        let mut sh = LegacyShared {
            v85,
            compat,
            tables,
            names_order,
            child_patterns,
            main_fields: HashMap::new(),
            common_fields: BTreeSet::new(),
            children: HashMap::new(),
            common_attributes: Vec::new(),
            constants: Vec::new(),
            by_name: HashMap::new(),
            std,
            props: HashMap::new(),
        };
        for (ctx, c) in sh.tables.view().learned.iter() {
            let m = sh.props.entry(ctx.to_string()).or_default();
            for (i, (p, _)) in c.props.iter().enumerate() {
                m.insert(p.to_string(), i);
            }
        }
        for u in &sh.names_order {
            let Some(n) = name(u) else { continue };
            let p: Vec<&str> = n.split('.').collect();
            match p.as_slice() {
                [k, o, _, c] => {
                    sh.main_fields
                        .entry(format!("{k}.{o}"))
                        .or_default()
                        .insert((*c).to_string());
                }
                ["CommonAttribute", c] => {
                    sh.common_fields.insert((*c).to_string());
                }
                _ => {}
            }
            if p.len() >= 4 {
                let parent = p.get(..p.len().saturating_sub(2)).unwrap_or_default();
                sh.children.entry(parent.join(".")).or_default().push(*u);
                if let [.., tag, name] = p.as_slice()
                    && BY_NAME_TAGS.contains(tag)
                {
                    sh.by_name
                        .entry((*name).to_string())
                        .or_default()
                        .push(Node::list(vec![Node::atom("0"), Node::guid(*u)]));
                }
            }
            match p.as_slice() {
                ["CommonAttribute", _] => sh.common_attributes.push(*u),
                ["Constant", _] => sh.constants.push(*u),
                _ => {}
            }
        }
        for (_, m) in std {
            for (k, nm) in m {
                let c = Node::list(vec![Node::atom(k.clone())]);
                let v = sh.by_name.entry(nm.clone()).or_default();
                if !v.contains(&c) {
                    v.push(c);
                }
            }
        }
        sh
    }
}
