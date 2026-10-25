use crate::model::Value;
use brace::Node;
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum XdtoTypeEncodeError {
    WrongValue,
    InvalidShape,
    UnsupportedValue,
    InvalidPrefix,
    ResourceLimit,
}
fn namespaces() -> impl Iterator<Item = (&'static str, &'static str)> {
    crate::xml_out::NS_DECL
        .split_whitespace()
        .filter_map(|declaration| {
            let (prefix, quoted) = declaration.strip_prefix("xmlns:")?.split_once('=')?;
            Some((prefix, quoted.strip_prefix('"')?.strip_suffix('"')?))
        })
}

pub(crate) fn encode_xdtotype(
    tag: &str,
    value: &Value,
    prefix: &Node,
) -> Result<Node, XdtoTypeEncodeError> {
    use XdtoTypeEncodeError::*;
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    let text = root.text.as_deref().ok_or(InvalidShape)?;
    if root.name != tag || root.expanded || !root.children.is_empty() {
        return Err(InvalidShape);
    }
    if matches!(prefix, Node::List(_)) {
        return Err(InvalidPrefix);
    }
    let (namespace, name) = match root.attrs.as_slice() {
        [] => match text.split_once(':') {
            None => ("", text),
            Some((head, name)) => (
                namespaces()
                    .find_map(|(p, u)| (p == head).then_some(u))
                    .ok_or(UnsupportedValue)?,
                name,
            ),
        },
        [(key, uri)] => {
            let (attribute, start) = if tag == "XDTOReturningValueType" {
                ("xmlns:d6p1", "d6p1:")
            } else {
                ("xmlns:d8p1", "d8p1:")
            };
            if key != attribute || uri.is_empty() || namespaces().any(|(_, u)| u == uri) {
                return Err(InvalidShape);
            }
            (uri.as_str(), text.strip_prefix(start).ok_or(InvalidShape)?)
        }
        _ => return Err(InvalidShape),
    };
    let mut out = Vec::new();
    out.try_reserve_exact(3).map_err(|_| ResourceLimit)?;
    out.push(prefix.clone());
    out.push(Node::str(namespace));
    out.push(Node::str(name));
    Ok(Node::list(out))
}
