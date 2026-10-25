use super::{ManagedIndex, ManagedObjectsError, Tree, index::GuidText, kind_spec};
use crate::graph::{
    read::Read,
    route::{RouteFacts, TaskAttribute, TaskFacts},
};
use crate::templates::allocation::{Allocation, Vector};
use crate::{EntryData, ManagedEntries, ManagedEntrySource};
use brace::{Guid, Node};
use mdschema::StaticValue;
use memory_budget::BudgetError;

fn pairs(value: Option<&'static StaticValue>) -> &'static [(&'static str, StaticValue)] {
    value.and_then(StaticValue::as_object).unwrap_or_default()
}
fn walk(
    node: Tree<'_>,
    children: &'static [(&'static str, StaticValue)],
    out: &mut Vector<Guid>,
) -> Result<(), BudgetError> {
    for (class, child) in children {
        if child
            .get("header")
            .and_then(StaticValue::as_array)
            .is_none()
        {
            continue;
        }
        for item in node
            .find_collection(class)
            .into_iter()
            .flat_map(|items| items.iter())
        {
            let id = child
                .get("uuid")
                .and_then(StaticValue::as_array)
                .and_then(|path| {
                    let mut tree = item;
                    for index in path {
                        tree = tree.get(&[usize::try_from(index.as_u64()?).ok()?])?;
                    }
                    tree.as_guid()
                });
            if let Some(id) = id {
                out.push(id)?;
            }
            walk(item, pairs(child.get("children")), out)?;
        }
    }
    Ok(())
}
fn child_order<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
    id: Guid,
    keep: bool,
) -> Result<Vector<Guid>, ManagedObjectsError<S::Error>> {
    let mut output = Allocation::Managed(entries.memory_budget()).vector(0)?;
    let Some((kind, Some(_))) = index.objs.get(&id) else {
        return Ok(output);
    };
    let Some(spec) = kind_spec(kind.as_str()) else {
        return Ok(output);
    };
    let name = GuidText::new(id);
    let name = name.as_str()?;
    let fetched = entries.doc(name);
    if !keep {
        entries.forget(name);
    }
    match fetched {
        Ok(Some(data)) => {
            if let EntryData::Brace(doc) = &*data {
                walk(
                    Tree::new("", &doc.root),
                    pairs(spec.field("children")),
                    &mut output,
                )?;
            }
        }
        Ok(None) => {}
        Err(error) if entries.recoverable_content_error(&error) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(output)
}
pub(super) fn task_of<'a, S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &'a ManagedIndex,
    bp: Guid,
    keep: bool,
) -> Result<RouteFacts<'a>, ManagedObjectsError<S::Error>> {
    let allocation = Allocation::Managed(entries.memory_budget());
    let read = Read::new(allocation);
    let name = GuidText::new(bp);
    let name = name.as_str()?;
    let data = match entries.doc(name) {
        Ok(Some(data)) => data,
        Ok(None) => return Ok(RouteFacts::Deferred("business process: no entry")),
        Err(error) if entries.recoverable_content_error(&error) => {
            return Ok(RouteFacts::Deferred("business process: source"));
        }
        Err(error) => return Err(error.into()),
    };
    if !keep {
        entries.forget(name);
    }
    let EntryData::Brace(doc) = &*data else {
        return Ok(RouteFacts::Deferred("business process: not brace"));
    };
    let Some(id) = doc
        .root
        .get(&[1, 25])
        .and_then(Node::text)
        .and_then(Guid::parse)
    else {
        return Ok(RouteFacts::Ready(None));
    };
    let Some(task) = index.names.get(&id) else {
        return Ok(RouteFacts::Ready(None));
    };
    let task = task.as_str();
    let mut rest = allocation.vector(0)?;
    for (id, name) in index.names.iter() {
        if name
            .as_str()
            .strip_prefix(task)
            .is_some_and(|suffix| suffix.starts_with(".AddressingAttribute."))
        {
            rest.push((*id, name.as_str()))?;
        }
    }
    let mut attrs = allocation.vector(rest.len())?;
    if !rest.is_empty() {
        for id in child_order(entries, index, id, keep)?.iter() {
            if let Some(at) = rest.iter().position(|(key, _)| key == id) {
                let last = rest.len() - 1;
                rest.swap(at, last);
                let (id, name) = rest.pop().ok_or(BudgetError::Overflow)?;
                attrs.push(
                    TaskAttribute::new(id, name, read).map_err(|error| match error {
                        crate::graph::read::ReadError::Memory(error) => error,
                        _ => BudgetError::Overflow,
                    })?,
                )?;
            }
        }
        rest.sort_unstable_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(&b.0)));
        for (id, name) in rest.iter() {
            attrs.push(
                TaskAttribute::new(*id, name, read).map_err(|error| match error {
                    crate::graph::read::ReadError::Memory(error) => error,
                    _ => BudgetError::Overflow,
                })?,
            )?;
        }
    }
    Ok(RouteFacts::Ready(Some(TaskFacts { name: task, attrs })))
}
