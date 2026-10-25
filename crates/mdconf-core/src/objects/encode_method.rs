use super::{codecs, encode_scalar::encode_scalar};
use crate::{index::Index, model::Value};
use brace::Node;
use mdschema::{Codec, PropSpec};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EncodedMethod {
    pub(crate) module: Node,
    pub(crate) method: Node,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MethodEncodeError {
    WrongValue,
    UnresolvedMethod,
    AmbiguousMethod,
}

pub(crate) fn encode_method(
    index: &Index,
    value: &Value,
) -> Result<EncodedMethod, MethodEncodeError> {
    let Value::Text(text) = value else {
        return Err(MethodEncodeError::WrongValue);
    };
    let mut candidate = None;
    for (id, fullname) in &index.names {
        if fullname.is_empty() {
            continue;
        }
        let fullname = codecs::cf_text(fullname);
        let Some(suffix) = text
            .strip_prefix(fullname.as_ref())
            .and_then(|s| s.strip_prefix('.'))
        else {
            continue;
        };
        if candidate.replace((*id, suffix)).is_some() {
            return Err(MethodEncodeError::AmbiguousMethod);
        }
    }
    let (id, suffix) = candidate.ok_or(MethodEncodeError::UnresolvedMethod)?;
    let method = encode_scalar(
        &PropSpec {
            codec: Codec::Str,
            ..Default::default()
        },
        &Value::Text(suffix.into()),
        None,
    )
    .map_err(|_| MethodEncodeError::UnresolvedMethod)?;
    Ok(EncodedMethod {
        module: Node::guid(id),
        method,
    })
}
