use crate::{
    index::Index,
    model::{Value, XmlElem},
};
use brace::{Guid, Node};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaContentEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnresolvedReference,
    AmbiguousReference,
    ResourceLimit,
}

type Result<T> = std::result::Result<T, CaContentEncodeError>;
const MAX_ITEMS: usize = 100_000;

fn shape(element: &XmlElem, name: &str, leaf: bool) -> Result<()> {
    if element.name != name
        || !element.attrs.is_empty()
        || element.expanded
        || (leaf && !element.children.is_empty())
        || (!leaf && element.text.is_some())
    {
        return Err(CaContentEncodeError::InvalidShape);
    }
    Ok(())
}

fn resolve(index: &Index, name: &str, conditional: bool) -> Result<Guid> {
    let nil = Guid::from_bytes([0; 16]);
    let mut found = None;
    for (id, actual) in &index.names {
        if actual != name || (conditional && *id == nil) {
            continue;
        }
        if found.replace(*id).is_some() {
            return Err(CaContentEncodeError::AmbiguousReference);
        }
    }
    found.ok_or(CaContentEncodeError::UnresolvedReference)
}

pub(crate) fn encode_cacontent(
    index: &Index,
    tag: &str,
    value: &Value,
    prefix: &Node,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(CaContentEncodeError::WrongValue);
    };
    shape(root, tag, false)?;
    if matches!(prefix, Node::List(_)) {
        return Err(CaContentEncodeError::InvalidPrefix);
    }
    let n = root.children.len();
    if n > MAX_ITEMS {
        return Err(CaContentEncodeError::ResourceLimit);
    }
    let count = i64::try_from(n).map_err(|_| CaContentEncodeError::ResourceLimit)?;
    let capacity = n
        .checked_mul(2)
        .and_then(|n| n.checked_add(2))
        .ok_or(CaContentEncodeError::ResourceLimit)?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|_| CaContentEncodeError::ResourceLimit)?;
    out.push(prefix.clone());
    out.push(Node::int(count));
    for item in &root.children {
        shape(item, "xr:Item", false)?;
        let [metadata, use_, conditional] = item.children.as_slice() else {
            return Err(CaContentEncodeError::InvalidShape);
        };
        shape(metadata, "xr:Metadata", true)?;
        shape(use_, "xr:Use", true)?;
        shape(conditional, "xr:ConditionalSeparation", true)?;
        let metadata = resolve(
            index,
            metadata
                .text
                .as_deref()
                .ok_or(CaContentEncodeError::InvalidShape)?,
            false,
        )?;
        let code = match use_.text.as_deref() {
            Some("Auto") => 0,
            Some("Use") => 1,
            Some("DontUse") => 2,
            _ => return Err(CaContentEncodeError::InvalidShape),
        };
        let conditional = match conditional.text.as_deref() {
            None => Guid::from_bytes([0; 16]),
            Some(name) => resolve(index, name, true)?,
        };
        out.push(Node::guid(metadata));
        out.push(Node::list(vec![
            Node::int(2),
            Node::int(code),
            Node::guid(conditional),
        ]));
    }
    Ok(Node::list(out))
}
