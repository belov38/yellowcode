use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::{MAX_DEPTH, MAX_XML_BYTES, ModelError};

#[derive(Debug, Default, Clone)]
pub(crate) struct XNode {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<XNode>,
}

impl XNode {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    pub fn child(&self, name: &str) -> Option<&XNode> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }
}

fn local(q: &[u8]) -> String {
    let s = String::from_utf8_lossy(q);
    s.rsplit(':').next().unwrap_or_default().to_string()
}

fn open(e: &BytesStart) -> Result<XNode, ModelError> {
    let mut n = XNode {
        name: local(e.local_name().as_ref()),
        ..Default::default()
    };
    for a in e.attributes() {
        let a = a.map_err(|e| ModelError::Xml(e.to_string()))?;
        let v = a
            .unescape_value()
            .map_err(|e| ModelError::Xml(e.to_string()))?;
        n.attrs
            .push((local(a.key.local_name().as_ref()), v.into_owned()));
    }
    Ok(n)
}

pub(crate) fn parse(xml: &str) -> Result<XNode, ModelError> {
    if xml.len() > MAX_XML_BYTES {
        return Err(ModelError::TooLarge);
    }
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut stack: Vec<XNode> = vec![XNode::default()];
    loop {
        let ev = r.read_event().map_err(|e| ModelError::Xml(e.to_string()))?;
        match ev {
            Event::Start(e) => {
                if stack.len() > MAX_DEPTH * 4 {
                    return Err(ModelError::TooDeep);
                }
                stack.push(open(&e)?);
            }
            Event::Empty(e) => {
                let n = open(&e)?;
                stack
                    .last_mut()
                    .ok_or_else(|| ModelError::Xml("unbalanced".into()))?
                    .children
                    .push(n);
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err(ModelError::Xml("unbalanced end tag".into()));
                }
                let n = stack
                    .pop()
                    .ok_or_else(|| ModelError::Xml("unbalanced".into()))?;
                stack
                    .last_mut()
                    .ok_or_else(|| ModelError::Xml("unbalanced".into()))?
                    .children
                    .push(n);
            }
            Event::Text(t) => {
                let s = t.unescape().map_err(|e| ModelError::Xml(e.to_string()))?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(ModelError::Xml("unclosed element".into()));
    }
    stack
        .pop()
        .and_then(|root| root.children.into_iter().next())
        .ok_or_else(|| ModelError::Xml("empty document".into()))
}
