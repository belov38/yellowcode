use super::encode_predefined_dtref::PredefinedContext;
use super::encode_typed::{TypedEncodeError, encode_typed, encode_typed_with_predefined};
use crate::model::{TypedValue, Value, XmlElem};
use brace::Node;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CParamsEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnsupportedValue,
    ResourceLimit,
}
type Result<T> = std::result::Result<T, CParamsEncodeError>;
const MAX_ITEMS: usize = 100_000;
const MAX_VISITS: usize = 100_000;
const MAX_DEPTH: usize = 128;

fn empty(element: &XmlElem) -> bool {
    element.text.is_none() && element.children.is_empty() && !element.expanded
}

pub(super) fn typed(
    element: &XmlElem,
    tag: &str,
    depth: usize,
    remaining: &mut usize,
    index: Option<&crate::index::Index>,
) -> Result<TypedValue> {
    if depth > MAX_DEPTH || *remaining == 0 {
        return Err(CParamsEncodeError::ResourceLimit);
    }
    *remaining -= 1;
    if element.name != tag {
        return Err(CParamsEncodeError::InvalidShape);
    }
    let [(key, kind)] = element.attrs.as_slice() else {
        return Err(CParamsEncodeError::InvalidShape);
    };
    if key == "xsi:nil" {
        return if kind == "true" && empty(element) {
            Ok(TypedValue::Nil)
        } else {
            Err(CParamsEncodeError::InvalidShape)
        };
    }
    if key != "xsi:type" {
        return Err(CParamsEncodeError::InvalidShape);
    }
    match kind.as_str() {
        "v8:TypeDescription" => {
            if !empty(element) {
                return Err(CParamsEncodeError::InvalidShape);
            }
            Ok(TypedValue::EmptyTypeDescription)
        }
        "xr:DesignTimeRef" => {
            if element.expanded || !element.children.is_empty() {
                return Err(CParamsEncodeError::InvalidShape);
            }
            match element.text.as_deref() {
                Some("") => Ok(TypedValue::DesignTimeRef(String::new())),
                Some(text) if index.is_some() => Ok(TypedValue::DesignTimeRef(text.to_owned())),
                Some(_) => Err(CParamsEncodeError::UnsupportedValue),
                None => Err(CParamsEncodeError::InvalidShape),
            }
        }
        "v8:FixedArray" => {
            if element.text.is_some() || !element.expanded {
                return Err(CParamsEncodeError::InvalidShape);
            }
            if element.children.len() > *remaining {
                return Err(CParamsEncodeError::ResourceLimit);
            }
            let mut values = Vec::new();
            values
                .try_reserve_exact(element.children.len())
                .map_err(|_| CParamsEncodeError::ResourceLimit)?;
            for child in &element.children {
                values.push(typed(child, "v8:Value", depth + 1, remaining, index)?);
            }
            Ok(TypedValue::FixedArray(values))
        }
        "xs:string" | "xs:decimal" | "xs:boolean" | "xs:dateTime" => {
            if element.expanded || !element.children.is_empty() {
                return Err(CParamsEncodeError::InvalidShape);
            }
            let text = element
                .text
                .as_ref()
                .ok_or(CParamsEncodeError::InvalidShape)?;
            Ok(match kind.as_str() {
                "xs:string" => TypedValue::Str(text.clone()),
                "xs:decimal" => TypedValue::Number(text.clone()),
                "xs:dateTime" => TypedValue::Date(text.clone()),
                _ => match text.as_str() {
                    "true" => TypedValue::Bool(true),
                    "false" => TypedValue::Bool(false),
                    _ => return Err(CParamsEncodeError::InvalidShape),
                },
            })
        }
        _ => Err(CParamsEncodeError::UnsupportedValue),
    }
}

#[allow(
    dead_code,
    reason = "C7 backend awaits separately reviewed consumer integration"
)]
pub(crate) fn encode_cparams_with_index(
    index: &crate::index::Index,
    tag: &str,
    value: &Value,
    prefix: &Node,
) -> Result<Node> {
    encode_cparams_with_predefined(index, &PredefinedContext::default(), tag, value, prefix)
}

pub(crate) fn encode_cparams_with_predefined(
    index: &crate::index::Index,
    predefined: &PredefinedContext<'_>,
    tag: &str,
    value: &Value,
    prefix: &Node,
) -> Result<Node> {
    encode_cparams_context(tag, value, prefix, Some(index), predefined)
}

pub(crate) fn encode_cparams(tag: &str, value: &Value, prefix: &Node) -> Result<Node> {
    encode_cparams_context(tag, value, prefix, None, &PredefinedContext::default())
}

fn encode_cparams_context(
    tag: &str,
    value: &Value,
    prefix: &Node,
    index: Option<&crate::index::Index>,
    predefined: &PredefinedContext<'_>,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(CParamsEncodeError::WrongValue);
    };
    if root.name != tag || !root.attrs.is_empty() || root.text.is_some() || root.expanded {
        return Err(CParamsEncodeError::InvalidShape);
    }
    if matches!(prefix, Node::List(_)) {
        return Err(CParamsEncodeError::InvalidPrefix);
    }
    let n = root.children.len();
    if n > MAX_ITEMS {
        return Err(CParamsEncodeError::ResourceLimit);
    }
    let count = i64::try_from(n).map_err(|_| CParamsEncodeError::ResourceLimit)?;
    let capacity = n
        .checked_mul(2)
        .and_then(|n| n.checked_add(2))
        .ok_or(CParamsEncodeError::ResourceLimit)?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|_| CParamsEncodeError::ResourceLimit)?;
    out.push(prefix.clone());
    out.push(Node::int(count));
    let mut remaining = MAX_VISITS;
    for item in &root.children {
        if item.name != "app:item" || item.text.is_some() || item.expanded {
            return Err(CParamsEncodeError::InvalidShape);
        }
        let [(key, name)] = item.attrs.as_slice() else {
            return Err(CParamsEncodeError::InvalidShape);
        };
        if key != "name" {
            return Err(CParamsEncodeError::InvalidShape);
        }
        let [element] = item.children.as_slice() else {
            return Err(CParamsEncodeError::InvalidShape);
        };
        let value = Value::Typed(typed(element, "app:value", 0, &mut remaining, index)?);
        let encoded = match index {
            Some(index) => encode_typed_with_predefined(index, predefined, &value),
            None => encode_typed(&value),
        }
        .map_err(|error| match error {
            TypedEncodeError::ResourceLimit => CParamsEncodeError::ResourceLimit,
            TypedEncodeError::UnsupportedValue => CParamsEncodeError::UnsupportedValue,
            TypedEncodeError::WrongValue | TypedEncodeError::InvalidShape => {
                CParamsEncodeError::InvalidShape
            }
        })?;
        out.push(Node::str(name));
        out.push(encoded);
    }
    Ok(Node::list(out))
}
