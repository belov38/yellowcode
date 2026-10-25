use super::encode_predefined_dtref::{
    PredefinedContext, encode_predefined_dtref, is_predefined_shape,
};
use super::encode_scalar::encode_scalar;
use crate::model::{TypedValue, Value};
use brace::{Guid, Node};
use mdschema::{Codec, PropSpec};

const TYPE_DESCRIPTION: &str = "f5c65050-3bbb-11d5-b988-0050bae0a95d";
const FIXED_ARRAY: &str = "4500381b-db30-4a10-9db4-990038032acf";
const MAX_DEPTH: usize = 128;
const MAX_VISITS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypedEncodeError {
    WrongValue,
    InvalidShape,
    UnsupportedValue,
    ResourceLimit,
}
type Result<T> = std::result::Result<T, TypedEncodeError>;

pub(crate) fn encode_typed(value: &Value) -> Result<Node> {
    let Value::Typed(value) = value else {
        return Err(TypedEncodeError::WrongValue);
    };
    let mut remaining = MAX_VISITS;
    typed(value, 0, &mut remaining, None)
}

#[allow(
    dead_code,
    reason = "C7 backend awaits separately reviewed consumer integration"
)]
pub(crate) fn encode_typed_with_index(index: &crate::index::Index, value: &Value) -> Result<Node> {
    encode_typed_with_predefined(index, &PredefinedContext::default(), value)
}

pub(crate) fn encode_typed_with_predefined(
    index: &crate::index::Index,
    predefined: &PredefinedContext<'_>,
    value: &Value,
) -> Result<Node> {
    let Value::Typed(value) = value else {
        return Err(TypedEncodeError::WrongValue);
    };
    let mut remaining = MAX_VISITS;
    typed_with(value, 0, &mut remaining, Some(index), predefined)
}

fn typed(
    value: &TypedValue,
    depth: usize,
    remaining: &mut usize,
    index: Option<&crate::index::Index>,
) -> Result<Node> {
    typed_with(
        value,
        depth,
        remaining,
        index,
        &PredefinedContext::default(),
    )
}

fn typed_with(
    value: &TypedValue,
    depth: usize,
    remaining: &mut usize,
    index: Option<&crate::index::Index>,
    predefined: &PredefinedContext<'_>,
) -> Result<Node> {
    if depth > MAX_DEPTH || *remaining == 0 {
        return Err(TypedEncodeError::ResourceLimit);
    }
    *remaining -= 1;
    let tagged = |tag: &str, argument: Node| Node::list(vec![Node::str(tag), argument]);
    Ok(match value {
        TypedValue::Nil => Node::list(vec![Node::str("U")]),
        TypedValue::Bool(value) => tagged("B", Node::int(i64::from(*value))),
        TypedValue::Str(text) => {
            let literal = encode_scalar(
                &PropSpec {
                    codec: Codec::Str,
                    ..Default::default()
                },
                &Value::Text(text.clone()),
                None,
            )
            .map_err(|_| TypedEncodeError::InvalidShape)?;
            tagged("S", literal)
        }
        TypedValue::Number(text) => {
            if !number_literal(text) {
                return Err(TypedEncodeError::InvalidShape);
            }
            tagged("N", Node::atom(text))
        }
        TypedValue::Date(text) => {
            let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
            if text.len() != 19
                || text.bytes().enumerate().any(|(i, b)| {
                    separators
                        .iter()
                        .find(|(position, _)| *position == i)
                        .map_or(!b.is_ascii_digit(), |(_, expected)| b != *expected)
                })
            {
                return Err(TypedEncodeError::InvalidShape);
            }
            tagged(
                "D",
                Node::atom(
                    text.chars()
                        .filter(char::is_ascii_digit)
                        .collect::<String>(),
                ),
            )
        }
        TypedValue::EmptyTypeDescription => {
            special(TYPE_DESCRIPTION, Node::list(vec![Node::str("Pattern")]))?
        }
        TypedValue::FixedArray(values) => {
            if values.len() > *remaining {
                return Err(TypedEncodeError::ResourceLimit);
            }
            let count = i64::try_from(values.len()).map_err(|_| TypedEncodeError::ResourceLimit)?;
            let capacity = values
                .len()
                .checked_add(1)
                .ok_or(TypedEncodeError::ResourceLimit)?;
            let mut out = Vec::new();
            out.try_reserve_exact(capacity)
                .map_err(|_| TypedEncodeError::ResourceLimit)?;
            out.push(Node::int(count));
            let child_depth = depth
                .checked_add(1)
                .ok_or(TypedEncodeError::ResourceLimit)?;
            for child in values {
                out.push(typed_with(
                    child,
                    child_depth,
                    remaining,
                    index,
                    predefined,
                )?);
            }
            special(FIXED_ARRAY, Node::list(out))?
        }
        TypedValue::DesignTimeRef(text) if text.is_empty() => special(
            "5c14e26f-099b-4d37-84a6-b433d87400da",
            Node::list(vec![
                Node::int(0),
                Node::guid(Guid::from_bytes([0; 16])),
                Node::guid(Guid::from_bytes([0; 16])),
            ]),
        )?,
        TypedValue::DesignTimeRef(text) => {
            let index = index.ok_or(TypedEncodeError::UnsupportedValue)?;
            let leaf = Value::Typed(TypedValue::DesignTimeRef(text.clone()));
            if index.names.values().any(|name| name == text) {
                super::encode_enum_dtref::encode_enum_dtref(index, &leaf)
                    .map_err(|_| TypedEncodeError::UnsupportedValue)?
            } else if is_predefined_shape(text) {
                encode_predefined_dtref(index, predefined, &leaf)
                    .map_err(|_| TypedEncodeError::UnsupportedValue)?
            } else {
                super::encode_named_emptyref::encode_named_emptyref(index, &leaf)
                    .map_err(|_| TypedEncodeError::UnsupportedValue)?
            }
        }
    })
}

fn special(id: &str, value: Node) -> Result<Node> {
    let id = Guid::parse(id).ok_or(TypedEncodeError::InvalidShape)?;
    Ok(Node::list(vec![Node::str("#"), Node::guid(id), value]))
}

fn number_literal(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let mut parts = unsigned.split(['e', 'E']);
    let mantissa = parts.next().unwrap_or_default();
    let exponent = parts.next();
    mantissa.bytes().any(|b| b.is_ascii_digit())
        && mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && mantissa.bytes().filter(|b| *b == b'.').count() <= 1
        && exponent.is_none_or(|text| {
            let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
            !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit())
        })
        && parts.next().is_none()
}
