use crate::index::Index;
use crate::model::{TypedValue, Value};
use brace::{Guid, Node};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnumDtRefError {
    WrongValue,
    UnsupportedValue,
    Unresolved,
    Ambiguous,
}

pub(crate) fn encode_enum_dtref(index: &Index, value: &Value) -> Result<Node, EnumDtRefError> {
    use EnumDtRefError::{Unresolved, UnsupportedValue, WrongValue};
    let Value::Typed(TypedValue::DesignTimeRef(text)) = value else {
        return Err(if matches!(value, Value::Typed(_)) {
            UnsupportedValue
        } else {
            WrongValue
        });
    };
    let mut parts = text.split('.');
    let (Some("Enum"), Some(name), Some("EnumValue"), Some(member), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(UnsupportedValue);
    };
    if name.is_empty() || member.is_empty() {
        return Err(UnsupportedValue);
    }
    let value_id = unique(
        index
            .names
            .iter()
            .filter_map(|(id, n)| (n == text).then_some(*id)),
    )?
    .ok_or(Unresolved)?;
    let zero = Guid::from_bytes([0; 16]);
    if value_id == zero {
        return Err(Unresolved);
    }
    let generated = format!("EnumRef.{name}");
    let preferred = unique(
        index
            .types
            .iter()
            .filter_map(|(id, n)| (n == &generated).then_some(*id)),
    )?;
    let stem = format!("Enum.{name}");
    let object_id = match preferred {
        Some(id) => id,
        None => unique(
            index
                .names
                .iter()
                .filter_map(|(id, n)| (n == &stem).then_some(*id)),
        )?
        .ok_or(Unresolved)?,
    };
    if object_id == zero {
        return Err(Unresolved);
    }
    let stem_matches = match index.names.get(&object_id) {
        Some(n) => n == &stem,
        None => index
            .types
            .get(&object_id)
            .and_then(|n| n.split_once('.'))
            .is_some_and(|(prefix, rest)| {
                prefix.strip_suffix("Ref").unwrap_or(prefix) == "Enum" && rest == name
            }),
    };
    if !stem_matches {
        return Err(Unresolved);
    }
    Ok(Node::list(vec![
        Node::str("#"),
        Node::atom("5c14e26f-099b-4d37-84a6-b433d87400da"),
        Node::list(vec![
            Node::int(0),
            Node::guid(object_id),
            Node::guid(value_id),
        ]),
    ]))
}

fn unique(ids: impl Iterator<Item = Guid>) -> Result<Option<Guid>, EnumDtRefError> {
    let mut chosen = None;
    for id in ids {
        if chosen.is_some_and(|previous| previous != id) {
            return Err(EnumDtRefError::Ambiguous);
        }
        chosen = Some(id);
    }
    Ok(chosen)
}
