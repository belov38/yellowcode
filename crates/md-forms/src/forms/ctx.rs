use std::collections::{BTreeSet, HashMap};

use brace::{Guid, Node};
use mdschema::Schema;

use super::FormResolution;
use super::decode::{DecodedForm, FORMID, Item, Rec};
use super::py::{Dict, Py, Raised, py_int, py_len, py_slice, py_str, str_at};
use super::query::result_fields;
use super::shared::Shared;

fn platform_type_name(t: &mdschema::PlatformType) -> TypeName {
    let (name, ns) = match t.legacy_qname() {
        Some(name) => (name, None),
        None => (
            format!("@:{}", t.name().local),
            Some(t.name().namespace_uri.to_owned()),
        ),
    };
    TypeName {
        kind: if t.set { "TypeSet" } else { "Type" }.into(),
        name,
        ns,
    }
}

pub(crate) const IDX: &str = "e67e2953-cebe-4d97-bb93-12b17e6384f8";
pub(crate) const ADDCOL: &str = "5bdad865-f2c5-434b-8041-ba4aad3b6687";
pub(crate) const DL_SPECIAL: [(&str, &str); 1] = [("10000000", "DefaultPicture")];
const SYS_ANY: [(&str, &str); 1] = [("100000000", "RowsCount")];
const SYS_EXTRA: [(&str, &str); 1] = [("v8:StandardPeriod|0", "Variant")];
pub(crate) const SET_CATEGORIES: [&str; 2] = ["DefinedType", "Characteristic"];
const ITEM_CHAIN_MAX: usize = 490;

fn lookup<'t>(table: &'t [(&str, &str)], key: &str) -> Option<&'t str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub(crate) fn bag_dict(v: &Node) -> Py<Dict<String, &Node>> {
    let mut out = Dict::default();
    let Some(l) = v.as_list() else {
        return Ok(out);
    };
    if l.len() < 2 {
        return Ok(out);
    }
    let n = py_int(&py_str(l.get(1).ok_or(Raised)?))?;
    for i in 0..usize::try_from(n).unwrap_or(0) {
        let k = i
            .checked_mul(2)
            .and_then(|x| x.checked_add(2))
            .ok_or(Raised)?;
        let key = py_str(l.get(k).ok_or(Raised)?).into_owned();
        let val = l.get(k.checked_add(1).ok_or(Raised)?).ok_or(Raised)?;
        out.insert(key, val);
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub(crate) struct Attr<'a> {
    pub rec: &'a Rec,
    pub id: String,
    pub name: &'a str,
    pub pattern: &'a Node,
    pub cols: Dict<String, &'a Rec>,
    pub settings: Dict<String, &'a Node>,
    pub dl_fields: Dict<String, String>,
    pub secondary: HashMap<String, String>,
    qf: Option<BTreeSet<String>>,
}

impl<'a> Attr<'a> {
    fn new(rec: &'a Rec) -> Py<Attr<'a>> {
        let id = rec
            .raw
            .get(&[1, 0])
            .and_then(Node::text)
            .map_or_else(|| rec.id.unwrap_or_default().to_string(), String::from);
        let mut cols = Dict::default();
        for c in &rec.children {
            let cid = c
                .raw
                .at(1)
                .and_then(Node::text)
                .map_or_else(|| c.id.unwrap_or_default().to_string(), String::from);
            cols.insert(cid, c);
        }
        let settings = match rec.body.get(14) {
            Some(b) => bag_dict(b)?,
            None => Dict::default(),
        };
        let mut dl_fields = Dict::default();
        let mut secondary = HashMap::new();
        let mut i = 0usize;
        while let Some(v) = settings.get(&format!("FieldsMapItemId{i}")) {
            let fid = str_at(v, 1)?.into_owned();
            if let Some(nm) = settings.get(&format!("FieldsMapItemName{i}")) {
                let nm = str_at(nm, 1)?.into_owned();
                dl_fields.insert(fid, nm.clone());
                if let Some(sn) = settings.get(&format!("FieldsMapItemSecondaryName{i}")) {
                    secondary.insert(nm, str_at(sn, 1)?.into_owned());
                }
            }
            i = i.saturating_add(1);
        }
        Ok(Attr {
            rec,
            id,
            name: &rec.name,
            pattern: rec.body.get(5).ok_or(Raised)?,
            cols,
            settings,
            dl_fields,
            secondary,
            qf: None,
        })
    }

    fn query_fields(&mut self) -> Py<&BTreeSet<String>> {
        if self.qf.is_none() {
            let q = match self.settings.get("QueryText") {
                Some(v) => str_at(v, 1)?.into_owned(),
                None => String::new(),
            };
            self.qf = Some(result_fields(&q));
        }
        self.qf.as_ref().ok_or(Raised)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Attr,
    Unknown,
    Col,
    Std,
    Sys,
    Ts,
    Md,
    Dl,
}

impl NodeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NodeKind::Attr => "attr",
            NodeKind::Unknown => "unknown",
            NodeKind::Col => "col",
            NodeKind::Std => "std",
            NodeKind::Sys => "sys",
            NodeKind::Ts => "ts",
            NodeKind::Md => "md",
            NodeKind::Dl => "dl",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PathNode {
    pub kind: NodeKind,
    pub attr: Option<usize>,
    pub md: Option<String>,
    pub tname: Option<String>,
    pub cols: Option<usize>,
    pub chain: String,
    pub apath: String,
}

impl PathNode {
    fn new(kind: NodeKind, chain: String, apath: String) -> PathNode {
        PathNode {
            kind,
            attr: None,
            md: None,
            tname: None,
            cols: None,
            chain,
            apath,
        }
    }

    fn with_tname(mut self, tname: Option<String>) -> PathNode {
        self.tname = tname;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeName {
    pub kind: String,
    pub name: String,
    pub ns: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AddCol<'a> {
    pub name: String,
    pub body: &'a [Node],
    pub rec: Rec,
}

#[derive(Debug, Clone)]
pub(crate) struct AddCols<'a> {
    pub cols: Dict<String, AddCol<'a>>,
}

pub(crate) struct FormInfo<'a> {
    cfg: &'a dyn FormResolution,
    shared: &'a Shared,
    schema: &'static Schema,
    pub form: &'a DecodedForm,
    #[allow(dead_code)]
    pub owner: Option<&'a str>,
    #[allow(dead_code)]
    pub owner_kind: Option<&'a str>,
    pub attrs: Vec<Attr<'a>>,
    attr_by_id: HashMap<String, usize>,
    items_by_id: Dict<String, &'a Item>,
    pub addcols: Dict<Option<String>, AddCols<'a>>,
    pub ca: Option<&'a Node>,
    item_nodes: Option<HashMap<String, Option<PathNode>>>,
    item_depth: usize,
    missing: bool,
    raised: bool,
}

pub(crate) fn item_key(it: &Item) -> String {
    it.id_text.clone()
}

pub(crate) fn md_name<'c>(cfg: &'c dyn FormResolution, s: &str) -> Option<&'c str> {
    cfg.name(&Guid::parse(s)?)
}

fn last_part(full: &str) -> &str {
    full.rsplit('.').next().unwrap_or(full)
}

fn rpartition(k: &str) -> (&str, &str) {
    k.rsplit_once('|').unwrap_or(("", k))
}

impl<'a> FormInfo<'a> {
    pub(crate) fn new(
        cfg: &'a dyn FormResolution,
        shared: &'a Shared,
        df: &'a DecodedForm,
        owner: Option<&'a str>,
    ) -> Option<FormInfo<'a>> {
        let schema = mdschema::schema().ok()?;
        let attrs: Vec<Attr<'a>> = df.attrs.iter().map(Attr::new).collect::<Py<_>>().ok()?;
        let mut attr_by_id = HashMap::new();
        for (i, a) in attrs.iter().enumerate() {
            attr_by_id.insert(a.id.clone(), i);
        }
        let mut items_by_id = Dict::default();
        for it in df.items() {
            items_by_id.insert(item_key(it), it);
        }
        let mut fi = FormInfo {
            cfg,
            shared,
            schema,
            form: df,
            owner,
            owner_kind: owner
                .filter(|o| !o.is_empty())
                .and_then(|o| o.split('.').next()),
            attrs,
            attr_by_id,
            items_by_id,
            addcols: Dict::default(),
            ca: None,
            item_nodes: None,
            item_depth: 0,
            missing: false,
            raised: false,
        };
        fi.init_addcols(df.root.at(3)?).ok()?;
        fi.item_nodes = Some(HashMap::new());
        Some(fi)
    }

    fn init_addcols(&mut self, a: &'a Node) -> Py<()> {
        let al = a.as_list().ok_or(Raised)?;
        let k = py_int(&str_at(a, 1)?)?;
        let tail = py_slice(al, k.saturating_add(2), None);
        let Some(t0) = tail.first() else {
            return Ok(());
        };
        let n = py_int(&py_str(t0))?;
        for g in py_slice(tail, 1, Some(n.saturating_add(1))) {
            let gl = g.as_list().ok_or(Raised)?;
            let p = self.path_py(gl.get(1).ok_or(Raised)?)?;
            let m = py_int(&str_at(g, 2)?)?;
            let mut cols = Dict::default();
            for c in py_slice(gl, 3, Some(m.saturating_add(3))) {
                let cl = c.as_list().ok_or(Raised)?;
                let id = str_at(c, 1)?.into_owned();
                let name = str_at(c, 3)?.into_owned();
                let rec = Rec::new("Column", c, cl.to_vec(), id.parse().ok(), name.clone());
                cols.insert(
                    id,
                    AddCol {
                        name,
                        body: cl,
                        rec,
                    },
                );
            }
            self.addcols.insert(p, AddCols { cols });
        }
        let rest = py_slice(tail, n.saturating_add(1), None);
        self.ca = if rest.len() > 1 { rest.last() } else { None };
        Ok(())
    }

    pub(crate) fn raised(&self) -> bool {
        self.raised
    }

    #[allow(dead_code)]
    pub(crate) fn missing(&self) -> bool {
        self.missing
    }

    fn check<T>(&mut self, r: Py<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(Raised) => {
                self.raised = true;
                None
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn attr(&self, id: i64) -> Option<&Attr<'a>> {
        self.attr_by_id
            .get(&id.to_string())
            .and_then(|&i| self.attrs.get(i))
    }

    pub(crate) fn attr_by_key(&self, id: &str) -> Option<usize> {
        self.attr_by_id.get(id).copied()
    }

    pub(crate) fn attr_index(&self, name: &str) -> Option<usize> {
        self.attrs.iter().position(|a| a.name == name)
    }

    pub(crate) fn item(&self, id: &str) -> Option<&'a Item> {
        self.items_by_id.get(id).copied()
    }

    #[allow(dead_code)]
    pub(crate) fn type_names(&mut self, pat: &Node) -> Option<Vec<TypeName>> {
        let r = self.type_names_py(pat);
        self.check(r).flatten()
    }

    pub(crate) fn type_names_py(&self, pat: &Node) -> Py<Option<Vec<TypeName>>> {
        let Some(items) = pat.as_list() else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for it in items.get(1..).unwrap_or_default() {
            let code = str_at(it, 0)?;
            if code != "#" {
                let name = match code.as_ref() {
                    "B" => "xs:boolean".to_string(),
                    "S" => "xs:string".to_string(),
                    "N" => "xs:decimal".to_string(),
                    "D" => "xs:dateTime".to_string(),
                    "R" => "xs:base64Binary".to_string(),
                    "L" => "v8:Null".to_string(),
                    other => format!("?{other}"),
                };
                out.push(TypeName {
                    kind: "Type".into(),
                    name,
                    ns: None,
                });
                continue;
            }
            let g = str_at(it, 1)?;
            if let Some(n) = Guid::parse(&g).and_then(|u| self.cfg.type_name(&u)) {
                let set = SET_CATEGORIES.contains(&n.split('.').next().unwrap_or_default());
                out.push(TypeName {
                    kind: if set { "TypeSet" } else { "Type" }.into(),
                    name: format!("cfg:{n}"),
                    ns: None,
                });
                continue;
            }
            let ft = self.shared.tables().types.get(g.as_ref()).filter(|v| {
                !(v.is_null()
                    || v.as_bool() == Some(false)
                    || v.as_f64() == Some(0.0)
                    || v.as_str() == Some("")
                    || v.as_array().is_some_and(|a| a.is_empty())
                    || v.as_object().is_some_and(|o| o.is_empty()))
            });
            if let Some(t) = ft {
                let a = t.as_array().ok_or(Raised)?;
                let s = |i: usize| a.get(i).and_then(|v| v.as_str()).map(String::from);
                out.push(TypeName {
                    kind: s(0).ok_or(Raised)?,
                    name: s(1).ok_or(Raised)?,
                    ns: s(2),
                });
                continue;
            }
            let Some(t) = self.schema.platform_type(&g) else {
                return Ok(None);
            };
            out.push(platform_type_name(t));
        }
        Ok(Some(out))
    }

    fn single_type(&self, pat: &Node) -> Py<Option<String>> {
        if !pat.is_list() {
            return Ok(None);
        }
        Ok(self
            .type_names_py(pat)?
            .filter(|tn| tn.len() == 1)
            .and_then(|tn| tn.into_iter().next())
            .map(|t| t.name))
    }

    pub(crate) fn attr_tname(&mut self, ai: usize) -> Option<String> {
        let pat = self.attrs.get(ai).map(|a| a.pattern)?;
        let r = self.single_type(pat);
        self.check(r).flatten()
    }

    fn col_tname(&self, body: &[Node]) -> Py<Option<String>> {
        self.single_type(body.get(5).ok_or(Raised)?)
    }

    fn md_type_name(&self, uuid: &str) -> Py<Option<String>> {
        let Some(pat) = Guid::parse(uuid).and_then(|g| self.shared.child_pattern(&g)) else {
            return Ok(None);
        };
        self.single_type(pat)
    }

    fn std_name(&self, node: &PathNode, n: &str) -> Option<String> {
        let key = node_type_key(Some(node));
        let k = format!("{key}|{n}");
        if let Some(nm) = self
            .shared
            .tables()
            .paths
            .get("std")
            .and_then(|m| m.get(k.as_str()))
            .and_then(|v| v.as_str())
        {
            return Some(nm.to_string());
        }
        let kind = kind_of(&key)?;
        self.schema.std_attr(kind, n).map(String::from)
    }

    fn item_node_py(&mut self, item_id: &str) -> Py<Option<PathNode>> {
        let Some(cache) = self.item_nodes.as_mut() else {
            return Err(Raised);
        };
        if let Some(r) = cache.get(item_id) {
            return Ok(r.clone());
        }
        cache.insert(item_id.to_string(), None);
        let mut r = None;
        if let Some(it) = self.items_by_id.get(item_id).copied()
            && it.class == Some("Table")
        {
            let v = it.body.get(11).ok_or(Raised)?;
            if self.item_depth >= ITEM_CHAIN_MAX {
                return Err(Raised);
            }
            let keep = self.missing;
            self.item_depth += 1;
            let res = self.resolve_inner(v);
            self.item_depth -= 1;
            let res = res?;
            self.missing = keep;
            if let Some((_, node)) = res {
                r = node;
            }
        }
        if let Some(cache) = self.item_nodes.as_mut() {
            cache.insert(item_id.to_string(), r.clone());
        }
        Ok(r)
    }

    pub(crate) fn item_node(&mut self, it: &Item) -> Option<PathNode> {
        let r = self.item_node_py(&item_key(it));
        self.check(r).flatten()
    }

    pub(crate) fn path_py(&mut self, v: &Node) -> Py<Option<String>> {
        let r = self.resolve_py(v)?;
        Ok(r.map(|(t, _)| if self.missing { format!("~{t}") } else { t }))
    }

    pub(crate) fn data_path(&mut self, v: &Node) -> Option<String> {
        let r = self.path_py(v);
        self.check(r).flatten()
    }

    pub(crate) fn dl_special_missing(&self, ai: usize) -> Py<bool> {
        let mt = self.attrs.get(ai).and_then(|a| a.settings.get("MainTable"));
        let full = match mt {
            Some(mt) => md_name(self.cfg, &str_at(mt, 2)?),
            None => None,
        };
        Ok(full.is_none_or(|f| {
            matches!(
                f.split('.').next().unwrap_or_default(),
                "Enum" | "FilterCriterion"
            )
        }))
    }

    pub(crate) fn dl_field_missing(&mut self, ai: usize, name: &str) -> Py<bool> {
        let cfg = self.cfg;
        let shared = self.shared;
        let Some(attr) = self.attrs.get_mut(ai) else {
            return Ok(false);
        };
        let full = match attr.settings.get("MainTable") {
            Some(mt) => md_name(cfg, &str_at(mt, 2)?),
            None => None,
        };
        let first = name.split('.').next().unwrap_or_default();
        let manual = match attr.settings.get("ManualQuery") {
            Some(v) => str_at(v, 1)? == "1",
            None => false,
        };
        if manual {
            let sec = attr.secondary.get(name).cloned().unwrap_or_default();
            let qf = attr.query_fields()?;
            if qf.is_empty() {
                return Ok(false);
            }
            if qf.contains(first) || qf.contains(sec.split('.').next().unwrap_or_default()) {
                return Ok(false);
            }
            if full.is_some_and(|f| shared.std_fields(f).contains(first)) {
                return Ok(false);
            }
            return Ok(true);
        }
        let Some(full) = full else {
            return Ok(false);
        };
        Ok(!shared.main_table_fields(full).contains(first))
    }

    fn resolve_py(&mut self, v: &Node) -> Py<Option<(String, Option<PathNode>)>> {
        self.missing = false;
        self.resolve_inner(v)
    }

    pub(crate) fn resolve(&mut self, v: &Node) -> Option<(String, Option<PathNode>)> {
        let r = self.resolve_py(v);
        self.check(r).flatten()
    }

    fn resolve_inner(&mut self, v: &Node) -> Py<Option<(String, Option<PathNode>)>> {
        let Some(l) = v.as_list() else {
            return Ok(None);
        };
        let Some(n0) = l.first().and_then(Node::text) else {
            return Ok(None);
        };
        if n0.is_empty() || !n0.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(None);
        }
        let n = py_int(n0)?;
        let segs = py_slice(l, 1, Some(n.saturating_add(1)));
        let Some((s0, rest)) = segs.split_first() else {
            return Ok(Some((String::new(), None)));
        };
        let (mut text, mut node);
        if py_len(s0) == 2 && str_at(s0, 1)? == FORMID {
            let id = str_at(s0, 0)?.into_owned();
            let Some(it) = self.items_by_id.get(&id).copied() else {
                return Ok(None);
            };
            text = format!("Items.{}.CurrentData", it.name().unwrap_or("None"));
            let base = self
                .item_node_py(&id)?
                .unwrap_or_else(|| PathNode::new(NodeKind::Unknown, String::new(), String::new()));
            node = PathNode {
                chain: text.clone(),
                ..base
            };
        } else if py_len(s0) == 1 {
            let id = str_at(s0, 0)?;
            let Some(&ai) = self.attr_by_id.get(id.as_ref()) else {
                return Ok(None);
            };
            let (name, pat) = match self.attrs.get(ai) {
                Some(a) => (a.name.to_string(), a.pattern),
                None => return Ok(None),
            };
            text = name.clone();
            node = PathNode {
                kind: NodeKind::Attr,
                attr: Some(ai),
                md: None,
                tname: self.single_type(pat)?,
                cols: Some(ai),
                chain: name.clone(),
                apath: name,
            };
        } else {
            return Ok(None);
        }
        for s in rest {
            let Some((t, nd)) = self.step_py(&node, s, &text)? else {
                return Ok(None);
            };
            text = t;
            node = nd;
        }
        Ok(Some((text, Some(node))))
    }

    #[allow(dead_code)]
    pub(crate) fn step(
        &mut self,
        node: &PathNode,
        s: &Node,
        text: &str,
    ) -> Option<(String, PathNode)> {
        let r = self.step_py(node, s, text);
        self.check(r).flatten()
    }

    fn step_py(&mut self, node: &PathNode, s: &Node, text: &str) -> Py<Option<(String, PathNode)>> {
        let chain = |nm: &str| format!("{}.{nm}", node.chain);
        let apath = |nm: &str| format!("{}.{nm}", node.apath);
        let len = py_len(s);
        if len == 2 && str_at(s, 1)? == IDX {
            return Ok(Some((format!("{text}[{}]", str_at(s, 0)?), node.clone())));
        }
        if len == 2 && str_at(s, 1)? == ADDCOL {
            let id = str_at(s, 0)?;
            let Some(c) = self
                .addcols
                .get(&Some(node.apath.clone()))
                .and_then(|g| g.cols.get(id.as_ref()))
            else {
                return Ok(None);
            };
            let (name, body) = (c.name.clone(), c.body);
            let n = PathNode::new(NodeKind::Col, chain(&name), apath(&name))
                .with_tname(self.col_tname(body)?);
            return Ok(Some((format!("{text}.{name}"), n)));
        }
        if len == 2 && str_at(s, 0)? == "101000000" {
            let Some(full) = md_name(self.cfg, &str_at(s, 1)?) else {
                return Ok(None);
            };
            let nm = format!("Total{}", last_part(full));
            let n = PathNode::new(NodeKind::Std, chain(&nm), apath(&nm));
            return Ok(Some((format!("{text}.{nm}"), n)));
        }
        if len == 2 && str_at(s, 0)? == "0" {
            let u = str_at(s, 1)?;
            let Some(full) = md_name(self.cfg, &u) else {
                let key = format!("u|{u}");
                let Some(nm) = self
                    .shared
                    .tables()
                    .paths
                    .get("sys")
                    .and_then(|m| m.get(key.as_str()))
                    .and_then(|v| v.as_str())
                else {
                    return Ok(None);
                };
                let n = PathNode::new(NodeKind::Sys, chain(nm), apath(nm)).with_tname(Some(key));
                return Ok(Some((format!("{text}.{nm}"), n)));
            };
            let nm = last_part(full);
            let parts: Vec<&str> = full.split('.').collect();
            if parts.len() >= 2 && parts.get(parts.len() - 2) == Some(&"TabularSection") {
                let mut n = PathNode::new(NodeKind::Ts, chain(nm), apath(nm))
                    .with_tname(Some("TabularSection".into()));
                n.md = Some(full.to_string());
                return Ok(Some((format!("{text}.{nm}"), n)));
            }
            let mut n = PathNode::new(NodeKind::Md, chain(nm), apath(nm))
                .with_tname(self.md_type_name(&u)?);
            n.md = Some(full.to_string());
            return Ok(Some((format!("{text}.{nm}"), n)));
        }
        if len != 1 {
            return Ok(None);
        }
        let k = str_at(s, 0)?.into_owned();
        let ik = py_int(&k)?;
        if ik < 0 {
            let Some(nm) = self.std_name(node, &k) else {
                return Ok(None);
            };
            let n = PathNode::new(NodeKind::Std, chain(&nm), apath(&nm));
            return Ok(Some((format!("{text}.{nm}"), n)));
        }
        let col = node
            .cols
            .and_then(|ai| self.attrs.get(ai))
            .and_then(|a| a.cols.get(&k).copied());
        if let Some(c) = col {
            let n = PathNode::new(NodeKind::Col, chain(&c.name), apath(&c.name))
                .with_tname(self.col_tname(&c.body)?);
            return Ok(Some((format!("{text}.{}", c.name), n)));
        }
        let special = lookup(&DL_SPECIAL, &k);
        if let Some(ai) = node.attr
            && matches!(node.kind, NodeKind::Attr | NodeKind::Dl)
            && let Some(attr) = self.attrs.get(ai)
            && (!attr.dl_fields.is_empty() || special.is_some())
        {
            let nm = attr
                .dl_fields
                .get(&k)
                .filter(|n| !n.is_empty())
                .cloned()
                .or_else(|| special.map(String::from));
            if let Some(nm) = nm {
                let base = if node.kind == NodeKind::Dl {
                    node.chain.clone()
                } else {
                    text.to_string()
                };
                let miss = if special.is_some() {
                    self.dl_special_missing(ai)?
                } else {
                    self.dl_field_missing(ai, &nm)?
                };
                if miss {
                    self.missing = true;
                }
                let mut n = PathNode::new(NodeKind::Dl, base.clone(), apath(&nm));
                n.attr = Some(ai);
                return Ok(Some((format!("{base}.{nm}"), n)));
            }
        }
        let key = node_type_key(Some(node));
        let kk = format!("{key}|{k}");
        let nm = self
            .shared
            .tables()
            .paths
            .get("sys")
            .and_then(|m| m.get(kk.as_str()))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| lookup(&SYS_EXTRA, &kk).filter(|s| !s.is_empty()))
            .or_else(|| lookup(&SYS_ANY, &k))
            .map(String::from);
        if let Some(nm) = nm {
            let n = PathNode::new(NodeKind::Sys, chain(&nm), apath(&nm))
                .with_tname(Some(format!("{key}/{nm}")));
            return Ok(Some((format!("{text}.{nm}"), n)));
        }
        Ok(None)
    }

    fn candidates(&self, node: &PathNode) -> Vec<Node> {
        let one = |k: &str| Node::list(vec![Node::atom(k)]);
        let two = |a: &str, b: &str| Node::list(vec![Node::atom(a), Node::atom(b)]);
        let mut out = Vec::new();
        if let Some(a) = node.cols.and_then(|ai| self.attrs.get(ai)) {
            out.extend(a.cols.keys().map(|k| one(k)));
        }
        if matches!(node.kind, NodeKind::Attr | NodeKind::Dl)
            && let Some(a) = node.attr.and_then(|ai| self.attrs.get(ai))
        {
            out.extend(a.dl_fields.keys().map(|k| one(k)));
            out.extend(DL_SPECIAL.iter().map(|(k, _)| one(k)));
        }
        let full = md_of_node(node).filter(|f| !f.is_empty());
        if let Some(full) = &full {
            for u in self.shared.md_children(full) {
                let u = u.to_string();
                out.push(two("0", &u));
                out.push(two("101000000", &u));
            }
            if full.matches('.').count() == 1 {
                out.extend(
                    self.shared
                        .common_attributes()
                        .iter()
                        .map(|u| two("0", &u.to_string())),
                );
            }
        }
        if node.tname.as_deref() == Some("cfg:ConstantsSet") {
            out.extend(
                self.shared
                    .constants()
                    .iter()
                    .map(|u| two("0", &u.to_string())),
            );
        }
        let key = node_type_key(Some(node));
        let kind = kind_of(&key).filter(|k| !k.is_empty());
        let mut ids: BTreeSet<String> = kind.map(|k| self.shared.std_ids(k)).unwrap_or_default();
        let section = |name: &str| {
            self.shared
                .tables()
                .paths
                .get(name)
                .and_then(|m| m.as_object())
                .map(|m| m.keys().map(String::from).collect::<Vec<String>>())
                .unwrap_or_default()
        };
        for k in section("std") {
            let (t, n) = rpartition(&k);
            if t == key {
                ids.insert(n.to_string());
            }
        }
        let mut sys_u = Vec::new();
        let sys_keys = section("sys")
            .into_iter()
            .chain(SYS_EXTRA.iter().map(|(k, _)| k.to_string()));
        for k in sys_keys {
            let (t, n) = rpartition(&k);
            if t == key {
                ids.insert(n.to_string());
            } else if t == "u" {
                sys_u.push(two("0", n));
            }
        }
        ids.extend(SYS_ANY.iter().map(|(k, _)| k.to_string()));
        out.extend(ids.iter().map(|n| one(n)));
        out.extend(sys_u);
        for (p, g) in self.addcols.iter() {
            if p.as_deref() == Some(node.apath.as_str()) {
                out.extend(g.cols.keys().map(|k| two(k, ADDCOL)));
            }
        }
        out
    }

    #[allow(dead_code)]
    pub(crate) fn unresolve(&mut self, text: &str) -> Option<Node> {
        let r = self.unresolve_py(text);
        self.check(r).flatten()
    }

    fn unresolve_py(&mut self, text: &str) -> Py<Option<Node>> {
        if text.is_empty() {
            return Ok(Some(Node::list(vec![Node::atom("0")])));
        }
        let text = text.trim_start_matches('~');
        let parts = split_parts(text);
        let mut segs: Vec<Node>;
        let rest: &[String];
        let first = parts.first().map(String::as_str).unwrap_or_default();
        if first == "Items" && parts.len() >= 3 {
            let want = parts.get(1).map(String::as_str);
            let Some(id) = self
                .items_by_id
                .iter()
                .find(|(_, it)| it.name() == want)
                .map(|(k, _)| k.clone())
            else {
                return Ok(None);
            };
            segs = vec![Node::list(vec![Node::atom(id), Node::atom(FORMID)])];
            rest = parts.get(3..).unwrap_or_default();
        } else {
            let (name, ix) = first.split_once('[').unwrap_or((first, ""));
            let Some(a) = self.attrs.iter().find(|a| a.name == name) else {
                return Ok(None);
            };
            segs = vec![Node::list(vec![Node::atom(a.id.clone())])];
            rest = parts.get(1..).unwrap_or_default();
            if !ix.is_empty() {
                segs.push(Node::list(vec![
                    Node::atom(ix.trim_end_matches(']')),
                    Node::atom(IDX),
                ]));
            }
        }
        let mut v = vec![Node::atom(segs.len().to_string())];
        v.extend(segs.iter().cloned());
        let Some((cur, Some(node))) = self.resolve_py(&Node::list(v))? else {
            return Ok(None);
        };
        let Some(out) = self.unresolve_rest(rest, 0, &cur, &node, 0)? else {
            return Ok(None);
        };
        segs.extend(out);
        let mut v = vec![Node::atom(segs.len().to_string())];
        v.extend(segs);
        Ok(Some(Node::list(v)))
    }

    fn unresolve_rest(
        &mut self,
        rest: &[String],
        i: usize,
        cur: &str,
        node: &PathNode,
        depth: usize,
    ) -> Py<Option<Vec<Node>>> {
        if i >= rest.len() {
            return Ok(Some(Vec::new()));
        }
        if depth > 12 {
            return Ok(None);
        }
        let part = rest.get(i).map(String::as_str).unwrap_or_default();
        let mut cands = self.candidates(node);
        cands.extend(
            self.shared
                .by_name(part.split('[').next().unwrap_or_default())
                .iter()
                .cloned(),
        );
        for s in cands {
            let Some((rr, nnode)) = self.step_py(node, &s, cur)? else {
                continue;
            };
            for j in (i + 1..=rest.len()).rev() {
                let want = format!("{cur}.{}", rest.get(i..j).unwrap_or_default().join("."));
                let base = strip_last_idx(&want);
                if rr != base && rr != want {
                    continue;
                }
                let mut segs = vec![s.clone()];
                let mut ncur = rr.clone();
                let tail = rest.get(j - 1).map(String::as_str).unwrap_or_default();
                if let Some(t) = tail.strip_suffix(']') {
                    let (_, k) = t.rsplit_once('[').ok_or(Raised)?;
                    segs.push(Node::list(vec![Node::atom(k), Node::atom(IDX)]));
                    ncur = format!("{ncur}[{k}]");
                }
                if let Some(more) = self.unresolve_rest(rest, j, &ncur, &nnode, depth + 1)? {
                    segs.extend(more);
                    return Ok(Some(segs));
                }
            }
        }
        Ok(None)
    }
}

#[allow(dead_code)]
pub(crate) fn strip_idx(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(p) = rest.find('[') {
        let (head, tail) = rest.split_at(p);
        out.push_str(head);
        let inner = tail.get(1..).unwrap_or_default();
        let digits = inner.find(|c: char| !is_py_digit(c)).unwrap_or(inner.len());
        if digits > 0 && inner.get(digits..).is_some_and(|r| r.starts_with(']')) {
            rest = inner.get(digits + 1..).unwrap_or_default();
        } else {
            out.push('[');
            rest = inner;
        }
    }
    out.push_str(rest);
    out
}

fn is_py_digit(c: char) -> bool {
    c.is_ascii_digit()
}

fn strip_last_idx(want: &str) -> String {
    let (body, nl) = match want.strip_suffix('\n') {
        Some(b) => (b, "\n"),
        None => (want, ""),
    };
    let stripped = body.strip_suffix(']').and_then(|b| {
        let (head, digits) = b.rsplit_once('[')?;
        (!digits.is_empty() && digits.chars().all(is_py_digit)).then_some(head)
    });
    match stripped {
        Some(head) => format!("{head}{nl}"),
        None => want.to_string(),
    }
}

fn split_parts(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c != '.' {
            continue;
        }
        let after = s.get(i + 1..).unwrap_or_default();
        let inside = after
            .chars()
            .find(|&ch| ch == '[' || ch == ']')
            .is_some_and(|ch| ch == ']');
        if !inside {
            out.push(s.get(start..i).unwrap_or_default().to_string());
            start = i + 1;
        }
    }
    out.push(s.get(start..).unwrap_or_default().to_string());
    out
}

pub(crate) fn node_type_key(node: Option<&PathNode>) -> String {
    let Some(node) = node else {
        return "?".into();
    };
    if node.kind == NodeKind::Ts {
        return "TabularSection".into();
    }
    match node.tname.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => general_type(t),
        None => node.kind.as_str().into(),
    }
}

pub(crate) fn general_type(t: &str) -> String {
    if !t.starts_with("cfg:") {
        return t.to_string();
    }
    let (head, rest) = t.split_once('/').unwrap_or((t, ""));
    let head = head.split('.').next().unwrap_or_default();
    if rest.is_empty() {
        head.to_string()
    } else {
        format!("{head}/{rest}")
    }
}

pub(crate) fn kind_of(tname: &str) -> Option<&str> {
    let pref = tname.strip_prefix("cfg:")?.split('.').next()?;
    [
        "Object",
        "Ref",
        "RecordSet",
        "RecordManager",
        "Selection",
        "List",
        "Manager",
    ]
    .iter()
    .find_map(|suf| pref.strip_suffix(suf))
}

fn md_of_node(node: &PathNode) -> Option<String> {
    if node.kind == NodeKind::Ts
        && let Some(md) = node.md.as_deref().filter(|m| !m.is_empty())
    {
        return Some(md.to_string());
    }
    let body = node
        .tname
        .as_deref()
        .unwrap_or_default()
        .strip_prefix("cfg:")?;
    let (pref, rest) = body.split_once('.').unwrap_or((body, ""));
    ["Object", "Ref", "RecordSet", "RecordManager"]
        .iter()
        .find_map(|suf| pref.strip_suffix(suf))
        .map(|k| format!("{k}.{rest}"))
}
