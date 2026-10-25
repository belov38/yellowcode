use brace::Node;

#[derive(Debug, Clone, PartialEq)]
pub struct CommandInterface {
    #[doc(hidden)]
    pub source: Node,
}
