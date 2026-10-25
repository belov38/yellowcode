use super::codecs::{self, Ctx, Owner};
use super::name_lookup::NameLookup;
use crate::{
    index::Index,
    model::{Value, XmlElem},
};
use brace::{Guid, Node};
use mdschema::Schema;
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CpLinksEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    UnresolvedField,
    AmbiguousField,
    ResourceLimit,
}
fn field(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &Owner,
    wanted: &str,
) -> Result<Node, CpLinksEncodeError> {
    let found = resolve(ctx, names, owner, wanted);
    #[cfg(test)]
    if super::name_lookup::oracle::checking() {
        assert_eq!(found, tests::field_by_enumeration(ctx, owner, wanted));
        super::name_lookup::oracle::checked(0);
    }
    found
}
fn resolve(
    ctx: &Ctx<'_>,
    names: &NameLookup<'_>,
    owner: &Owner,
    wanted: &str,
) -> Result<Node, CpLinksEncodeError> {
    let mut found: Option<Node> = None;
    let mut consider = |candidate: Node| -> Result<(), CpLinksEncodeError> {
        if codecs::field_path(ctx, owner, std::slice::from_ref(&candidate)).as_deref()
            == Some(wanted)
        {
            if found
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return Err(CpLinksEncodeError::AmbiguousField);
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
    found.ok_or(CpLinksEncodeError::UnresolvedField)
}
fn leaf<'a>(e: &'a XmlElem, name: &str, data_path: bool) -> Result<&'a str, CpLinksEncodeError> {
    if e.name != name || e.expanded || !e.children.is_empty() {
        return Err(CpLinksEncodeError::InvalidShape);
    }
    if data_path {
        let [(key, kind)] = e.attrs.as_slice() else {
            return Err(CpLinksEncodeError::InvalidShape);
        };
        if key != "xsi:type" || kind != "xs:string" {
            return Err(CpLinksEncodeError::InvalidShape);
        }
    } else if !e.attrs.is_empty() {
        return Err(CpLinksEncodeError::InvalidShape);
    }
    e.text.as_deref().ok_or(CpLinksEncodeError::InvalidShape)
}
pub(crate) fn encode_cplinks(
    schema: &'static Schema,
    index: &Index,
    owner: &Owner,
    tag: &str,
    value: &Value,
    prefix: &Node,
) -> Result<Node, CpLinksEncodeError> {
    use CpLinksEncodeError::*;
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.name != tag || root.expanded || root.text.is_some() || !root.attrs.is_empty() {
        return Err(InvalidShape);
    }
    if matches!(prefix, Node::List(_)) {
        return Err(InvalidPrefix);
    }
    let n = root.children.len();
    if n > 100_000 {
        return Err(ResourceLimit);
    }
    let count = i64::try_from(n).map_err(|_| ResourceLimit)?;
    let capacity = n
        .checked_mul(5)
        .and_then(|x| x.checked_add(2))
        .ok_or(ResourceLimit)?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity).map_err(|_| ResourceLimit)?;
    let predef = Default::default();
    let ctx = Ctx {
        schema,
        index,
        predef: &predef,
    };
    let names = std::cell::OnceCell::new();
    out.push(prefix.clone());
    out.push(Node::int(count));
    for link in &root.children {
        if link.name != "xr:Link" || link.expanded || link.text.is_some() || !link.attrs.is_empty()
        {
            return Err(InvalidShape);
        }
        let [name, path, change] = link.children.as_slice() else {
            return Err(InvalidShape);
        };
        let name = leaf(name, "xr:Name", false)?;
        let path = leaf(path, "xr:DataPath", true)?;
        let change = match leaf(change, "xr:ValueChange", false)? {
            "DontChange" => 1,
            "Clear" => 0,
            _ => return Err(InvalidShape),
        };
        let names = NameLookup::once(&names, index).ok_or(ResourceLimit)?;
        let segment = field(&ctx, names, owner, path)?;
        let parent = names
            .tabular_parent(&ctx, owner, path, &segment)
            .map_err(|()| AmbiguousField)?;
        out.push(Node::str(name));
        out.push(Node::int(if parent.is_some() { 2 } else { 1 }));
        out.extend(parent);
        out.push(segment);
        out.push(Node::int(change));
    }
    Ok(Node::list(out))
}
