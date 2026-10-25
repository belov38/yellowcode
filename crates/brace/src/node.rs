use crate::Guid;
use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

const B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    List(Vec<Node>),
    Str(String),
    Atom(String),
}

impl Node {
    pub fn list(items: Vec<Node>) -> Node {
        Node::List(items)
    }

    pub fn str(s: impl Into<String>) -> Node {
        Node::Str(s.into())
    }

    pub fn atom(s: impl Into<String>) -> Node {
        Node::Atom(s.into())
    }

    pub fn int(v: i64) -> Node {
        Node::Atom(v.to_string())
    }

    pub fn guid(g: Guid) -> Node {
        Node::Atom(g.to_string())
    }

    pub fn empty() -> Node {
        Node::Atom(String::new())
    }

    pub fn as_list(&self) -> Option<&[Node]> {
        match self {
            Node::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Node>> {
        match self {
            Node::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_atom(&self) -> Option<&str> {
        match self {
            Node::Atom(s) => Some(s),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Node::Str(s) | Node::Atom(s) => Some(s),
            Node::List(_) => None,
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Node::List(_))
    }

    pub fn is_empty_slot(&self) -> bool {
        matches!(self, Node::Atom(s) if s.is_empty())
    }

    pub fn at(&self, i: usize) -> Option<&Node> {
        self.as_list()?.get(i)
    }

    pub fn get(&self, path: &[usize]) -> Option<&Node> {
        path.iter().try_fold(self, |n, &i| n.at(i))
    }

    pub fn get_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        path.iter()
            .try_fold(self, |n, &i| n.as_list_mut()?.get_mut(i))
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_atom()?.parse().ok()
    }

    pub fn as_u32(&self) -> Option<u32> {
        self.as_atom()?.parse().ok()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.as_atom()? {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        }
    }

    pub fn as_guid(&self) -> Option<Guid> {
        Guid::parse(self.as_atom()?)
    }

    pub fn is_guid(&self) -> bool {
        self.as_guid().is_some()
    }

    pub fn base64(&self) -> Option<Vec<u8>> {
        let body = self.as_atom()?.strip_prefix("#base64:")?;
        let clean: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        B64.decode(clean).ok()
    }
}
