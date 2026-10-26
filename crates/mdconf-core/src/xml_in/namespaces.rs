use crate::xml::{XmlNode, view::NodeRef};

pub trait NamespaceScope {
    fn namespace(&self, prefix: Option<&str>) -> Option<&str>;
}

pub struct NamespaceContext<'a> {
    declarations: &'a [(Option<String>, String)],
    parent: Option<&'a dyn NamespaceScope>,
}
impl NamespaceContext<'_> {
    pub const EMPTY: Self = Self {
        declarations: &[],
        parent: None,
    };
}
impl<'a> NamespaceContext<'a> {
    pub fn extend(node: &'a XmlNode, parent: &'a dyn NamespaceScope) -> Self {
        Self {
            declarations: &node.ns_decls,
            parent: Some(parent),
        }
    }
}
impl NamespaceScope for NamespaceContext<'_> {
    fn namespace(&self, prefix: Option<&str>) -> Option<&str> {
        self.declarations
            .iter()
            .rev()
            .find(|(p, _)| p.as_deref() == prefix)
            .map(|(_, uri)| uri.as_str())
            .or_else(|| self.parent.and_then(|parent| parent.namespace(prefix)))
    }
}
impl NamespaceScope for XmlNode {
    fn namespace(&self, prefix: Option<&str>) -> Option<&str> {
        self.lookup_prefix(prefix)
    }
}
impl NamespaceScope for NodeRef<'_, '_> {
    fn namespace(&self, prefix: Option<&str>) -> Option<&str> {
        let mut node = Some(*self);
        while let Some(current) = node {
            if let Some((_, uri)) = current.ns_decls().find(|(p, _)| *p == prefix) {
                return Some(uri);
            }
            node = current.parent();
        }
        None
    }
}

pub(super) fn expanded<'a>(
    scope: &'a dyn NamespaceScope,
    text: &'a str,
) -> Option<(&'a str, &'a str)> {
    let (prefix, local) = match text.split_once(':') {
        Some((prefix, local)) if mdschema::is_ncname(prefix) && mdschema::is_ncname(local) => {
            (Some(prefix), local)
        }
        Some(_) => return None,
        None if mdschema::is_ncname(text) => (None, text),
        None => return None,
    };
    let uri = scope.namespace(prefix).filter(|uri| !uri.is_empty())?;
    Some((uri, local))
}
