use brace::{Guid, Node};

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDumpInfo {
    pub versions: Vec<(String, Guid)>,
    #[doc(hidden)]
    pub source: Node,
}
