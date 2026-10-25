use super::codecs::Owner;
use crate::index::Index;
use crate::model::Value;
use brace::Node;
use mdschema::Schema;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldListEncodeError {
    WrongValue,
    InvalidPrefix,
    UnresolvedField,
    AmbiguousField,
    ResourceLimit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Candidate {
    Standard(i64),
    Indexed(brace::Guid),
}

fn select(found: &mut Option<Candidate>, next: Candidate) -> Result<(), FieldListEncodeError> {
    if found.is_some_and(|previous| previous != next) {
        return Err(FieldListEncodeError::AmbiguousField);
    }
    *found = Some(next);
    Ok(())
}

fn resolve(
    schema: &Schema,
    index: &Index,
    owner: &Owner,
    name: &str,
) -> Result<Candidate, FieldListEncodeError> {
    if name.is_empty() {
        return Err(FieldListEncodeError::UnresolvedField);
    }
    let mut found = None;
    for (id, indexed_name) in &index.names {
        if indexed_name == name {
            select(&mut found, Candidate::Indexed(*id))?;
        }
    }
    for (key, attribute) in schema.standard_attributes(&owner.kind) {
        if let Ok(number) = key.parse::<i64>()
            && number.to_string() == key
            && name
                == format!(
                    "{}.{}.StandardAttribute.{attribute}",
                    owner.kind, owner.name
                )
        {
            select(&mut found, Candidate::Standard(number))?;
        }
    }
    found.ok_or(FieldListEncodeError::UnresolvedField)
}

pub(crate) fn encode_fieldlist(
    schema: &Schema,
    index: &Index,
    owner: &Owner,
    value: &Value,
    outer_prefix: &Node,
    inner_prefix: &Node,
) -> Result<Node, FieldListEncodeError> {
    let Value::FieldList(fields) = value else {
        return Err(FieldListEncodeError::WrongValue);
    };
    if matches!(outer_prefix, Node::List(_)) || matches!(inner_prefix, Node::List(_)) {
        return Err(FieldListEncodeError::InvalidPrefix);
    }
    if fields.len() > 100_000 {
        return Err(FieldListEncodeError::ResourceLimit);
    }
    let count = i64::try_from(fields.len()).map_err(|_| FieldListEncodeError::ResourceLimit)?;
    let capacity = fields
        .len()
        .checked_add(2)
        .ok_or(FieldListEncodeError::ResourceLimit)?;
    let mut inner = Vec::new();
    inner
        .try_reserve_exact(capacity)
        .map_err(|_| FieldListEncodeError::ResourceLimit)?;
    inner.push(inner_prefix.clone());
    inner.push(Node::int(count));
    for name in fields {
        let segment = match resolve(schema, index, owner, name)? {
            Candidate::Standard(id) => vec![Node::int(id)],
            Candidate::Indexed(id) => vec![Node::int(0), Node::guid(id)],
        };
        inner.push(Node::list(vec![
            Node::str("#"),
            Node::atom("60ea359f-3a6e-48bb-8e71-d2a457572918"),
            Node::list(segment),
        ]));
    }
    Ok(Node::list(vec![outer_prefix.clone(), Node::list(inner)]))
}
