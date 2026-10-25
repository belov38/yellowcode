use crate::{
    index::Index,
    model::{Value, XmlElem},
};
use brace::{Guid, Node};
const MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
const MAX_ITEMS: usize = 100_000;
const MAX_PREFIX_NODES: usize = 1000;
const MAX_PREFIX_DEPTH: usize = 128;
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum XdtoPackagesEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnresolvedReference,
    AmbiguousReference,
    ResourceLimit,
}
pub(crate) struct XdtoPackagesPrefixes<'a> {
    pub record: [&'a Node; 4],
    pub references: &'a Node,
    pub tail: &'a [Node],
}

impl<'a> XdtoPackagesPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node) -> Option<Self> {
        let l = raw.as_list()?;
        let tail = l.get(6..).unwrap_or(&[]);
        Some(Self {
            record: [l.first()?, l.get(1)?, l.get(2)?, l.get(4)?],
            references: l.get(3)?.as_list()?.first()?,
            tail: tail.get(..2).unwrap_or(tail),
        })
    }
}
fn shape(e: &XmlElem, name: &str, leaf: bool) -> Result<(), XdtoPackagesEncodeError> {
    if e.name != name
        || !e.attrs.is_empty()
        || e.expanded
        || (leaf && !e.children.is_empty())
        || (!leaf && e.text.is_some())
    {
        return Err(XdtoPackagesEncodeError::InvalidShape);
    }
    Ok(())
}
fn resolve(index: &Index, name: &str) -> Result<Guid, XdtoPackagesEncodeError> {
    let mut found = None;
    for (id, actual) in &index.names {
        if actual == name && found.replace(*id).is_some() {
            return Err(XdtoPackagesEncodeError::AmbiguousReference);
        }
    }
    found.ok_or(XdtoPackagesEncodeError::UnresolvedReference)
}
fn validate_prefix(
    node: &Node,
    depth: usize,
    remaining: &mut usize,
) -> Result<(), XdtoPackagesEncodeError> {
    if depth > MAX_PREFIX_DEPTH || *remaining == 0 {
        return Err(XdtoPackagesEncodeError::ResourceLimit);
    }
    *remaining -= 1;
    if let Node::List(children) = node {
        for child in children {
            validate_prefix(child, depth + 1, remaining)?;
        }
    }
    Ok(())
}
pub(crate) fn encode_xdtopackages(
    index: &Index,
    tag: &str,
    value: &Value,
    prefixes: XdtoPackagesPrefixes<'_>,
) -> Result<Node, XdtoPackagesEncodeError> {
    use XdtoPackagesEncodeError::*;
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    shape(root, tag, false)?;
    if [
        prefixes.record[0],
        prefixes.record[1],
        prefixes.record[3],
        prefixes.references,
    ]
    .into_iter()
    .chain(prefixes.tail)
    .any(|n| matches!(n, Node::List(_)))
    {
        return Err(InvalidPrefix);
    }
    let mut remaining = MAX_PREFIX_NODES;
    for prefix in prefixes
        .record
        .iter()
        .copied()
        .chain(std::iter::once(prefixes.references))
        .chain(prefixes.tail)
    {
        validate_prefix(prefix, 0, &mut remaining)?;
    }
    let n = root.children.len();
    if n > MAX_ITEMS {
        return Err(ResourceLimit);
    }
    let refs_capacity = n.checked_add(2).ok_or(ResourceLimit)?;
    let uris_capacity = n.checked_add(1).ok_or(ResourceLimit)?;
    let mut refs = Vec::new();
    refs.try_reserve_exact(refs_capacity)
        .map_err(|_| ResourceLimit)?;
    let mut uris = Vec::new();
    uris.try_reserve_exact(uris_capacity)
        .map_err(|_| ResourceLimit)?;
    let mut out = Vec::new();
    let out_capacity = prefixes.tail.len().checked_add(6).ok_or(ResourceLimit)?;
    out.try_reserve_exact(out_capacity)
        .map_err(|_| ResourceLimit)?;
    refs.push(prefixes.references.clone());
    refs.push(Node::int(0));
    uris.push(Node::int(0));
    let mut uri_phase = false;
    for item in &root.children {
        shape(item, "xr:Item", false)?;
        let [presentation, state, value] = item.children.as_slice() else {
            return Err(InvalidShape);
        };
        shape(presentation, "xr:Presentation", true)?;
        if presentation.text.is_some() {
            return Err(InvalidShape);
        }
        shape(state, "xr:CheckState", true)?;
        if state.text.as_deref() != Some("0") {
            return Err(InvalidShape);
        }
        if value.name != "xr:Value" || !value.children.is_empty() || value.expanded {
            return Err(InvalidShape);
        }
        let [(key, kind)] = value.attrs.as_slice() else {
            return Err(InvalidShape);
        };
        if key != "xsi:type" {
            return Err(InvalidShape);
        }
        let text = value.text.as_deref().ok_or(InvalidShape)?;
        match kind.as_str() {
            "xr:MDObjectRef" if !uri_phase => {
                let id = resolve(index, text)?;
                refs.push(Node::list(vec![
                    Node::str("#"),
                    Node::atom(MDREF),
                    Node::list(vec![Node::int(1), Node::guid(id)]),
                ]));
            }
            "xs:string" => {
                uri_phase = true;
                uris.push(Node::str(text));
            }
            _ => return Err(InvalidShape),
        }
    }
    let reference_count = i64::try_from(refs.len() - 2).map_err(|_| ResourceLimit)?;
    let uri_count = i64::try_from(uris.len() - 1).map_err(|_| ResourceLimit)?;
    *refs.get_mut(1).ok_or(InvalidShape)? = Node::int(reference_count);
    *uris.first_mut().ok_or(InvalidShape)? = Node::int(uri_count);
    out.push(prefixes.record[0].clone());
    out.push(prefixes.record[1].clone());
    out.push(prefixes.record[2].clone());
    out.push(Node::list(refs));
    out.push(prefixes.record[3].clone());
    out.push(Node::list(uris));
    out.extend(prefixes.tail.iter().cloned());
    Ok(Node::list(out))
}
