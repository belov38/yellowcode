use crate::XmlElem;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NamespaceEnvelope {
    pub anchors: Vec<NamespaceAnchor>,
    pub raw_properties: Vec<ScopedRawProperty>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceAnchor {
    pub path: Vec<usize>,
    pub element: String,
    pub identity: Option<(String, String)>,
    pub declarations: Vec<(String, String)>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedRawProperty {
    pub path: Vec<usize>,
    pub element: XmlElem,
}
