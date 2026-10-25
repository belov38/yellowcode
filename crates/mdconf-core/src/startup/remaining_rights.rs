use super::{
    ManagedIndex,
    index::text,
    source_copy::copy_node,
    values::{append, description_text, empty},
};
use crate::{
    ConfigSettings,
    parts::{ObjectRights, Restriction, Right, RightTarget, RightValue, Rights},
};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
#[derive(Debug)]
pub(super) enum Error<'a> {
    Memory(BudgetError),
    Semantic(&'static str, Option<&'a str>),
}
impl From<BudgetError> for Error<'_> {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
type Result<'a, T> = std::result::Result<T, Error<'a>>;
fn bad<'a>() -> Error<'a> {
    Error::Semantic("malformed", None)
}
fn list(node: Option<&Node>) -> Result<'_, &[Node]> {
    node.and_then(Node::as_list).ok_or_else(bad)
}
fn txt(node: Option<&Node>) -> Result<'_, &str> {
    node.and_then(Node::text).ok_or_else(bad)
}
fn num(node: Option<&Node>) -> Result<'_, usize> {
    txt(node)?
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(bad)
}
fn counted<'a>(nodes: &'a [Node], from: usize, count: Option<&'a Node>) -> Result<'a, &'a [Node]> {
    let n = num(count)?;
    let tail = nodes.get(from..).unwrap_or_default();
    tail.get(..n.min(tail.len())).ok_or_else(bad)
}
fn flag(node: Option<&Node>) -> Result<'_, bool> {
    match txt(node)? {
        "1" => Ok(true),
        "0" | "-1" | "4294967295" => Ok(false),
        _ => Err(bad()),
    }
}
fn vec<T>(budget: &MemoryBudget, n: usize) -> std::result::Result<Charged<Vec<T>>, BudgetError> {
    Ok(BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?.into_charged_vec())
}
fn std_attr(kind: &str, id: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("stdattrs.json")?
        .get(kind)?
        .get(id)?
        .as_str()
}
fn key(target: &RightTarget, root: Guid) -> (Guid, u8, i32) {
    match *target {
        RightTarget::Configuration => (root, 0, 0),
        RightTarget::Object(id) | RightTarget::Child { id, .. } => (id, 0, 0),
        RightTarget::StandardAttribute { owner, id } => (owner, 1, id),
    }
}
struct Resolved<'a> {
    ordinal: usize,
    target: RightTarget,
    name: BudgetedText,
    raw: &'a Node,
}
fn resolve<'a>(
    index: &ManagedIndex,
    settings: &ConfigSettings,
    node: Option<&'a Node>,
    raw: &'a Node,
    ordinal: usize,
) -> Result<'a, Resolved<'a>> {
    let r = list(node)?;
    let id = r.get(1).ok_or_else(bad)?.text().and_then(Guid::parse);
    let standard = r.get(2).ok_or_else(bad)?.text() == Some("1");
    let id = id.ok_or(Error::Semantic("unresolved_object", None))?;
    let cfg = text(index.memory_budget(), &["Configuration.", &settings.name])?;
    let owner = if id == index.cfg_id {
        cfg.as_str()
    } else {
        index
            .names
            .get(&id)
            .map(|n| n.as_str())
            .ok_or(Error::Semantic("unresolved_object", None))?
    };
    let (target, name) = if standard {
        let kind = if owner.split('.').count() >= 4 {
            owner.rsplit('.').nth(1)
        } else {
            owner.split('.').next()
        };
        let sid = txt(r.get(3).and_then(|n| n.at(0)))?;
        let attr = kind
            .and_then(|kind| std_attr(kind, sid))
            .ok_or(Error::Semantic("unresolved_object", None))?;
        let sid = sid.trim().parse().map_err(|_| bad())?;
        (
            RightTarget::StandardAttribute { owner: id, id: sid },
            text(index.memory_budget(), &[owner, ".StandardAttribute.", attr])?,
        )
    } else {
        let target = if id == index.cfg_id {
            RightTarget::Configuration
        } else if index.objs.get(&id).is_some() {
            RightTarget::Object(id)
        } else {
            RightTarget::Child {
                id,
                field: r.get(3).and_then(Node::text) == Some("1"),
            }
        };
        (target, text(index.memory_budget(), &[owner])?)
    };
    Ok(Resolved {
        ordinal,
        target,
        name,
        raw,
    })
}
struct RawRls<'a> {
    condition: &'a str,
    fields: &'a [Node],
}
type Restrictions<'a> = BudgetedVec<(&'a str, BudgetedVec<RawRls<'a>>)>;
fn rightlist<'a>(
    budget: &MemoryBudget,
    node: &'a Node,
) -> Result<'a, (&'a [Node], Restrictions<'a>)> {
    let l = list(Some(node))?;
    let mut restrictions = BudgetedVec::new(budget, Category::Metadata)?;
    if l.first().ok_or_else(bad)?.text() == Some("0") {
        return Ok((l.get(1..).unwrap_or_default(), restrictions));
    }
    let n = num(l.get(1))?;
    let end = n
        .checked_mul(2)
        .and_then(|n| n.checked_add(2))
        .ok_or_else(bad)?;
    let pairs = l.get(2..end).ok_or_else(bad)?;
    let rest = l.get(end..).unwrap_or_default();
    if !rest.is_empty() {
        for entry in counted(rest, 1, rest.first())? {
            let entry = list(Some(entry))?;
            let key = txt(entry.first())?;
            let l = list(entry.get(1))?;
            let items = counted(l, 1, l.first())?;
            let mut output =
                BudgetedVec::try_with_capacity(budget, Category::Metadata, items.len().max(1))?;
            if items.is_empty() {
                output.try_push(RawRls {
                    condition: "",
                    fields: &[],
                })?;
            }
            for rls in items {
                let rls = list(Some(rls))?;
                let n = match rls.get(2) {
                    Some(n) => num(Some(n))?,
                    None => 0,
                };
                let condition = txt(rls.get(1))?;
                let fields = rls.get(3..).unwrap_or_default();
                output.try_push(RawRls {
                    condition,
                    fields: fields.get(..n.min(fields.len())).ok_or_else(bad)?,
                })?;
            }
            if let Some(slot) = restrictions.iter_mut().find(|(old, _)| *old == key) {
                *slot = (key, output);
            } else {
                restrictions.try_push((key, output))?;
            }
        }
    }
    Ok((pairs, restrictions))
}
fn field<'a>(index: &ManagedIndex, node: &'a Node, owner: &str) -> Result<'a, Charged<String>> {
    let r = match node.at(1) {
        Some(n) if n.is_list() => n,
        _ => node,
    };
    let first = txt(r.at(0))?;
    if first == "0" && r.at(1).is_some() {
        let name = r
            .at(1)
            .and_then(Node::text)
            .and_then(Guid::parse)
            .and_then(|id| index.names.get(&id))
            .ok_or(Error::Semantic("unresolved_field", None))?;
        return Ok(text(index.memory_budget(), &[name.as_str()])?.into_string()?);
    }
    let kind = owner.split('.').next().unwrap_or_default();
    let name = std_attr(kind, first).ok_or(Error::Semantic("unresolved_field", None))?;
    Ok(text(index.memory_budget(), &[owner, ".StandardAttribute.", name])?.into_string()?)
}
fn restriction<'a>(
    index: &ManagedIndex,
    owner: &str,
    raw: &RawRls<'a>,
) -> Result<'a, Charged<Restriction>> {
    let budget = index.memory_budget();
    let condition = description_text(budget, raw.condition)?.into_string()?;
    for f in raw.fields {
        txt(f.get(&[1, 0]))?;
    }
    let mut fields = vec(budget, raw.fields.len())?;
    for regular in [true, false] {
        for f in raw.fields {
            if (f.get(&[1, 0]).and_then(Node::text) == Some("0")) == regular {
                fields = append(fields, field(index, f, owner)?)?;
            }
        }
    }
    Ok(condition
        .try_zip(fields)?
        .map(|(condition, fields)| Restriction { condition, fields }))
}
fn object_rights<'a>(
    index: &ManagedIndex,
    res: &Resolved<'a>,
    default: bool,
) -> Result<'a, Charged<Vec<RightValue>>> {
    let budget = index.memory_budget();
    let (pairs, restrictions) = rightlist(budget, res.raw)?;
    for pair in pairs.as_chunks::<2>().0 {
        txt(pair.first())?;
    }
    let mut output = vec(budget, pairs.len() / 2)?;
    for pair in pairs.as_chunks::<2>().0 {
        let raw = txt(pair.first())?;
        let value = pair[1].text() == Some("1");
        let stored = restrictions
            .iter()
            .find(|(key, _)| *key == raw)
            .map(|(_, items)| items);
        let dropped = stored.is_none()
            && pairs
                .as_chunks::<2>()
                .0
                .iter()
                .any(|p| p[0].text() == Some(raw) && (p[1].text() == Some("1")) == default);
        let right = Guid::parse(raw).and_then(Right::from_static_guid);
        if dropped && right.is_none() {
            continue;
        }
        let right = right.ok_or(Error::Semantic("unknown_right:", Some(raw)))?;
        let mut actual = vec(
            budget,
            if dropped {
                0
            } else {
                stored.map_or(0, |items| items.len())
            },
        )?;
        if !dropped && let Some(items) = stored {
            for item in items.iter() {
                actual = append(actual, restriction(index, res.name.as_str(), item)?)?;
            }
        }
        output = append(
            output,
            actual.map(|restrictions| RightValue {
                right,
                value,
                restrictions,
            }),
        )?;
    }
    Ok(output)
}
pub(super) fn decode<'a>(
    index: &ManagedIndex,
    settings: &ConfigSettings,
    node: &'a Node,
    keep: bool,
) -> Result<'a, Charged<Rights>> {
    let budget = index.memory_budget();
    let set_for_new_objects = flag(node.at(3))?;
    let set_for_attributes_by_default = flag(node.at(4))?;
    let independent_rights_of_child_objects = flag(node.at(5))?;
    let list = list(node.at(1))?;
    let objects = counted(list, 1, list.first())?;
    let mut resolved = BudgetedVec::try_with_capacity(budget, Category::Metadata, objects.len())?;
    for (ordinal, object) in objects.iter().enumerate() {
        let object = self::list(Some(object))?;
        let raw = object.get(1);
        let mut res = resolve(
            index,
            settings,
            object.first(),
            raw.unwrap_or(node),
            ordinal,
        )?;
        res.raw = raw.ok_or_else(bad)?;
        resolved.try_push(res)?;
    }
    resolved.sort_unstable_by_key(|r| (key(&r.target, index.cfg_id), r.ordinal));
    let mut decoded = BudgetedVec::try_with_capacity(budget, Category::Metadata, resolved.len())?;
    resolved.reverse();
    while let Some(res) = resolved.pop() {
        let default = if res.target.is_field() {
            set_for_attributes_by_default
        } else {
            set_for_new_objects
        };
        let values = object_rights(index, &res, default)?;
        let ordinal = res.ordinal;
        let target = res.target;
        let actual = res
            .name
            .into_string()?
            .try_zip(values)?
            .map(|(full_name, rights)| ObjectRights {
                target,
                full_name,
                rights,
            });
        decoded.try_push((ordinal, actual))?;
    }
    decoded.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    decoded.reverse();
    let mut objects = vec(budget, decoded.len())?;
    while let Some((_, object)) = decoded.pop() {
        objects = append(objects, object)?;
    }
    let tl = self::list(node.at(2))?;
    let tl = counted(tl, 1, tl.first())?;
    let mut templates = vec(budget, tl.len())?;
    for t in tl {
        let t = self::list(Some(t))?;
        let name = description_text(budget, txt(t.first())?)?.into_string()?;
        let body = description_text(budget, txt(t.get(1))?)?.into_string()?;
        templates = append(templates, name.try_zip(body)?)?;
    }
    let source = if keep {
        copy_node(budget, node)?
    } else {
        empty(budget, Node::empty())?
    };
    Ok(objects
        .try_zip(templates)?
        .try_zip(source)?
        .map(|((objects, templates), source)| Rights {
            set_for_new_objects,
            set_for_attributes_by_default,
            independent_rights_of_child_objects,
            objects,
            templates,
            source,
        }))
}
