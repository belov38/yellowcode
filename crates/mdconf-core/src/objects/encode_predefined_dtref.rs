use super::encode_named_emptyref::encode_named_emptyref;
use crate::index::Index;
use crate::model::{TypedValue, Value};
use brace::{Guid, Node};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PredefinedDtRefError {
    WrongValue,
    UnsupportedValue,
    Unresolved,
    Ambiguous,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct PredefinedContext<'a> {
    names: Option<&'a HashMap<Guid, String>>,
    tables: Option<&'a HashMap<Guid, HashMap<Guid, String>>>,
}

impl<'a> PredefinedContext<'a> {
    pub(crate) fn names(&self) -> Option<&'a HashMap<Guid, String>> {
        self.names
    }
    pub(crate) fn new(
        names: &'a HashMap<Guid, String>,
        tables: &'a HashMap<Guid, HashMap<Guid, String>>,
    ) -> Self {
        Self {
            names: Some(names),
            tables: Some(tables),
        }
    }
}

pub(crate) fn is_predefined_shape(text: &str) -> bool {
    let mut parts = text.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(kind), Some(object), Some(item), None)
            if !kind.is_empty() && !object.is_empty() && !item.is_empty() && item != "EmptyRef"
    )
}

pub(crate) fn encode_predefined_dtref(
    index: &Index,
    context: &PredefinedContext<'_>,
    value: &Value,
) -> Result<Node, PredefinedDtRefError> {
    use PredefinedDtRefError::*;
    let Value::Typed(TypedValue::DesignTimeRef(text)) = value else {
        return Err(WrongValue);
    };
    if !is_predefined_shape(text) || index.names.values().any(|name| name == text) {
        return Err(UnsupportedValue);
    }
    let (Some(names), Some(tables)) = (context.names, context.tables) else {
        return Err(UnsupportedValue);
    };
    let Some((stem, item)) = text.rsplit_once('.') else {
        return Err(UnsupportedValue);
    };
    let object = unique(
        index
            .names
            .iter()
            .filter_map(|(id, name)| (name == stem).then_some(*id)),
    )?
    .ok_or(Unresolved)?;
    let zero = Guid::from_bytes([0; 16]);
    let own = tables.get(&object).ok_or(Unresolved)?;
    let item_id = unique(own.iter().filter_map(|(id, name)| {
        (name == item
            && names.get(id) == Some(name)
            && *id != zero
            && !index.names.contains_key(id))
        .then_some(*id)
    }))?
    .ok_or(Unresolved)?;
    let empty = Value::Typed(TypedValue::DesignTimeRef(format!("{stem}.EmptyRef")));
    let node = encode_named_emptyref(index, &empty).map_err(|_| Unresolved)?;
    let Node::List(outer) = node else {
        return Err(Unresolved);
    };
    let [hash, kind, Node::List(payload)] = outer.as_slice() else {
        return Err(Unresolved);
    };
    let [head, object_or_type, _] = payload.as_slice() else {
        return Err(Unresolved);
    };
    Ok(Node::list(vec![
        hash.clone(),
        kind.clone(),
        Node::list(vec![
            head.clone(),
            object_or_type.clone(),
            Node::guid(item_id),
        ]),
    ]))
}

fn unique(mut ids: impl Iterator<Item = Guid>) -> Result<Option<Guid>, PredefinedDtRefError> {
    let Some(first) = ids.next() else {
        return Ok(None);
    };
    if ids.any(|id| id != first) {
        return Err(PredefinedDtRefError::Ambiguous);
    }
    Ok(Some(first))
}
