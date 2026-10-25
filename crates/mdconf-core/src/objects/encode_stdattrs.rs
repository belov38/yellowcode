use super::{
    codecs::Owner,
    encode_cparams::encode_cparams_with_predefined,
    encode_cplinks::encode_cplinks,
    encode_linkbytype::encode_linkbytype,
    encode_predefined_dtref::PredefinedContext,
    encode_reference::ReferenceContext,
    encode_scalar::encode_scalar,
    encode_typed::{encode_typed, encode_typed_with_predefined},
    overlay,
};
use crate::model::{Prop, Value};
use brace::{Guid, Node};
use mdschema::{BagProp, Codec, PropSpec, Schema};

const MAX_RECORDS: usize = 1000;
const MAX_TOTAL_PROPS: usize = 1000;
const MAX_TOTAL_LSTR_ITEMS: usize = 1000;
const MAX_TOTAL_TYPED_NODES: usize = 1000;
const MAX_TYPED_DEPTH: usize = 128;
const MAX_TOTAL_XML_NODES: usize = 1000;
const MAX_XML_DEPTH: usize = 128;
const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StdAttrsEncodeError {
    WrongValue,
    NonemptyProps,
    UnknownName,
    AmbiguousName,
    InvalidPrefix,
    ResourceLimit,
}
#[derive(Clone, Copy)]
pub(crate) struct StdAttrRecordPrefixes<'a> {
    pub(crate) class: &'a Node,
    pub(crate) bag: &'a Node,
}
pub(crate) struct StdAttrsPrefixes<'a> {
    pub(crate) outer: &'a Node,
    pub(crate) inner: &'a Node,
    pub(crate) records: Vec<StdAttrRecordPrefixes<'a>>,
}
impl<'a> StdAttrsPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node, count: usize) -> Result<Self, StdAttrsEncodeError> {
        use StdAttrsEncodeError::*;
        if count > MAX_RECORDS {
            return Err(ResourceLimit);
        }
        let outer = raw.as_list().ok_or(InvalidPrefix)?;
        let inner = outer.get(1).and_then(Node::as_list).ok_or(InvalidPrefix)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(count)
            .map_err(|_| ResourceLimit)?;
        for i in 0..count {
            let pos = i
                .checked_mul(3)
                .and_then(|n| n.checked_add(3))
                .ok_or(ResourceLimit)?;
            let class = inner.get(pos).ok_or(InvalidPrefix)?;
            let bag = inner
                .get(pos.checked_add(1).ok_or(ResourceLimit)?)
                .and_then(Node::as_list)
                .and_then(|n| n.first())
                .ok_or(InvalidPrefix)?;
            records.push(StdAttrRecordPrefixes { class, bag });
        }
        let result = Self {
            outer: outer.first().ok_or(InvalidPrefix)?,
            inner: inner.first().ok_or(InvalidPrefix)?,
            records,
        };
        result.validate(count)?;
        Ok(result)
    }
    fn validate(&self, count: usize) -> Result<(), StdAttrsEncodeError> {
        if self.records.len() != count
            || matches!(self.outer, Node::List(_))
            || matches!(self.inner, Node::List(_))
            || self
                .records
                .iter()
                .any(|r| matches!(r.class, Node::List(_)) || matches!(r.bag, Node::List(_)))
        {
            return Err(StdAttrsEncodeError::InvalidPrefix);
        }
        Ok(())
    }
}

pub(crate) struct StdAttrsSource<'a> {
    pub(crate) prefixes: StdAttrsPrefixes<'a>,
    bags: Vec<Vec<(&'a str, &'a Node)>>,
    ids: Vec<&'a str>,
}
impl<'a> StdAttrsSource<'a> {
    #[allow(
        dead_code,
        reason = "C7 backend awaits separately reviewed consumer integration"
    )]
    pub(crate) fn from_source(raw: &'a Node, count: usize) -> Result<Self, StdAttrsEncodeError> {
        use StdAttrsEncodeError::*;
        let prefixes = StdAttrsPrefixes::from_raw(raw, count)?;
        let inner = raw
            .as_list()
            .and_then(|outer| outer.get(1))
            .and_then(Node::as_list)
            .ok_or(InvalidPrefix)?;
        let mut bags = Vec::new();
        bags.try_reserve_exact(count).map_err(|_| ResourceLimit)?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(count).map_err(|_| ResourceLimit)?;
        let mut total = 0usize;
        for i in 0..count {
            let pos = i
                .checked_mul(3)
                .and_then(|n| n.checked_add(4))
                .ok_or(ResourceLimit)?;
            let id = pos
                .checked_sub(2)
                .and_then(|at| inner.get(at))
                .and_then(Node::as_list)
                .and_then(|key| key.first())
                .and_then(Node::text)
                .ok_or(InvalidPrefix)?;
            ids.push(id);
            let bag = inner
                .get(pos)
                .and_then(Node::as_list)
                .ok_or(InvalidPrefix)?;
            let k = bag
                .get(1)
                .and_then(super::codecs::count)
                .ok_or(InvalidPrefix)?;
            total = total.checked_add(k).ok_or(ResourceLimit)?;
            if total > MAX_TOTAL_PROPS {
                return Err(ResourceLimit);
            }
            let needed = k
                .checked_mul(2)
                .and_then(|n| n.checked_add(2))
                .ok_or(ResourceLimit)?;
            let pairs = bag.get(2..needed).ok_or(InvalidPrefix)?;
            let mut entries = Vec::new();
            entries.try_reserve_exact(k).map_err(|_| ResourceLimit)?;
            for [guid, value] in pairs.as_chunks::<2>().0 {
                entries.push((guid.text().ok_or(InvalidPrefix)?, value));
            }
            bags.push(entries);
        }
        Ok(Self {
            prefixes,
            bags,
            ids,
        })
    }
}

fn resolve_id(
    schema: &Schema,
    kind: &str,
    name: &str,
    source: Option<&str>,
) -> Result<i64, StdAttrsEncodeError> {
    use StdAttrsEncodeError::*;
    let mut candidates: Vec<i64> = Vec::new();
    let keys = schema.standard_attributes(kind).map(|(key, _)| key).chain(
        overlay::STD_ATTRS
            .iter()
            .filter(move |(k, _)| *k == kind)
            .flat_map(|(_, entries)| entries.iter().map(|(key, _)| *key)),
    );
    for key in keys {
        let Ok(id) = key.parse::<i64>() else { continue };
        if id.to_string() != key {
            continue;
        }
        if schema
            .std_attr(kind, key)
            .or_else(|| overlay::std_attr(kind, key))
            != Some(name)
        {
            continue;
        }
        if !candidates.contains(&id) {
            candidates.try_reserve(1).map_err(|_| ResourceLimit)?;
            candidates.push(id);
        }
    }
    match candidates.as_slice() {
        [] => Err(UnknownName),
        [id] => Ok(*id),
        _ => source
            .and_then(|text| text.parse::<i64>().ok().filter(|id| id.to_string() == text))
            .filter(|id| candidates.contains(id))
            .ok_or(AmbiguousName),
    }
}

fn resolve_prop<'a>(
    schema: &'a Schema,
    prop: &Prop,
    structured: bool,
) -> Result<(&'a str, &'a BagProp), StdAttrsEncodeError> {
    use StdAttrsEncodeError::NonemptyProps;
    if !matches!(
        prop.codec,
        Codec::Bool | Codec::Str | Codec::LStr | Codec::Enum | Codec::Ref | Codec::TValue
    ) && !(structured && is_structured(prop.codec))
    {
        return Err(NonemptyProps);
    }
    let mut found: Option<(&str, &BagProp)> = None;
    for (id, _) in schema.bag_properties() {
        let Some(guid) = Guid::parse(id) else {
            continue;
        };
        if guid.to_string() != id {
            continue;
        }
        let Some(entry) = schema.bag_prop(id) else {
            continue;
        };
        if entry.xml != prop.name || entry.codec != prop.codec {
            continue;
        }
        if found.is_some_and(|(prior, _)| prior != id) {
            return Err(NonemptyProps);
        }
        found = Some((id, entry));
    }
    found.ok_or(NonemptyProps)
}

fn count_typed_nodes(
    root: &crate::model::TypedValue,
    remaining: &mut usize,
) -> Result<(), StdAttrsEncodeError> {
    use StdAttrsEncodeError::ResourceLimit;
    let mut pending = Vec::new();
    pending.try_reserve_exact(1).map_err(|_| ResourceLimit)?;
    pending.push((root, 0usize));
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_TYPED_DEPTH {
            return Err(ResourceLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(ResourceLimit)?;
        if let crate::model::TypedValue::FixedArray(children) = value {
            let available = remaining.checked_sub(pending.len()).ok_or(ResourceLimit)?;
            if children.len() > available {
                return Err(ResourceLimit);
            }
            let child_depth = depth.checked_add(1).ok_or(ResourceLimit)?;
            pending
                .try_reserve_exact(children.len())
                .map_err(|_| ResourceLimit)?;
            pending.extend(children.iter().rev().map(|child| (child, child_depth)));
        }
    }
    Ok(())
}

fn is_structured(codec: Codec) -> bool {
    matches!(codec, Codec::LinkByType | Codec::CpLinks | Codec::CParams)
}

fn count_xml_nodes(
    root: &crate::model::XmlElem,
    remaining: &mut usize,
) -> Result<(), StdAttrsEncodeError> {
    use StdAttrsEncodeError::ResourceLimit;
    let mut pending = Vec::new();
    pending.try_reserve_exact(1).map_err(|_| ResourceLimit)?;
    pending.push((root, 0usize));
    while let Some((element, depth)) = pending.pop() {
        if depth > MAX_XML_DEPTH {
            return Err(ResourceLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(ResourceLimit)?;
        let available = remaining.checked_sub(pending.len()).ok_or(ResourceLimit)?;
        if element.children.len() > available {
            return Err(ResourceLimit);
        }
        let child_depth = depth.checked_add(1).ok_or(ResourceLimit)?;
        pending
            .try_reserve_exact(element.children.len())
            .map_err(|_| ResourceLimit)?;
        pending.extend(
            element
                .children
                .iter()
                .rev()
                .map(|child| (child, child_depth)),
        );
    }
    Ok(())
}

fn encode_active_accounttype(
    xml: &str,
    element: &crate::model::XmlElem,
) -> Result<Node, StdAttrsEncodeError> {
    use StdAttrsEncodeError::NonemptyProps;
    let [(key, kind)] = element.attrs.as_slice() else {
        return Err(NonemptyProps);
    };
    if element.name.strip_prefix("xr:") != Some(xml)
        || element.expanded
        || !element.children.is_empty()
        || element.text.as_deref() != Some("Active")
        || key != "xsi:type"
        || kind != "ent:AccountType"
    {
        return Err(NonemptyProps);
    }
    Ok(Node::list(vec![
        Node::str("#"),
        Node::guid(Guid::parse("872f7198-7083-4e3e-b57e-a2a9802c769e").ok_or(NonemptyProps)?),
        Node::int(0),
    ]))
}

pub(crate) fn encode_stdattrs(
    schema: &Schema,
    kind: &str,
    value: &Value,
    prefixes: &StdAttrsPrefixes<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    encode_stdattrs_context(schema, kind, value, prefixes, references, Extra::default())
}

#[allow(
    dead_code,
    reason = "C7 backend awaits separately reviewed consumer integration"
)]
pub(crate) fn encode_stdattrs_with_index(
    index: &crate::index::Index,
    schema: &Schema,
    kind: &str,
    value: &Value,
    prefixes: &StdAttrsPrefixes<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    let extra = Extra {
        index: Some(index),
        ..Default::default()
    };
    encode_stdattrs_context(schema, kind, value, prefixes, references, extra)
}

#[allow(
    dead_code,
    reason = "C7 backend awaits separately reviewed consumer integration"
)]
pub(crate) fn encode_stdattrs_source(
    index: &crate::index::Index,
    schema: &Schema,
    kind: &str,
    value: &Value,
    source: &StdAttrsSource<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    encode_stdattrs_source_with_predefined(
        index,
        &PredefinedContext::default(),
        schema,
        kind,
        value,
        source,
        references,
    )
}

pub(crate) fn encode_stdattrs_source_with_predefined(
    index: &crate::index::Index,
    predefined: &PredefinedContext<'_>,
    schema: &Schema,
    kind: &str,
    value: &Value,
    source: &StdAttrsSource<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    let extra = Extra {
        index: Some(index),
        predefined: *predefined,
        bags: Some(&source.bags),
        ids: Some(&source.ids),
        owned: None,
    };
    encode_stdattrs_context(schema, kind, value, &source.prefixes, references, extra)
}

#[allow(
    dead_code,
    reason = "C7 backend awaits separately reviewed consumer integration"
)]
pub(crate) fn encode_stdattrs_owned_source(
    index: &crate::index::Index,
    schema: &'static Schema,
    owner: &Owner,
    value: &Value,
    source: &StdAttrsSource<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    encode_stdattrs_owned_source_with_predefined(
        index,
        &PredefinedContext::default(),
        schema,
        owner,
        value,
        source,
        references,
    )
}

pub(crate) fn encode_stdattrs_owned_source_with_predefined(
    index: &crate::index::Index,
    predefined: &PredefinedContext<'_>,
    schema: &'static Schema,
    owner: &Owner,
    value: &Value,
    source: &StdAttrsSource<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    encode_stdattrs_owned_source_named(
        index,
        predefined,
        schema,
        &owner.kind,
        owner,
        value,
        source,
        references,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_stdattrs_owned_source_named(
    index: &crate::index::Index,
    predefined: &PredefinedContext<'_>,
    schema: &'static Schema,
    kind: &str,
    owner: &Owner,
    value: &Value,
    source: &StdAttrsSource<'_>,
    references: &ReferenceContext,
) -> Result<Node, StdAttrsEncodeError> {
    let extra = Extra {
        index: Some(index),
        predefined: *predefined,
        bags: Some(&source.bags),
        ids: Some(&source.ids),
        owned: Some((schema, owner)),
    };
    encode_stdattrs_context(schema, kind, value, &source.prefixes, references, extra)
}

#[derive(Clone, Copy, Default)]
struct Extra<'a> {
    index: Option<&'a crate::index::Index>,
    predefined: PredefinedContext<'a>,
    bags: Option<&'a [Vec<(&'a str, &'a Node)>]>,
    ids: Option<&'a [&'a str]>,
    owned: Option<(&'static Schema, &'a Owner)>,
}

type Wrapper<'a> = Option<(&'a Node, &'a Node)>;

fn structured_payload(context: &Node) -> Result<(Wrapper<'_>, &[Node]), StdAttrsEncodeError> {
    use StdAttrsEncodeError::NonemptyProps;
    let atom = |n: &Node| !matches!(n, Node::List(_));
    let items = context.as_list().ok_or(NonemptyProps)?;
    match items {
        [hash, kind, Node::List(payload)]
            if atom(hash)
                && hash.text() == Some("#")
                && atom(kind)
                && !matches!(kind.text(), Some(T_DTREF | T_FIXARR)) =>
        {
            Ok((Some((hash, kind)), payload))
        }
        _ if !items
            .first()
            .is_some_and(|n| atom(n) && n.text() == Some("#")) =>
        {
            Ok((None, items))
        }
        _ => Err(NonemptyProps),
    }
}

fn wrap_bag_value(codec: Codec, context: &Node, value: Node) -> Result<Node, StdAttrsEncodeError> {
    use StdAttrsEncodeError::NonemptyProps;
    let atom = |n: &Node| !matches!(n, Node::List(_));
    let wrapper = |hash: &Node, kind: &Node| atom(hash) && hash.text() == Some("#") && atom(kind);
    let Node::List(items) = context else {
        return if codec == Codec::LStr {
            Err(NonemptyProps)
        } else {
            Ok(value)
        };
    };
    match (codec, items.as_slice()) {
        (Codec::TValue, _) => Ok(value),
        (Codec::Bool | Codec::Str, [head, _]) if atom(head) => {
            Ok(Node::list(vec![head.clone(), value]))
        }
        (Codec::Enum | Codec::Ref | Codec::LStr, [hash, kind, payload]) if wrapper(hash, kind) => {
            if matches!(kind.text(), Some(T_DTREF | T_FIXARR)) {
                return Err(NonemptyProps);
            }
            let inner = match (codec, payload) {
                (Codec::LStr, _) => value,
                (_, Node::List(payload)) => match payload.as_slice() {
                    [head, _] if atom(head) => Node::list(vec![head.clone(), value]),
                    _ => return Err(NonemptyProps),
                },
                _ => value,
            };
            Ok(Node::list(vec![hash.clone(), kind.clone(), inner]))
        }
        (Codec::Enum | Codec::Ref, [head, _]) if atom(head) => {
            Ok(Node::list(vec![head.clone(), value]))
        }
        (Codec::LStr, _)
            if !items
                .first()
                .is_some_and(|n| atom(n) && n.text() == Some("#")) =>
        {
            Ok(value)
        }
        _ => Err(NonemptyProps),
    }
}

fn encode_stdattrs_context(
    schema: &Schema,
    kind: &str,
    value: &Value,
    prefixes: &StdAttrsPrefixes<'_>,
    references: &ReferenceContext,
    extra: Extra<'_>,
) -> Result<Node, StdAttrsEncodeError> {
    use StdAttrsEncodeError::*;
    let Extra {
        index,
        predefined,
        bags,
        ids,
        owned,
    } = extra;
    let Value::StandardAttributes(attrs) = value else {
        return Err(WrongValue);
    };
    let n = attrs.len();
    if n > MAX_RECORDS {
        return Err(ResourceLimit);
    }
    prefixes.validate(n)?;
    let mut total_props = 0usize;
    let mut total_languages = 0usize;
    let mut remaining_typed = MAX_TOTAL_TYPED_NODES;
    let mut remaining_xml = MAX_TOTAL_XML_NODES;
    for attr in attrs {
        total_props = total_props
            .checked_add(attr.props.len())
            .ok_or(ResourceLimit)?;
        for prop in &attr.props {
            if prop.codec == Codec::TValue
                && let Value::Typed(value) = &prop.value
            {
                count_typed_nodes(value, &mut remaining_typed)?;
            }
            if owned.is_some()
                && is_structured(prop.codec)
                && let Value::Xml(root) = &prop.value
            {
                count_xml_nodes(root, &mut remaining_xml)?;
            }
            if let Value::LStr(items) = &prop.value {
                total_languages = total_languages
                    .checked_add(items.0.len())
                    .ok_or(ResourceLimit)?;
            }
        }
    }
    if total_props > MAX_TOTAL_PROPS || total_languages > MAX_TOTAL_LSTR_ITEMS {
        return Err(ResourceLimit);
    }
    let mut records = Vec::new();
    records.try_reserve_exact(n).map_err(|_| ResourceLimit)?;
    let mut contexts = Vec::new();
    let mut structured = Vec::new();
    for (i, attr) in attrs.iter().enumerate() {
        let id = resolve_id(
            schema,
            kind,
            &attr.name,
            ids.and_then(|ids| ids.get(i)).copied(),
        )?;
        let bag = bags.and_then(|bags| bags.get(i));
        let capacity = attr.props.len().checked_mul(2).ok_or(ResourceLimit)?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(capacity)
            .map_err(|_| ResourceLimit)?;
        for (j, prop) in attr.props.iter().enumerate() {
            let (guid, entry) = resolve_prop(schema, prop, owned.is_some())?;
            let context = bag
                .and_then(|bag| bag.get(j))
                .filter(|(raw_guid, _)| *raw_guid == guid)
                .map(|(_, context)| *context);
            if is_structured(entry.codec) {
                structured.try_reserve(1).map_err(|_| ResourceLimit)?;
                structured.push((
                    records.len(),
                    payload.len() + 1,
                    entry,
                    &prop.value,
                    context,
                ));
                payload.push(Node::atom(guid));
                payload.push(Node::int(0));
                continue;
            }
            if let Some(context) = context {
                contexts.try_reserve(1).map_err(|_| ResourceLimit)?;
                contexts.push((records.len(), payload.len() + 1, entry.codec, context));
            }
            let spec = PropSpec {
                codec: entry.codec,
                map: entry.map.clone(),
                ..Default::default()
            };
            let node = if entry.codec == Codec::Ref {
                references.encode(&prop.value).map_err(|_| NonemptyProps)?
            } else if entry.codec == Codec::TValue {
                if let Value::Xml(element) = &prop.value {
                    encode_active_accounttype(&entry.xml, element)?
                } else {
                    match index {
                        Some(index) => {
                            encode_typed_with_predefined(index, &predefined, &prop.value)
                        }
                        None => encode_typed(&prop.value),
                    }
                    .map_err(|_| NonemptyProps)?
                }
            } else {
                encode_scalar(&spec, &prop.value, None).map_err(|_| NonemptyProps)?
            };
            payload.push(Node::atom(guid));
            payload.push(node);
        }
        records.push((id, payload));
    }
    for (record, slot, entry, value, context) in structured {
        let (schema, owner) = owned.ok_or(NonemptyProps)?;
        let index = index.ok_or(NonemptyProps)?;
        let (wrapper, payload) = structured_payload(context.ok_or(NonemptyProps)?)?;
        let prefix = payload.first().ok_or(NonemptyProps)?;
        let tag = format!("xr:{}", entry.xml);
        let node = match entry.codec {
            Codec::LinkByType => {
                let empty_tail = match value {
                    Value::Xml(root) if root.children.is_empty() => payload.get(2),
                    _ => None,
                };
                encode_linkbytype(schema, index, owner, &tag, value, prefix, empty_tail)
                    .map_err(|_| NonemptyProps)?
            }
            Codec::CpLinks => encode_cplinks(schema, index, owner, &tag, value, prefix)
                .map_err(|_| NonemptyProps)?,
            _ => encode_cparams_with_predefined(index, &predefined, &tag, value, prefix)
                .map_err(|_| NonemptyProps)?,
        };
        let target = records
            .get_mut(record)
            .and_then(|(_, payload)| payload.get_mut(slot))
            .ok_or(ResourceLimit)?;
        *target = match wrapper {
            Some((hash, kind)) => Node::list(vec![hash.clone(), kind.clone(), node]),
            None => node,
        };
    }
    for (record, slot, codec, context) in contexts {
        let target = records
            .get_mut(record)
            .and_then(|(_, payload)| payload.get_mut(slot))
            .ok_or(ResourceLimit)?;
        let value = std::mem::replace(target, Node::int(0));
        *target = wrap_bag_value(codec, context, value)?;
    }
    let capacity = n
        .checked_mul(3)
        .and_then(|x| x.checked_add(2))
        .ok_or(ResourceLimit)?;
    let mut outer = Vec::new();
    outer.try_reserve_exact(2).map_err(|_| ResourceLimit)?;
    let mut inner = Vec::new();
    inner
        .try_reserve_exact(capacity)
        .map_err(|_| ResourceLimit)?;
    inner.push(prefixes.inner.clone());
    inner.push(Node::int(i64::try_from(n).map_err(|_| ResourceLimit)?));
    for ((id, payload), context) in records.into_iter().zip(&prefixes.records) {
        let mut key = Vec::new();
        key.try_reserve_exact(1).map_err(|_| ResourceLimit)?;
        key.push(Node::int(id));
        let capacity = payload.len().checked_add(2).ok_or(ResourceLimit)?;
        let count = i64::try_from(payload.len() / 2).map_err(|_| ResourceLimit)?;
        let mut bag = Vec::new();
        bag.try_reserve_exact(capacity).map_err(|_| ResourceLimit)?;
        bag.push(context.bag.clone());
        bag.push(Node::int(count));
        bag.extend(payload);
        inner.push(Node::list(key));
        inner.push(context.class.clone());
        inner.push(Node::list(bag));
    }
    outer.push(prefixes.outer.clone());
    outer.push(Node::list(inner));
    Ok(Node::list(outer))
}
