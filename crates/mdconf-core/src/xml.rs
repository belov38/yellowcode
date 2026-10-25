#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlNode {
    pub ns: Option<String>,
    pub local: String,
    pub prefix: Option<String>,
    pub attrs: Vec<XmlAttr>,
    pub ns_decls: Vec<(Option<String>, String)>,
    pub children: Vec<XmlNode>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlAttr {
    pub ns: Option<String>,
    pub local: String,
    pub prefix: Option<String>,
    pub value: String,
}

impl XmlNode {
    pub fn attr(&self, ns: Option<&str>, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns.as_deref() == ns && a.local == local)
            .map(|a| a.value.as_str())
    }

    pub fn lookup_prefix(&self, prefix: Option<&str>) -> Option<&str> {
        self.ns_decls
            .iter()
            .find(|(p, _)| p.as_deref() == prefix)
            .map(|(_, u)| u.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &XmlNode> {
        self.children.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("xml at {offset}: {what}")]
pub struct XmlError {
    pub offset: usize,
    pub what: String,
}

pub fn parse_xml(bytes: &[u8]) -> Result<XmlNode, XmlError> {
    read::parse_arena(bytes, crate::allocation::Allocation::Native)
        .and_then(|arena| arena.into_native().map_err(read::ReadError::Memory))
        .map_err(|error| read::legacy_error(bytes, error))
}

#[doc(hidden)]
pub mod read;
#[doc(hidden)]
pub mod view;
