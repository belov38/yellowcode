use brace::Node;

use super::LogicalSchema;
use super::tree::{TreeBuild, TreeNode, decode, encode};
use crate::error::{Error, Result};

const WHAT: &str = "DBSCHEMA";
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

impl TreeNode for Node {
    fn list(&self) -> Option<&[Node]> {
        self.as_list()
    }

    fn int(&self) -> Option<i64> {
        crate::canonical_i64(self.as_atom()?)
    }

    fn string(&self) -> Option<String> {
        self.as_str().map(str::to_owned)
    }
}

impl TreeBuild for Node {
    fn list(items: Vec<Node>) -> Node {
        Node::List(items)
    }

    fn int(v: i64) -> Node {
        Node::Atom(v.to_string())
    }

    fn string(s: &str) -> Node {
        Node::Str(s.to_owned())
    }
}

impl LogicalSchema {
    pub fn from_text(bytes: &[u8]) -> Result<LogicalSchema> {
        let root = brace::parse_bytes(bytes).map_err(|e| Error::Parse {
            what: WHAT,
            at: "file".into(),
            why: e.to_string(),
        })?;
        decode(&root, WHAT)
    }

    pub fn to_text(&self) -> Vec<u8> {
        let mut out = BOM.to_vec();
        out.extend_from_slice(brace::to_text(&encode::<Node>(self)).as_bytes());
        out
    }
}
