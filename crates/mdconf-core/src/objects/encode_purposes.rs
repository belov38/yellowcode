use crate::model::{Value, XmlElem};
use brace::Node;

const MAX_CHILDREN: usize = 100_000;
const PURPOSE: &str = "1708fdaa-cbce-4289-b373-07a5a74bee91";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PurposesEncodeError {
    WrongValue,
    InvalidShape,
    UnknownPurpose,
    ResourceLimit,
}

fn purpose_code(child: &XmlElem) -> Result<i64, PurposesEncodeError> {
    if child.name != "v8:Value"
        || !matches!(child.attrs.as_slice(), [(name, value)] if name == "xsi:type" && value == "app:ApplicationUsePurpose")
        || !child.children.is_empty()
        || child.expanded
    {
        return Err(PurposesEncodeError::InvalidShape);
    }
    match child.text.as_deref() {
        Some("PlatformApplication") => Ok(1),
        Some("MobilePlatformApplication") => Ok(2),
        Some(_) => Err(PurposesEncodeError::UnknownPurpose),
        None => Err(PurposesEncodeError::InvalidShape),
    }
}

pub(crate) fn encode_purposes(tag: &str, value: &Value) -> Result<Node, PurposesEncodeError> {
    let Value::Xml(root) = value else {
        return Err(PurposesEncodeError::WrongValue);
    };
    if root.children.len() > MAX_CHILDREN {
        return Err(PurposesEncodeError::ResourceLimit);
    }
    if root.name != tag || !root.attrs.is_empty() || root.text.is_some() || root.expanded {
        return Err(PurposesEncodeError::InvalidShape);
    }
    let count =
        i64::try_from(root.children.len()).map_err(|_| PurposesEncodeError::ResourceLimit)?;
    let capacity = root
        .children
        .len()
        .checked_add(1)
        .ok_or(PurposesEncodeError::ResourceLimit)?;
    for child in &root.children {
        purpose_code(child)?;
    }
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(capacity)
        .map_err(|_| PurposesEncodeError::ResourceLimit)?;
    nodes.push(Node::int(count));
    for child in &root.children {
        nodes.push(Node::list(vec![
            Node::str("#"),
            Node::atom(PURPOSE),
            Node::int(purpose_code(child)?),
        ]));
    }
    Ok(Node::list(nodes))
}
