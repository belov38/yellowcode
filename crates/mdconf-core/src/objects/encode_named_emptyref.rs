use crate::index::Index;
use crate::model::{TypedValue, Value};
use brace::{Guid, Node};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamedEmptyRefError {
    WrongValue,
    UnsupportedValue,
    Unresolved,
    Ambiguous,
}

pub(crate) fn encode_named_emptyref(
    index: &Index,
    value: &Value,
) -> Result<Node, NamedEmptyRefError> {
    use NamedEmptyRefError::{Unresolved, UnsupportedValue, WrongValue};
    let Value::Typed(typed) = value else {
        return Err(WrongValue);
    };
    let TypedValue::DesignTimeRef(text) = typed else {
        return Err(UnsupportedValue);
    };
    let stem = text.strip_suffix(".EmptyRef").ok_or(UnsupportedValue)?;
    let (kind, rest) = stem.split_once('.').ok_or(UnsupportedValue)?;
    if kind.is_empty() || rest.is_empty() {
        return Err(UnsupportedValue);
    }
    if index.names.values().any(|name| name == text) {
        return Err(UnsupportedValue);
    }
    let generated = format!("{kind}Ref.{rest}");
    let preferred = unique(
        index
            .types
            .iter()
            .filter_map(|(id, name)| (name == &generated).then_some(*id)),
    )?;
    let id = match preferred {
        Some(id) => id,
        None => unique(
            index
                .names
                .iter()
                .filter_map(|(id, name)| (name == stem).then_some(*id)),
        )?
        .ok_or(Unresolved)?,
    };
    let zero = Guid::from_bytes([0; 16]);
    if id == zero {
        return Err(Unresolved);
    }
    let matches = match index.names.get(&id) {
        Some(name) => name == stem,
        None => index
            .types
            .get(&id)
            .and_then(|name| name.split_once('.'))
            .is_some_and(|(prefix, tail)| {
                prefix.strip_suffix("Ref").unwrap_or(prefix) == kind && tail == rest
            }),
    };
    if !matches {
        return Err(Unresolved);
    }
    Ok(Node::list(vec![
        Node::str("#"),
        Node::atom("5c14e26f-099b-4d37-84a6-b433d87400da"),
        Node::list(vec![Node::int(0), Node::guid(id), Node::guid(zero)]),
    ]))
}

fn unique(ids: impl Iterator<Item = Guid>) -> Result<Option<Guid>, NamedEmptyRefError> {
    let mut chosen = None;
    for id in ids {
        if chosen.is_some_and(|previous| previous != id) {
            return Err(NamedEmptyRefError::Ambiguous);
        }
        chosen = Some(id);
    }
    Ok(chosen)
}
