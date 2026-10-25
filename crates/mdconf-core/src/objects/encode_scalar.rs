use crate::model::Value;
use brace::Node;
use mdschema::{Codec, PropSpec};
use std::borrow::Cow;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScalarEncodeError {
    NoInverse(Codec),
    BadValue(&'static str),
    EventTable(String),
}

fn cf_literal(text: &str) -> Cow<'_, str> {
    if text.contains('\n') {
        Cow::Owned(text.replace('\n', "\r\n"))
    } else {
        Cow::Borrowed(text)
    }
}

fn text(value: &Value) -> Result<&str, ScalarEncodeError> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(ScalarEncodeError::BadValue("expected text")),
    }
}

pub(crate) fn encode_scalar(
    spec: &PropSpec,
    value: &Value,
    current: Option<&Node>,
) -> Result<Node, ScalarEncodeError> {
    use ScalarEncodeError::BadValue;
    Ok(match spec.codec {
        Codec::Bool => match value {
            Value::Bool(value) => Node::int(i64::from(*value)),
            _ => return Err(BadValue("expected boolean")),
        },
        Codec::Int => {
            let text = text(value)?;
            text.parse::<i64>()
                .map_err(|_| BadValue("expected integer"))?;
            Node::atom(text)
        }
        Codec::Str => Node::str(cf_literal(text(value)?)),
        Codec::Enum => {
            let wanted = text(value)?;
            let key = if let Some(key) = current
                .and_then(Node::text)
                .filter(|key| spec.map.get(*key).is_some_and(|value| value == wanted))
            {
                key
            } else {
                let mut keys = spec
                    .map
                    .iter()
                    .filter(|(_, value)| value.as_str() == wanted);
                let (key, _) = keys.next().ok_or(BadValue("unknown enumeration literal"))?;
                if keys.next().is_some() {
                    return Err(BadValue("ambiguous enumeration literal"));
                }
                key
            };
            Node::atom(key)
        }
        Codec::LStr => {
            let Value::LStr(items) = value else {
                return Err(BadValue("expected localized string"));
            };
            let count = i64::try_from(items.0.len()).map_err(|_| BadValue("too many languages"))?;
            let capacity = items
                .0
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(1))
                .ok_or(BadValue("too many languages"))?;
            let mut nodes = Vec::new();
            nodes
                .try_reserve_exact(capacity)
                .map_err(|_| BadValue("localized string allocation failed"))?;
            nodes.push(Node::int(count));
            for (language, content) in &items.0 {
                nodes.push(Node::str(language));
                nodes.push(Node::str(cf_literal(content)));
            }
            Node::list(nodes)
        }
        Codec::Event => {
            let english = text(value)?;
            let events = mdschema::build::events()
                .map_err(|error| ScalarEncodeError::EventTable(error.to_string()))?;
            let localized = events.get(english).ok_or(BadValue("unknown event"))?;
            Node::str(format!("{english}_{localized}"))
        }
        other => return Err(ScalarEncodeError::NoInverse(other)),
    })
}
