use super::encode_mdlist::{MdListEncodeError, encode_mdlist};
use super::encode_reference::ReferenceContext;
use crate::model::Value;
use brace::Node;

const FOCONTENT: &str = "3ea29ea5-66f6-4e3b-8595-d8940db766a2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FoContentEncodeError {
    Backend(MdListEncodeError),
    InternalShape,
}

pub(crate) fn encode_focontent(
    references: &ReferenceContext,
    value: &Value,
    prefix: &Node,
) -> Result<Node, FoContentEncodeError> {
    let generated =
        encode_mdlist(references, value, prefix).map_err(FoContentEncodeError::Backend)?;
    let Node::List(mut nodes) = generated else {
        return Err(FoContentEncodeError::InternalShape);
    };
    for node in nodes.iter_mut().skip(2) {
        let reference = std::mem::replace(node, Node::list(Vec::new()));
        *node = Node::list(vec![
            Node::str("#"),
            Node::atom(FOCONTENT),
            Node::list(vec![Node::int(0), reference]),
        ]);
    }
    Ok(Node::list(nodes))
}
