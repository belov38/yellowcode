use super::*;
use crate::xml::view::NodeRef;

#[derive(Clone, Copy, Debug)]
pub enum EmbeddedNamespaces {
    Plain,
    Settings,
}

#[derive(Clone, Copy)]
pub struct EmbeddedRoot<'a> {
    pub namespace: &'a str,
    pub local: &'a str,
    pub wrap: Option<(&'a str, &'a str)>,
    pub namespaces: EmbeddedNamespaces,
}

#[derive(Clone, Copy)]
pub struct EmbeddedChild<'a, 'src> {
    pub source: NodeRef<'a, 'src>,
    pub rename: Option<(&'a str, &'a str)>,
    pub fallback_type: Option<(&'a str, &'a str)>,
}

fn name(namespace: &str, local: &str, allow_empty: bool) -> Result<()> {
    if !mdschema::is_ncname(local)
        || !(mdschema::valid_type_namespace(namespace) || (allow_empty && namespace.is_empty()))
    {
        return Err(BuildError::Invalid("embedded name"));
    }
    Ok(())
}

pub fn write_embedded_checked<'a, 'src: 'a>(
    children: impl IntoIterator<Item = EmbeddedChild<'a, 'src>>,
    root: EmbeddedRoot<'_>,
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Charged<Vec<u8>>> {
    let cx = Context {
        allocation: Allocation::Managed(budget),
        cancelled,
    };
    cx.check()?;
    name(root.namespace, root.local, true)?;
    let mut document = Node::new(root.namespace, root.local, cx)?;
    let mut content = if let Some((namespace, local)) = root.wrap {
        name(namespace, local, true)?;
        Node::new(namespace, local, cx)?
    } else {
        Node::new(root.namespace, root.local, cx)?
    };
    let converter = tree::Converter {
        cx,
        lookup,
        assets,
        embedded: true,
    };
    for child in children {
        cx.check()?;
        let mut node = converter.convert(child.source, if root.wrap.is_some() { 3 } else { 2 })?;
        if let Some((namespace, local)) = child.rename {
            name(namespace, local, true)?;
            node.ns = namespace;
            node.local = local;
        }
        if !node.attrs.iter().any(|a| (a.ns, a.local) == (XSI, "type"))
            && let Some((namespace, local)) = child.fallback_type
        {
            name(namespace, local, false)?;
            node.attrs.push(tree::Attribute {
                ns: XSI,
                local: "type",
                value: tree::Value::QName(namespace, local),
            })?;
        }
        content.children.push(node)?;
    }
    if root.wrap.is_some() {
        document.children.push(content)?;
    } else {
        std::mem::swap(&mut document.children, &mut content.children);
    }
    let result = write::embedded_file(&document, root.namespaces, cx)?;
    cx.check()?;
    Ok(result.managed()?)
}

pub fn write_embedded<'a, 'src: 'a>(
    children: impl IntoIterator<Item = EmbeddedChild<'a, 'src>>,
    root: EmbeddedRoot<'_>,
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancelled: &AtomicBool,
) -> Result<Charged<Vec<u8>>> {
    write_embedded_checked(children, root, assets, lookup, budget, &|| {
        cancelled.load(Ordering::Relaxed)
    })
}
