use crate::index::Index;
use crate::model::Value;
use brace::{Guid, Node};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReferenceError {
    AmbiguousName,
    WrongValue,
    Unresolved,
}

type Result<T> = std::result::Result<T, ReferenceError>;

pub(crate) struct ReferenceContext {
    by_name: HashMap<String, Guid>,
}

impl ReferenceContext {
    pub(crate) fn new(index: &Index) -> Result<Self> {
        Self::from_pairs(index.names.iter().map(|(id, name)| (name.as_str(), *id)))
    }

    fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, Guid)>) -> Result<Self> {
        let mut by_name = HashMap::new();
        for (name, id) in pairs {
            if let Some(previous) = by_name.insert(name.to_owned(), id)
                && previous != id
            {
                return Err(ReferenceError::AmbiguousName);
            }
        }
        Ok(Self { by_name })
    }

    pub(crate) fn encode(&self, value: &Value) -> Result<Node> {
        let Value::Ref(reference) = value else {
            return Err(ReferenceError::WrongValue);
        };
        let id = if let Some(id) = reference.id {
            id
        } else if reference.name.is_empty() {
            Guid::from_bytes([0; 16])
        } else {
            *self
                .by_name
                .get(&reference.name)
                .ok_or(ReferenceError::Unresolved)?
        };
        Ok(Node::guid(id))
    }
}
