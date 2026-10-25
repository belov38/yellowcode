use super::bags::{section_attributes_elem, std_attr_list};
use super::codecs::{Ctx, Owner};
use super::decode::description_value;
use super::encode_predefined_dtref::PredefinedContext;
use super::encode_reference::ReferenceContext;
use super::encode_scalar::encode_scalar;
use super::encode_stdattrs::{StdAttrsSource, encode_stdattrs_owned_source_named};
use super::overlay::std_section_id;
use crate::index::Index;
use crate::model::{LocalString, Value, XmlElem};
use brace::Node;
use mdschema::{Codec, PropSpec, Schema};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StdTabularEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnknownSection,
    SectionCount,
    SourceAttributes,
    AttributesMismatch,
    Field,
    StdAttrs,
}
use StdTabularEncodeError::*;
type Result<T> = std::result::Result<T, StdTabularEncodeError>;

fn scalar(n: &Node) -> Result<&Node> {
    if matches!(n, Node::List(_)) {
        Err(InvalidPrefix)
    } else {
        Ok(n)
    }
}

fn local_string(e: &XmlElem) -> Result<Value> {
    if !e.attrs.is_empty() || e.text.is_some() {
        return Err(InvalidShape);
    }
    let mut items = Vec::new();
    for item in &e.children {
        let [lang, content] = item.children.as_slice() else {
            return Err(InvalidShape);
        };
        if item.name != "v8:item" || lang.name != "v8:lang" || content.name != "v8:content" {
            return Err(InvalidShape);
        }
        items.push((
            lang.text.clone().unwrap_or_default(),
            content.text.clone().unwrap_or_default(),
        ));
    }
    Ok(Value::LStr(LocalString(items)))
}

fn field(codec: Codec, value: &Value) -> Result<Node> {
    let spec = PropSpec {
        codec,
        ..Default::default()
    };
    encode_scalar(&spec, value, None).map_err(|_| Field)
}

fn child<'a>(e: &'a XmlElem, k: usize, name: &str) -> Result<&'a XmlElem> {
    let c = e.children.get(k).ok_or(InvalidShape)?;
    if c.name != name {
        return Err(InvalidShape);
    }
    Ok(c)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_std_tabular(
    schema: &'static Schema,
    index: &Index,
    predefined: &PredefinedContext<'_>,
    references: &ReferenceContext,
    owner: &Owner,
    value: &Value,
    raw: &Node,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.name != "StandardTabularSections" || !root.attrs.is_empty() || root.text.is_some() {
        return Err(InvalidShape);
    }
    let [flag, body] = raw.as_list().ok_or(InvalidShape)? else {
        return Err(InvalidShape);
    };
    if flag.text() != Some("1") {
        return Err(InvalidShape);
    }
    let body = body.as_list().ok_or(InvalidShape)?;
    let body0 = scalar(body.first().ok_or(InvalidShape)?)?;
    let n = root.children.len();
    let source_n: usize = body
        .get(1)
        .and_then(Node::text)
        .and_then(|t| t.parse().ok())
        .ok_or(InvalidShape)?;
    if source_n != n || body.len() < 2 + 2 * n {
        return Err(SectionCount);
    }
    let empty = HashMap::new();
    let ctx = Ctx {
        schema,
        index,
        predef: predefined.names().unwrap_or(&empty),
    };
    let mut out = vec![
        body0.clone(),
        Node::int(i64::try_from(n).map_err(|_| InvalidShape)?),
    ];
    for (i, section) in root.children.iter().enumerate() {
        let [(key, name)] = section.attrs.as_slice() else {
            return Err(InvalidShape);
        };
        if section.name != "xr:StandardTabularSection"
            || key != "name"
            || section.children.len() != 5
        {
            return Err(InvalidShape);
        }
        let id = std_section_id(&owner.kind, name).ok_or(UnknownSection)?;
        let src = body
            .get(3 + 2 * i)
            .and_then(Node::as_list)
            .ok_or(InvalidShape)?;
        let s0 = scalar(src.first().ok_or(InvalidShape)?)?;
        let src_attrs = src.get(5).and_then(Node::as_list).ok_or(InvalidShape)?;
        let synonym = local_string(child(section, 0, "xr:Synonym")?)?;
        let comment = child(section, 1, "xr:Comment")?;
        let tooltip = local_string(child(section, 2, "xr:ToolTip")?)?;
        let fill = match child(section, 3, "xr:FillChecking")?.text.as_deref() {
            Some("DontCheck") => 0,
            Some("ShowError") => 1,
            _ => return Err(InvalidShape),
        };
        let model_attrs = child(section, 4, "xr:StandardAttributes")?;
        let kind = format!("{}.{name}", owner.kind);
        let list = std_attr_list(&ctx, src_attrs, &kind, Some(owner)).ok_or(SourceAttributes)?;
        if &section_attributes_elem(list.clone()) != model_attrs {
            return Err(AttributesMismatch);
        }
        let typed = description_value(Value::StandardAttributes(list));
        let Value::StandardAttributes(attrs) = &typed else {
            return Err(StdAttrs);
        };
        let wrapper = Node::list(vec![Node::int(0), Node::list(src_attrs.to_vec())]);
        let source = StdAttrsSource::from_source(&wrapper, attrs.len()).map_err(|_| StdAttrs)?;
        let encoded = encode_stdattrs_owned_source_named(
            index, predefined, schema, &kind, owner, &typed, &source, references,
        )
        .map_err(|_| StdAttrs)?;
        let attrs_node = encoded
            .as_list()
            .and_then(|l| l.get(1))
            .cloned()
            .ok_or(StdAttrs)?;
        out.push(Node::atom(id));
        out.push(Node::list(vec![
            s0.clone(),
            field(Codec::LStr, &synonym)?,
            field(
                Codec::Str,
                &Value::Text(comment.text.clone().unwrap_or_default()),
            )?,
            Node::int(fill),
            Node::int(0),
            attrs_node,
            field(Codec::LStr, &tooltip)?,
        ]));
    }
    Ok(Node::list(vec![Node::int(1), Node::list(out)]))
}
