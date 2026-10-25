use std::sync::{Arc, LazyLock};

use brace::Guid;
use mdschema::forms::{FormTables, form_tables};

use super::fe::{E, prefix_of};
use super::py::b64_decode;
use crate::Configuration;
use crate::xml::{XmlNode, parse_xml};

const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const V8: &str = "http://v8.1c.ru/8.1/data/core";
const XS: &str = "http://www.w3.org/2001/XMLSchema";
const LOCAL_FIXED: &[(&str, &str)] = &[
    ("http://v8.1c.ru/8.2/data/spreadsheet", "mxl"),
    ("http://v8.1c.ru/8.2/data/formatted-document", "fd"),
    ("http://v8.1c.ru/8.3/data/pdf", "pdfdoc"),
    ("http://v8.1c.ru/8.2/managed-application/planner", "pl"),
];

static PLATFORM_TYPES: LazyLock<Option<Arc<FormTables>>> =
    LazyLock::new(|| form_tables(false, None).ok());

fn platform_type(key: &str) -> Option<(String, String, bool)> {
    if key.starts_with('_') {
        return None;
    }
    let arr = PLATFORM_TYPES.as_ref()?.types.get(key)?.as_array()?;
    Some((
        arr.first()?.as_str()?.to_string(),
        arr.get(1)?.as_str()?.to_string(),
        arr.get(2).is_none_or(|v| v.is_null()),
    ))
}

fn style_ref_uuid(text: &str) -> Option<Guid> {
    let t = text.strip_suffix('\n').unwrap_or(text);
    let (n, u) = t.split_once(':')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if u.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    Guid::parse(u)
}

pub(crate) fn style_ref(cfg: Option<&Configuration>, text: &str) -> String {
    if let (Some(cfg), Some(g)) = (cfg, style_ref_uuid(text))
        && let Some((_, short)) = cfg.index.names.get(&g).and_then(|n| n.split_once('.'))
    {
        return format!("style:{short}");
    }
    text.to_string()
}

const LXML_MAX_DEPTH: usize = 256;

fn element_depth(root: &XmlNode) -> usize {
    let mut max = 0;
    let mut stack = vec![(root, 1usize)];
    while let Some((n, d)) = stack.pop() {
        max = max.max(d);
        stack.extend(n.children.iter().map(|c| (c, d + 1)));
    }
    max
}

pub(crate) fn parse_bytes(b: &[u8]) -> Option<XmlNode> {
    let find = |pat: &[u8]| b.windows(pat.len()).position(|w| w == pat);
    let i = find(b"<?xml").or_else(|| find(b"<"))?;
    let root = parse_xml(b.get(i..)?).ok()?;
    (element_depth(&root) <= LXML_MAX_DEPTH).then_some(root)
}

pub(crate) fn b64_xml(atom: &brace::Node) -> Option<XmlNode> {
    b64_xml_text(atom.text()?)
}

pub(crate) fn b64_xml_text(s: &str) -> Option<XmlNode> {
    parse_bytes(&b64_decode(s).ok()?)
}

#[derive(Default)]
struct Dict(Vec<(String, String)>);

impl Dict {
    fn set(&mut self, k: String, v: String) {
        match self.0.iter_mut().find(|(ek, _)| *ek == k) {
            Some(slot) => slot.1 = v,
            None => self.0.push((k, v)),
        }
    }
    fn set_default(&mut self, k: String, v: String) {
        if !self.0.iter().any(|(ek, _)| *ek == k) {
            self.0.push((k, v));
        }
    }
}

fn q(ns: Option<&str>, local: &str, decl: &mut Dict) -> String {
    let Some(ns) = ns else {
        return local.to_string();
    };
    if let Some(p) = prefix_of(ns) {
        return if p.is_empty() {
            local.to_string()
        } else {
            format!("{p}:{local}")
        };
    }
    if let Some((_, p)) = LOCAL_FIXED.iter().find(|(u, _)| *u == ns) {
        decl.set_default(format!("xmlns:{p}"), ns.to_string());
        return format!("{p}:{local}");
    }
    decl.set("xmlns:@".to_string(), ns.to_string());
    format!("@:{local}")
}

#[derive(Clone, Default)]
struct Scope(Vec<(Option<String>, String)>);

impl Scope {
    fn with(&self, el: &XmlNode) -> Scope {
        let mut v = self.0.clone();
        v.extend(el.ns_decls.iter().cloned());
        Scope(v)
    }
    fn get(&self, prefix: Option<&str>) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(p, _)| p.as_deref() == prefix)
            .map(|(_, u)| u.as_str())
            .filter(|u| !u.is_empty())
    }
}

fn split_qname(v: &str) -> (&str, &str) {
    v.rsplit_once(':').unwrap_or(("", v))
}

fn qval(scope: &Scope, v: &str, decl: &mut Dict) -> String {
    let (p, local) = split_qname(v);
    match scope.get(if p.is_empty() { None } else { Some(p) }) {
        Some(ns) => q(Some(ns), local, decl),
        None => v.to_string(),
    }
}

fn is_generated_prefix(p: &str) -> bool {
    let Some(rest) = p.strip_prefix('d') else {
        return false;
    };
    let Some((a, b)) = rest.split_once('p') else {
        return false;
    };
    [a, b]
        .iter()
        .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
}

fn scope_of(ancestors: &[&XmlNode]) -> Scope {
    ancestors.iter().fold(Scope::default(), |s, a| s.with(a))
}

pub(crate) fn conv(
    cfg: Option<&Configuration>,
    el: &XmlNode,
    ancestors: &[&XmlNode],
    tag: Option<&str>,
    ns_override: Option<&str>,
) -> E {
    conv_in(cfg, el, &scope_of(ancestors), tag, ns_override)
}

fn conv_in(
    cfg: Option<&Configuration>,
    el: &XmlNode,
    outer: &Scope,
    tag: Option<&str>,
    ns_override: Option<&str>,
) -> E {
    let scope = outer.with(el);
    let mut decl = Dict::default();
    let ns = ns_override.or(el.ns.as_deref());
    let local = el.local.as_str();
    if ns == Some(V8)
        && local == "TypeId"
        && el.children.is_empty()
        && let Some(cfg) = cfg
    {
        let key = el.text.as_deref().unwrap_or("").trim();
        let tn = if key.bytes().any(|b| b.is_ascii_uppercase()) {
            None
        } else {
            Guid::parse(key).and_then(|g| cfg.index.types.get(&g))
        };
        if let Some(tn) = tn {
            let head = tn.split('.').next().unwrap_or("");
            if head != "DefinedType" && head != "Characteristic" {
                return E::new("v8:Type", Some(format!("cfg:{tn}")), vec![], vec![]);
            }
        } else if let Some((kind, name, no_ns)) = platform_type(key)
            && name.starts_with("cfg:")
            && no_ns
        {
            return E::new(format!("v8:{kind}"), Some(name), vec![], vec![]);
        }
    }
    let name = match tag {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => q(ns, local, &mut decl),
    };
    let mut attrs = Dict::default();
    let mut tval: Option<(Option<String>, String)> = None;
    for a in &el.attrs {
        let mut v = a.value.clone();
        if a.ns.as_deref() == Some(XSI) && a.local == "type" {
            let (p, l) = split_qname(&v);
            let tns = scope
                .get(if p.is_empty() { None } else { Some(p) })
                .map(str::to_string);
            tval = Some((tns, l.to_string()));
            v = qval(&scope, &v, &mut decl);
        }
        if a.local == "ref" && a.ns.is_none() {
            v = style_ref(cfg, &v);
        }
        let key = match a.ns.as_deref() {
            Some(kns) => q(Some(kns), &a.local, &mut decl),
            None => a.local.clone(),
        };
        attrs.set(key, v);
    }
    let kids: Vec<E> = el
        .children
        .iter()
        .map(|c| conv_in(cfg, c, &scope, None, None))
        .collect();
    let mut text = None;
    if kids.is_empty() {
        text = el.text.clone();
        if let Some(t) = text.clone() {
            let qname_type = tval.as_ref().is_some_and(|(n, l)| {
                matches!(
                    (n.as_deref(), l.as_str()),
                    (Some(V8), "Type") | (Some(XS), "QName")
                )
            });
            if qname_type {
                text = Some(qval(&scope, &t, &mut decl));
            } else if ns == Some(V8)
                && (local == "Type" || local == "TypeSet")
                && !t.contains(':')
                && scope.get(None).is_some()
            {
                text = Some(qval(&scope, t.trim(), &mut decl));
            } else if tval.is_some() {
                let t = style_ref(cfg, &t);
                let p = t.split_once(':').map(|(p, _)| p);
                text = Some(match p {
                    Some(p) if is_generated_prefix(p) && scope.get(Some(p)).is_some() => {
                        qval(&scope, &t, &mut decl)
                    }
                    _ => t,
                });
            }
        }
    }
    let mut out = decl;
    for (k, v) in attrs.0 {
        out.set(k, v);
    }
    E {
        tag: name,
        attrs: out.0,
        text,
        kids,
        render: None,
    }
}

pub(crate) fn children(
    cfg: Option<&Configuration>,
    el: &XmlNode,
    ancestors: &[&XmlNode],
) -> Vec<E> {
    let scope = scope_of(ancestors).with(el);
    el.children
        .iter()
        .map(|c| conv_in(cfg, c, &scope, None, None))
        .collect()
}
