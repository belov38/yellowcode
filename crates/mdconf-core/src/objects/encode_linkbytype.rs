use super::codecs::{self, Ctx, Owner};
use super::name_lookup::NameLookup;
use crate::{
    index::Index,
    model::{Value, XmlElem},
};
use brace::{Guid, Node};
use mdschema::Schema;
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LinkByTypeEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    MissingEmptyTail,
    UnresolvedField,
    AmbiguousField,
    ResourceLimit,
}
fn field(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &Owner,
    wanted: &str,
) -> Result<Node, LinkByTypeEncodeError> {
    let found = resolve(ctx, names, owner, wanted);
    #[cfg(test)]
    if super::name_lookup::oracle::checking() {
        assert_eq!(found, tests::field_by_enumeration(ctx, owner, wanted));
        super::name_lookup::oracle::checked(1);
    }
    found
}
fn resolve(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &Owner,
    wanted: &str,
) -> Result<Node, LinkByTypeEncodeError> {
    let mut found: Option<Node> = None;
    let mut consider = |candidate: Node| -> Result<(), LinkByTypeEncodeError> {
        if codecs::field_path(ctx, owner, std::slice::from_ref(&candidate)).as_deref()
            == Some(wanted)
        {
            if found
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return Err(LinkByTypeEncodeError::AmbiguousField);
            }
            found = Some(candidate);
        }
        Ok(())
    };
    let shown = wanted
        .strip_prefix("0:")
        .and_then(Guid::parse)
        .filter(|id| ctx.index.names.contains_key(id));
    for id in names.ids(wanted).iter().copied().chain(shown) {
        consider(Node::list(vec![Node::int(0), Node::guid(id)]))?;
    }
    for (key, _) in ctx.schema.standard_attributes(&owner.kind) {
        let Ok(code) = key.parse::<i64>() else {
            continue;
        };
        if code.to_string() == key {
            consider(Node::list(vec![Node::int(code)]))?;
        }
    }
    found.ok_or(LinkByTypeEncodeError::UnresolvedField)
}
fn leaf<'a>(e: &'a XmlElem, name: &str) -> Result<&'a str, LinkByTypeEncodeError> {
    if e.name != name || e.expanded || !e.attrs.is_empty() || !e.children.is_empty() {
        return Err(LinkByTypeEncodeError::InvalidShape);
    }
    e.text.as_deref().ok_or(LinkByTypeEncodeError::InvalidShape)
}
pub(crate) fn encode_linkbytype(
    schema: &'static Schema,
    index: &Index,
    owner: &Owner,
    tag: &str,
    value: &Value,
    prefix: &Node,
    empty_tail: Option<&Node>,
) -> Result<Node, LinkByTypeEncodeError> {
    use LinkByTypeEncodeError::*;
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.name != tag || root.expanded || root.text.is_some() || !root.attrs.is_empty() {
        return Err(InvalidShape);
    }
    if matches!(prefix, Node::List(_)) {
        return Err(InvalidPrefix);
    }
    let mut out = Vec::new();
    if root.children.is_empty() {
        let tail = empty_tail.ok_or(MissingEmptyTail)?;
        if matches!(tail, Node::List(_)) {
            return Err(InvalidPrefix);
        }
        out.try_reserve_exact(3).map_err(|_| ResourceLimit)?;
        out.extend([prefix.clone(), Node::int(0), tail.clone()]);
    } else {
        let [path, item] = root.children.as_slice() else {
            return Err(InvalidShape);
        };
        let path = leaf(path, "xr:DataPath")?;
        let item = leaf(item, "xr:LinkItem")?;
        let predef = Default::default();
        let ctx = Ctx {
            schema,
            index,
            predef: &predef,
        };
        let names = NameLookup::new(index).ok_or(ResourceLimit)?;
        let segment = field(&ctx, &names, owner, path)?;
        let parent = names
            .tabular_parent(&ctx, owner, path, &segment)
            .map_err(|()| AmbiguousField)?;
        out.try_reserve_exact(5).map_err(|_| ResourceLimit)?;
        out.push(prefix.clone());
        out.push(Node::int(if parent.is_some() { 2 } else { 1 }));
        out.extend(parent);
        out.extend([segment, Node::atom(item)]);
    }
    Ok(Node::list(out))
}
