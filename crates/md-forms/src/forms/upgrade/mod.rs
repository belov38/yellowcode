pub(crate) mod canon;
mod created;
mod fields;
mod form;
mod groups;
mod items;
mod records;
pub(crate) mod values;
pub(crate) mod writer_set;

use std::collections::BTreeMap;
use std::fmt;

use brace::Node;
use mdschema::forms_upgrade::{UpgradeTables, upgrade_tables};

use super::decode::{self, DecodedForm};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    NoRule {
        class: String,
        version: String,
    },
    NoRuleKind {
        what: &'static str,
        kind: String,
        version: String,
    },
    Value {
        ty: &'static str,
        version: String,
    },
    Domain {
        op: String,
        value: String,
    },
    Events,
    FormTail,
    FormBag,
    Record(&'static str),
    Navigator,
    IdPassRename,
    Shape(&'static str),
    Table(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NoRule { class, version } => write!(f, "no-rule:{class}/{version}"),
            Refusal::NoRuleKind {
                what,
                kind,
                version,
            } => write!(f, "no-rule:{what}:{kind}/{version}"),
            Refusal::Value { ty, version } => write!(f, "value:{ty}:{version}"),
            Refusal::Domain { op, value } => write!(f, "value:{op}:{value}"),
            Refusal::Events => f.write_str("events"),
            Refusal::FormTail => f.write_str("form-tail"),
            Refusal::FormBag => f.write_str("form-bag"),
            Refusal::Record(what) => write!(f, "record:{what}"),
            Refusal::Navigator => f.write_str("navigator"),
            Refusal::IdPassRename => f.write_str("id-pass:rename"),
            Refusal::Shape(what) => write!(f, "shape:{what}"),
            Refusal::Table(what) => write!(f, "table:{what}"),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats(pub BTreeMap<String, u32>);

impl Stats {
    pub(crate) fn hit(&mut self, key: &str) {
        let n = self.0.entry(key.to_string()).or_insert(0);
        *n = n.saturating_add(1);
    }

    pub(crate) fn hit_owned(&mut self, key: String) {
        let n = self.0.entry(key).or_insert(0);
        *n = n.saturating_add(1);
    }
}

pub(crate) struct Cx<'a> {
    pub tables: &'a UpgradeTables,
    pub stats: &'a mut Stats,
    pub empty_picture: Node,
    defaults: std::collections::HashMap<String, Node>,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(
        tables: &'a UpgradeTables,
        stats: &'a mut Stats,
        empty_picture: Node,
    ) -> Cx<'a> {
        Cx {
            tables,
            stats,
            empty_picture,
            defaults: std::collections::HashMap::new(),
        }
    }

    pub(crate) fn upgrade_value(&mut self, ty: &str, v: &Node, tag: &str) -> Result<Node, Refusal> {
        let r = values::upgrade(ty, v, &self.empty_picture)?;
        if r != *v && !matches!(ty, "Plain" | "Raw") {
            self.stats.hit_owned(format!("value-upgrade:{tag}:{ty}"));
        }
        Ok(r)
    }

    pub(crate) fn parse_default(&mut self, text: &str) -> Result<Node, Refusal> {
        if let Some(n) = self.defaults.get(text) {
            return Ok(n.clone());
        }
        let n = brace::parse(text).map_err(|_| Refusal::Shape("default"))?;
        self.defaults.insert(text.to_string(), n.clone());
        Ok(n)
    }
}

pub(crate) fn translate(d: &DecodedForm, stats: &mut Stats) -> Result<Node, Refusal> {
    if d.layout.serialization.is_4_59() {
        return Ok(d.root.clone());
    }
    let tables = upgrade_tables().map_err(|e| Refusal::Table(e.0))?;
    let empty_picture = brace::parse(
        tables
            .defaults
            .get("EmptyPicture")
            .map_or("", String::as_str),
    )
    .map_err(|_| Refusal::Table("EmptyPicture".into()))?;
    let mut cx = Cx::new(tables, stats, empty_picture);
    cx.stats.hit_owned(format!("form v{}", d.form.version));
    let t = d.root.as_list().ok_or(Refusal::Shape("entry"))?;
    let section = |i: usize| t.get(i).ok_or(Refusal::Shape("entry"));
    let attrs = records::attributes(section(3)?, &mut cx)?;
    let params = section(4)?.clone();
    let commands = records::commands(section(5)?, &mut cx)?;
    let acb = d.acb().ok_or(Refusal::Shape("acb"))?;
    let mut acb = items::translate_item(acb, None, &mut cx)?;
    let mut children = Vec::with_capacity(d.form.children.len());
    for c in &d.form.children {
        children.push(items::translate_item(c, None, &mut cx)?);
    }
    form::id_pass(&mut acb, &mut children, &mut cx)?;
    let form_item = form::translate_form(d, acb, children, &mut cx)?;
    let mut out: Vec<Node> = t.to_vec();
    let put = |out: &mut Vec<Node>, i: usize, n: Node| -> Result<(), Refusal> {
        *out.get_mut(i).ok_or(Refusal::Shape("entry"))? = n;
        Ok(())
    };
    put(&mut out, 0, Node::int(4))?;
    put(&mut out, 1, form_item.to_raw())?;
    put(&mut out, 3, attrs)?;
    put(&mut out, 4, params)?;
    put(&mut out, 5, commands)?;
    Ok(Node::list(out))
}

pub(crate) fn translate_entry(root: &Node) -> Result<(Node, Stats), String> {
    let d = decode::decode_form_checked(root).map_err(|e| format!("decode: {e:?}"))?;
    let mut stats = Stats::default();
    let out = translate(&d, &mut stats).map_err(|e| e.to_string())?;
    Ok((out, stats))
}

pub(crate) fn writer_entry(root: &Node) -> Result<(Node, Stats), String> {
    let d = decode::decode_form_checked(root).map_err(|e| format!("decode: {e:?}"))?;
    let mut stats = Stats::default();
    let out = writer_set::for_writer(&d, &mut stats).map_err(|e| e.to_string())?;
    Ok((out, stats))
}

pub(crate) fn refusal_of(root: &Node) -> Option<String> {
    let d = decode::decode_form(root)?;
    if !d.layout.strict {
        return None;
    }
    let mut stats = Stats::default();
    writer_set::for_writer(&d, &mut stats)
        .err()
        .map(|e| e.to_string())
}

pub(crate) fn at<'a>(body: &'a [Node], i: usize, what: &'static str) -> Result<&'a Node, Refusal> {
    body.get(i).ok_or(Refusal::Shape(what))
}

pub(crate) fn int_at(body: &[Node], i: usize, what: &'static str) -> Result<i64, Refusal> {
    at(body, i, what)?.as_i64().ok_or(Refusal::Shape(what))
}
