use super::parse::{Doc, parse_doc};
use crate::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newline {
    CrLf,
    Lf,
}

impl Newline {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Newline::CrLf => "\r\n",
            Newline::Lf => "\n",
        }
    }
}

pub fn to_text(root: &Node) -> String {
    to_text_with(root, Newline::CrLf)
}

pub fn to_text_with(root: &Node, nl: Newline) -> String {
    let mut out = String::new();
    let mut first = true;
    write_node(root, nl.as_str(), &mut out, &mut first);
    out
}

fn push_quoted(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

fn write_node(n: &Node, nl: &str, out: &mut String, first: &mut bool) {
    match n {
        Node::List(items) => {
            if !*first {
                out.push_str(nl);
            }
            *first = false;
            out.push('{');
            let mut prev_list = false;
            for (i, e) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_node(e, nl, out, first);
                prev_list = e.is_list();
            }
            if prev_list {
                out.push_str(nl);
            }
            out.push('}');
        }
        Node::Str(s) => {
            *first = false;
            push_quoted(s, out);
        }
        Node::Atom(s) => {
            *first = false;
            out.push_str(s);
        }
    }
}

pub fn to_compact(root: &Node) -> String {
    let mut out = String::new();
    compact(root, &mut out);
    out
}

fn compact(n: &Node, out: &mut String) {
    match n {
        Node::List(items) => {
            out.push('{');
            for (i, e) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                compact(e, out);
            }
            out.push('}');
        }
        Node::Str(s) => push_quoted(s, out),
        Node::Atom(s) => out.push_str(s),
    }
}

pub fn write_doc(doc: &Doc) -> Vec<u8> {
    let body = to_text(&doc.root);
    let mut out = Vec::with_capacity(3 + doc.lead.len() + body.len() + doc.trailer.len());
    if doc.bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    out.extend_from_slice(doc.lead.as_bytes());
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(doc.trailer.as_bytes());
    out
}

pub fn roundtrips(bytes: &[u8]) -> bool {
    parse_doc(bytes).is_ok_and(|d| write_doc(&d) == bytes)
}
