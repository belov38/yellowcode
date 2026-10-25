use super::encode_reference::ReferenceContext;
use crate::model::Value;
use brace::Node;

const MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
const MAX_REFERENCES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MdListEncodeError {
    WrongValue,
    InvalidPrefix,
    UnresolvedReference,
    ResourceLimit,
}
type Result<T> = std::result::Result<T, MdListEncodeError>;

pub(crate) fn encode_mdlist(
    references: &ReferenceContext,
    value: &Value,
    prefix: &Node,
) -> Result<Node> {
    let Value::RefList(items) = value else {
        return Err(MdListEncodeError::WrongValue);
    };
    if matches!(prefix, Node::List(_)) {
        return Err(MdListEncodeError::InvalidPrefix);
    }
    if items.len() > MAX_REFERENCES {
        return Err(MdListEncodeError::ResourceLimit);
    }
    let count = i64::try_from(items.len()).map_err(|_| MdListEncodeError::ResourceLimit)?;
    let capacity = items
        .len()
        .checked_add(2)
        .ok_or(MdListEncodeError::ResourceLimit)?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|_| MdListEncodeError::ResourceLimit)?;
    out.push(prefix.clone());
    out.push(Node::int(count));
    for item in items {
        let id = references
            .encode(&Value::Ref(item.clone()))
            .map_err(|_| MdListEncodeError::UnresolvedReference)?;
        out.push(Node::list(vec![
            Node::str("#"),
            Node::atom(MDREF),
            Node::list(vec![Node::int(1), id]),
        ]));
    }
    Ok(Node::list(out))
}
