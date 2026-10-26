pub use mdschema::FixedNamespace;

#[derive(Debug, Clone)]
pub enum TypeNamespace {
    Fixed(FixedNamespace),
    Uri(String),
}
impl TypeNamespace {
    pub fn uri(&self) -> &str {
        match self {
            Self::Fixed(ns) => ns.uri(),
            Self::Uri(uri) => uri,
        }
    }
}
impl PartialEq for TypeNamespace {
    fn eq(&self, other: &Self) -> bool {
        self.uri() == other.uri()
    }
}
impl Eq for TypeNamespace {}
impl std::hash::Hash for TypeNamespace {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uri().hash(state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XmlTypeName {
    pub namespace: TypeNamespace,
    pub local: String,
}
impl XmlTypeName {
    pub fn from_qname(qname: &str) -> Option<Self> {
        let name = mdschema::ExpandedTypeName::from_qname(qname)?;
        Self::new(name.namespace_uri, name.local)
    }
    pub fn new(namespace_uri: &str, local: &str) -> Option<Self> {
        mdschema::ExpandedTypeName::new(namespace_uri, local)?;
        Some(Self {
            namespace: match FixedNamespace::from_uri(namespace_uri) {
                Some(ns) => TypeNamespace::Fixed(ns),
                None => TypeNamespace::Uri(namespace_uri.into()),
            },
            local: local.into(),
        })
    }
    pub fn namespace_uri(&self) -> &str {
        self.namespace.uri()
    }
    pub fn legacy_qname(&self) -> Option<String> {
        if !mdschema::is_ncname(&self.local) {
            return None;
        }
        Some(format!(
            "{}:{}",
            FixedNamespace::from_uri(self.namespace_uri())?.prefix(),
            self.local
        ))
    }
    pub fn fixed_local(&self, namespace: FixedNamespace) -> Option<&str> {
        (self.namespace_uri() == namespace.uri()).then_some(self.local.as_str())
    }
    pub fn is_qname(&self, qname: &str) -> bool {
        mdschema::ExpandedTypeName::from_qname(qname).is_some_and(|name| {
            self.namespace_uri() == name.namespace_uri && self.local == name.local
        })
    }
}
impl std::fmt::Display for XmlTypeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ns) = FixedNamespace::from_uri(self.namespace_uri()) {
            write!(f, "{}:{}", ns.prefix(), self.local)
        } else {
            write!(f, "{{{}}}{}", self.namespace_uri(), self.local)
        }
    }
}
