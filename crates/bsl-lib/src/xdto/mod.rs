mod lexical;
mod model;
mod platform;
mod xdto_file;
mod xsd;

pub use lexical::{
    LexError, ModelFault, builtin_types, check_lexical, check_package_facets, is_builtin,
};
pub use model::{
    Facets, Form, Model, ObjectType, Package, Property, QName, TypeDef, ValueType, Variety,
    WhiteSpace,
};
pub use platform::{
    load_platform_model, load_resource_model, platform_model, split as split_models,
};
pub use xdto_file::parse_xdto_model;
pub use xsd::{parse_xsd, parse_xsd_with};

use crate::xml::{XmlAttr, XmlError, XmlEvent, XmlReader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdtoParseError {
    pub message: String,
    pub fault: Option<Fault>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    Pattern {
        ty: QName,
        pattern: String,
        reason: String,
    },
}

impl std::fmt::Display for XdtoParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for XdtoParseError {}

pub(crate) fn perr<T>(message: impl Into<String>) -> Result<T, XdtoParseError> {
    Err(XdtoParseError {
        message: message.into(),
        fault: None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlElement {
    pub ns: String,
    pub local: String,
    pub attrs: Vec<XmlAttr>,
    pub children: Vec<XmlNodeOwned>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlNodeOwned {
    Element(XmlElement),
    Text(String),
}

pub const MAX_TREE_DEPTH: usize = 1000;

pub fn read_tree(r: &mut XmlReader) -> Result<XmlElement, XmlError> {
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut lines: Vec<u32> = Vec::new();
    loop {
        let Some(ev) = r.next()? else {
            let (line, col) = r.position();
            let message = match stack.last() {
                Some(open) => format!(
                    "Premature end of data in tag {} line {}",
                    open.local,
                    lines.last().copied().unwrap_or(1)
                ),
                None => "Start tag expected, '<' not found".to_string(),
            };
            return Err(XmlError {
                message,
                line,
                col,
                fatal: true,
            });
        };
        match ev {
            XmlEvent::Start {
                local, ns, attrs, ..
            } => {
                if stack.len() >= MAX_TREE_DEPTH {
                    let (line, col) = r.position();
                    return Err(XmlError {
                        message: format!(
                            "Excessive depth in document: {MAX_TREE_DEPTH} use XML_PARSE_HUGE option"
                        ),
                        line,
                        col,
                        fatal: true,
                    });
                }
                lines.push(r.position().0);
                stack.push(XmlElement {
                    ns,
                    local,
                    attrs,
                    children: Vec::new(),
                });
            }
            XmlEvent::End { .. } => {
                let Some(done) = stack.pop() else {
                    continue;
                };
                lines.pop();
                match stack.last_mut() {
                    Some(parent) => parent.children.push(XmlNodeOwned::Element(done)),
                    None => {
                        finish_document(r)?;
                        return Ok(done);
                    }
                }
            }
            XmlEvent::Text(t) | XmlEvent::CData(t) | XmlEvent::Whitespace(t) => {
                if let Some(top) = stack.last_mut() {
                    match top.children.last_mut() {
                        Some(XmlNodeOwned::Text(prev)) => prev.push_str(&t),
                        _ => top.children.push(XmlNodeOwned::Text(t)),
                    }
                }
            }
            _ => {}
        }
    }
}

fn finish_document(r: &mut XmlReader) -> Result<(), XmlError> {
    loop {
        match r.next()? {
            None => return Ok(()),
            Some(XmlEvent::Whitespace(_) | XmlEvent::Comment(_) | XmlEvent::Pi { .. }) => {}
            Some(_) => {
                let (line, col) = r.position();
                return Err(XmlError {
                    message: "Extra content at the end of the document".into(),
                    line,
                    col,
                    fatal: true,
                });
            }
        }
    }
}

impl XmlElement {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns.is_empty() && a.local == name && a.prefix.is_empty())
            .map(|a| a.value.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &XmlElement> {
        self.children.iter().filter_map(|c| match c {
            XmlNodeOwned::Element(e) => Some(e),
            XmlNodeOwned::Text(_) => None,
        })
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.children {
            if let XmlNodeOwned::Text(t) = c {
                s.push_str(t);
            }
        }
        s
    }
}

#[derive(Default)]
pub(crate) struct Scope {
    marks: Vec<usize>,
    binds: Vec<(String, String)>,
}

impl Scope {
    pub(crate) fn push(&mut self, e: &XmlElement) {
        self.marks.push(self.binds.len());
        for a in &e.attrs {
            if a.qname == "xmlns" {
                self.binds.push((String::new(), a.value.clone()));
            } else if a.prefix == "xmlns" {
                self.binds.push((a.local.clone(), a.value.clone()));
            }
        }
    }

    pub(crate) fn pop(&mut self) {
        if let Some(m) = self.marks.pop() {
            self.binds.truncate(m);
        }
    }

    pub(crate) fn lookup(&self, prefix: &str) -> Option<&str> {
        self.binds
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
    }

    pub(crate) fn qname(&self, value: &str, what: &str) -> Result<QName, XdtoParseError> {
        let v = value.trim();
        if let Some(rest) = v.strip_prefix('{') {
            return match rest.split_once('}') {
                Some((ns, name)) if !name.is_empty() => Ok(QName::new(ns, name)),
                _ => perr(format!("{what}: bad qualified name `{v}`")),
            };
        }
        match v.split_once(':') {
            Some((p, l)) if !p.is_empty() && !l.is_empty() => match self.lookup(p) {
                Some(ns) => Ok(QName::new(ns, l)),
                None => perr(format!("{what}: undeclared prefix `{p}` in `{v}`")),
            },
            None if !v.is_empty() => Ok(QName::new(self.lookup("").unwrap_or(""), v)),
            _ => perr(format!("{what}: bad qualified name `{v}`")),
        }
    }
}
