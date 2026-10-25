use brace::binary::{Atom, BinNode};

use super::LogicalSchema;
use super::tree::{TreeBuild, TreeNode, decode, encode};
use crate::error::Result;

const WHAT: &str = "DBSchema";

impl TreeNode for BinNode {
    fn list(&self) -> Option<&[BinNode]> {
        match self {
            BinNode::List(items) => Some(items),
            BinNode::Atom(_) => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            BinNode::Atom(a) => i64::try_from(a.as_int()?).ok(),
            BinNode::List(_) => None,
        }
    }

    fn string(&self) -> Option<String> {
        match self {
            BinNode::Atom(a) => a.as_str(),
            BinNode::List(_) => None,
        }
    }
}

impl TreeBuild for BinNode {
    fn list(items: Vec<BinNode>) -> BinNode {
        BinNode::List(items)
    }

    fn int(v: i64) -> BinNode {
        BinNode::Atom(Atom::int(i128::from(v)))
    }

    fn string(s: &str) -> BinNode {
        BinNode::Atom(Atom::str(s))
    }
}

impl LogicalSchema {
    pub fn from_bin(root: &BinNode) -> Result<LogicalSchema> {
        decode(root, WHAT)
    }

    pub fn to_bin(&self) -> BinNode {
        encode::<BinNode>(self)
    }
}
