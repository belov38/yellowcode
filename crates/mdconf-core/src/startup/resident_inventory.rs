use super::{
    GuidMap, ManagedConfigurationError, ManagedDiagnostics, ManagedIndex, ManagedViewerContext,
    ObjectFacts, objects::name_of,
};
use crate::{ManagedEntrySource, MdKind};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};

#[derive(Debug)]
pub struct ResidentObject {
    pub parent: Option<usize>,
    pub object: Charged<ObjectFacts>,
}
#[derive(Debug)]
pub struct ManagedViewerFacts {
    pub(super) objects: BudgetedVec<ResidentObject>,
    pub(super) diagnostics: ManagedDiagnostics,
}
impl ManagedViewerFacts {
    pub fn build<S: ManagedEntrySource>(
        context: &ManagedViewerContext<S>,
    ) -> Result<Self, ManagedConfigurationError<S::Error>> {
        let index = context.index();
        let budget = index.memory_budget();
        let locations = locations(index)?;
        let mut rows = BudgetedVec::try_with_capacity(budget, Category::Metadata, locations.len())?;
        for _ in locations.iter() {
            rows.try_push(None)?;
        }
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        for sets_first in [true, false] {
            for (ordinal, (id, parent)) in locations.iter().enumerate() {
                let is_set = parent.is_none()
                    && index
                        .objs
                        .get(id)
                        .is_some_and(|(kind, _)| *kind == MdKind::DefinedType);
                if is_set != sets_first {
                    continue;
                }
                let selected = context
                    .read_object(*id)?
                    .ok_or(BudgetError::Overflow)?
                    .into_facts()?;
                diagnostics.merge(selected.diagnostics)?;
                *rows.get_mut(ordinal).ok_or(BudgetError::Overflow)? = Some(selected.object);
            }
        }
        let mut objects =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, locations.len())?;
        for (ordinal, (_, parent)) in locations.iter().enumerate() {
            let object = rows
                .get_mut(ordinal)
                .and_then(Option::take)
                .ok_or(BudgetError::Overflow)?;
            objects.try_push(ResidentObject {
                parent: *parent,
                object,
            })?;
        }
        Ok(Self {
            objects,
            diagnostics,
        })
    }
    pub fn objects(&self) -> &[ResidentObject] {
        &self.objects
    }
    pub fn diagnostics(&self) -> &ManagedDiagnostics {
        &self.diagnostics
    }
}
struct Node {
    id: Guid,
    children: BudgetedVec<usize>,
}
fn node(index: &ManagedIndex, id: Guid) -> Result<Node, BudgetError> {
    let children = BudgetedVec::try_with_capacity(
        index.memory_budget(),
        Category::Metadata,
        index
            .nested
            .iter()
            .filter(|(_, parent)| *parent == id)
            .count(),
    )?;
    Ok(Node { id, children })
}
fn locations(index: &ManagedIndex) -> Result<BudgetedVec<(Guid, Option<usize>)>, BudgetError> {
    let budget = index.memory_budget();
    let capacity = index
        .order
        .len()
        .checked_add(index.nested.len())
        .ok_or(BudgetError::Overflow)?;
    let mut nodes = BudgetedVec::try_with_capacity(budget, Category::Metadata, capacity)?;
    let mut roots = BudgetedVec::try_with_capacity(budget, Category::Metadata, index.order.len())?;
    let mut top = GuidMap::new(budget)?;
    for id in index.order.iter() {
        if name_of(index, id).is_none() {
            continue;
        }
        let at = nodes.len();
        if top.get(id).is_none() {
            top.insert(*id, at)?;
        }
        roots.try_push(at)?;
        nodes.try_push(node(index, *id)?)?;
    }
    let mut pool = GuidMap::new(budget)?;
    for (child, _) in index.nested.iter() {
        if name_of(index, child).is_none() {
            continue;
        }
        let at = nodes.len();
        nodes.try_push(node(index, *child)?)?;
        pool.insert(*child, Some(at))?;
    }
    for (child, parent) in index.nested.iter().rev() {
        let Some(child) = pool.get_mut(child).and_then(Option::take) else {
            continue;
        };
        nodes
            .get_mut(child)
            .ok_or(BudgetError::Overflow)?
            .children
            .reverse();
        let owner = pool.get_mut(parent).and_then(Option::take);
        if let Some(parent) = owner.or_else(|| top.get(parent).copied()) {
            let children = &mut nodes.get_mut(parent).ok_or(BudgetError::Overflow)?.children;
            if children.len() >= children.capacity() {
                return Err(BudgetError::Overflow);
            }
            children.try_push(child)?;
        }
        if let Some(owner) = owner {
            *pool.get_mut(parent).ok_or(BudgetError::Overflow)? = Some(owner);
        }
    }
    for root in roots.iter() {
        nodes
            .get_mut(*root)
            .ok_or(BudgetError::Overflow)?
            .children
            .reverse();
    }
    let mut pending = BudgetedVec::try_with_capacity(budget, Category::Metadata, nodes.len())?;
    for root in roots.iter().rev() {
        pending.try_push((*root, None))?;
    }
    let mut out = BudgetedVec::try_with_capacity(budget, Category::Metadata, nodes.len())?;
    while let Some((node, parent)) = pending.pop() {
        if out.len() >= nodes.len() {
            return Err(BudgetError::Overflow);
        }
        let node = nodes.get(node).ok_or(BudgetError::Overflow)?;
        let at = out.len();
        out.try_push((node.id, parent))?;
        for child in node.children.iter().rev() {
            pending.try_push((*child, Some(at)))?;
        }
    }
    Ok(out)
}
