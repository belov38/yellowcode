use super::super::{shell_xml, walk};
use super::Assembly;
use crate::Result;
use std::path::Path;

impl Assembly {
    fn charge_scope(&mut self, bytes: usize, path: &Path) -> Result<()> {
        let total = self
            .report
            .value_bytes
            .checked_add(bytes as u64)
            .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?;
        if total > self.max_value_bytes {
            return Err(walk::err(path, "namespace scope byte limit"));
        }
        self.report.value_bytes = total;
        Ok(())
    }
    fn scope_path(&mut self, mut n: shell_xml::N<'_, '_>, path: &Path) -> Result<Vec<usize>> {
        let depth = std::iter::successors(n.parent(), |n| n.parent()).count();
        self.charge_scope(
            depth
                .checked_mul(std::mem::size_of::<usize>())
                .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?,
            path,
        )?;
        let mut out = Vec::with_capacity(depth);
        while let Some(parent) = n.parent() {
            out.push(
                parent
                    .children()
                    .position(|child| child.same_node(n))
                    .ok_or_else(|| walk::err(path, "invalid namespace scope path"))?,
            );
            n = parent;
        }
        out.reverse();
        Ok(out)
    }
    pub(super) fn capture_scope(
        &mut self,
        property: shell_xml::N<'_, '_>,
        value: &crate::Value,
        path: &Path,
    ) -> Result<()> {
        use md_model::namespace_scope::NamespaceAnchor;
        let mut root = property;
        while let Some(parent) = root.parent() {
            root = parent;
        }
        let owner = root
            .children()
            .next()
            .ok_or_else(|| walk::err(path, "missing namespace scope owner"))?;
        let id = shell_xml::attr(owner, None, "uuid")
            .and_then(brace::Guid::parse)
            .ok_or_else(|| walk::err(path, "missing namespace scope owner UUID"))?;
        let mut current = property.parent();
        while let Some(n) = current {
            let node_path = self.scope_path(n, path)?;
            let existing = self
                .namespace_envelopes
                .get(&id)
                .and_then(|envelope| envelope.anchors.iter().find(|a| a.path == node_path));
            if let Some(existing) = existing {
                if existing.declarations.len() != n.ns_decls().count()
                    || existing.declarations.iter().zip(n.ns_decls()).any(
                        |((key, uri), (prefix, value))| {
                            uri != value
                                || match prefix {
                                    Some(p) => key.strip_prefix("xmlns:") != Some(p),
                                    None => key != "xmlns",
                                }
                        },
                    )
                {
                    return Err(walk::err(path, "conflicting namespace envelope"));
                }
            } else {
                let identity = n
                    .attrs()
                    .find(|a| a.ns().is_none() && matches!(a.local(), "uuid" | "name"));
                let mut bytes = std::mem::size_of::<NamespaceAnchor>()
                    .checked_add(n.local().len())
                    .and_then(|b| b.checked_add(n.prefix().map_or(0, |p| p.len() + 1)))
                    .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?;
                if let Some(a) = identity {
                    bytes = bytes
                        .checked_add(a.local().len())
                        .and_then(|b| b.checked_add(a.value().len()))
                        .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?;
                }
                for (prefix, uri) in n.ns_decls() {
                    bytes = bytes
                        .checked_add(std::mem::size_of::<(String, String)>())
                        .and_then(|b| b.checked_add(6 + prefix.map_or(0, str::len)))
                        .and_then(|b| b.checked_add(uri.len()))
                        .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?;
                }
                self.charge_scope(bytes, path)?;
                let prefix = n
                    .prefix()
                    .map_or(String::new(), |prefix| format!("{prefix}:"));
                let anchor = NamespaceAnchor {
                    path: node_path,
                    element: format!("{prefix}{}", n.local()),
                    identity: identity.map(|a| (a.local().to_owned(), a.value().to_owned())),
                    declarations: n
                        .ns_decls()
                        .map(|(prefix, uri)| {
                            (
                                prefix.map_or_else(|| "xmlns".into(), |p| format!("xmlns:{p}")),
                                uri.to_owned(),
                            )
                        })
                        .collect(),
                };
                self.namespace_envelopes
                    .entry(id)
                    .or_default()
                    .anchors
                    .push(anchor);
            }
            current = n.parent();
        }
        self.capture_raw_scope(id, property, value, path)
    }
    fn capture_raw_scope(
        &mut self,
        id: brace::Guid,
        property: shell_xml::N<'_, '_>,
        value: &crate::Value,
        path: &Path,
    ) -> Result<()> {
        use md_model::namespace_scope::ScopedRawProperty;
        if let crate::Value::StandardAttributes(attributes) = value {
            for (record, attribute) in property.children().zip(attributes) {
                for (node, prop) in record.children().zip(&attribute.props) {
                    self.capture_raw_scope(id, node, &prop.value, path)?;
                }
            }
        }
        if let crate::Value::Xml(raw) = value {
            let node_path = self.scope_path(property, path)?;
            fn storage(e: &crate::XmlElem) -> Option<usize> {
                let mut bytes = std::mem::size_of::<crate::XmlElem>()
                    .checked_add(e.name.len())?
                    .checked_add(e.text.as_ref().map_or(0, String::len))?;
                for (name, value) in &e.attrs {
                    bytes = bytes
                        .checked_add(std::mem::size_of::<(String, String)>())?
                        .checked_add(name.len())?
                        .checked_add(value.len())?;
                }
                for child in &e.children {
                    bytes = bytes.checked_add(storage(child)?)?;
                }
                Some(bytes)
            }
            let bytes = storage(raw)
                .and_then(|b| b.checked_add(std::mem::size_of::<ScopedRawProperty>()))
                .ok_or_else(|| walk::err(path, "namespace scope byte limit"))?;
            self.charge_scope(bytes, path)?;
            self.namespace_envelopes
                .entry(id)
                .or_default()
                .raw_properties
                .push(ScopedRawProperty {
                    path: node_path,
                    element: raw.clone(),
                });
        }
        Ok(())
    }
}
