use super::index::{GuidText, text};
use super::values::{append, empty};
use super::{GuidMap, ManagedIndex, Tree, kind_spec};
use crate::forms::shared::{Shared as FormState, std_with_ru};
use crate::forms::tables::{PropRef, TableView};
use crate::{EntryData, ManagedEntries, ManagedEntriesError, ManagedEntrySource};
use brace::{Guid, Node};
use mdschema::{StaticValue, static_schema};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Shared,
};
use std::collections::BTreeSet;
const CATEGORY: Category = Category::Metadata;
const BY_NAME_TAGS: [&str; 7] = [
    "Attribute",
    "TabularSection",
    "Dimension",
    "Resource",
    "AccountingFlag",
    "ExtDimensionAccountingFlag",
    "AddressingAttribute",
];
#[derive(Debug)]
struct NamedGuids {
    name: BudgetedText,
    values: BudgetedVec<Guid>,
}
#[derive(Debug)]
struct NamedNodes {
    name: BudgetedText,
    values: Charged<Vec<Node>>,
}
#[derive(Debug)]
#[doc(hidden)]
pub struct ManagedShared {
    v85: bool,
    names_order: BudgetedVec<Guid>,
    names: BudgetedVec<(Guid, BudgetedText)>,
    patterns: GuidMap<Option<Charged<Node>>>,
    children: BudgetedVec<NamedGuids>,
    common: BudgetedVec<Guid>,
    constants: BudgetedVec<Guid>,
    by_name: BudgetedVec<NamedNodes>,
}
impl ManagedShared {
    pub(crate) fn memory_budget(&self) -> &MemoryBudget {
        self.names_order.reservation().budget()
    }
    pub(crate) fn v85(&self) -> bool {
        self.v85
    }
    pub(crate) fn child_patterns(&self) -> impl Iterator<Item = (&Guid, &Node)> {
        self.patterns
            .iter()
            .filter_map(|(id, value)| value.as_deref().map(|node| (id, node)))
    }
    pub(crate) fn names_order(&self) -> &[Guid] {
        &self.names_order
    }
    pub(crate) fn child_pattern(&self, id: &Guid) -> Option<&Node> {
        self.patterns.get(id)?.as_deref()
    }
    pub(crate) fn tables(&self) -> TableView<'_> {
        TableView::embedded(self.v85)
    }
    pub(crate) fn prop(&self, context: &str, property: &str) -> Option<PropRef<'_>> {
        self.tables()
            .learned
            .get(context)?
            .props
            .iter()
            .filter(|(key, _)| *key == property)
            .map(|(_, value)| value)
            .last()
    }
    pub(crate) fn std_values(&self, kind: &str) -> BTreeSet<String> {
        std_kind(kind)
            .iter()
            .filter_map(|(_, value)| value.as_str().map(String::from))
            .collect()
    }
    pub(crate) fn std_ids(&self, kind: &str) -> BTreeSet<String> {
        std_kind(kind)
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect()
    }
    pub(crate) fn std_fields(&self, full: &str) -> BTreeSet<String> {
        std_with_ru(&self.std_values(full.split('.').next().unwrap_or_default()))
    }
    pub(crate) fn main_table_fields(&self, full: &str) -> BTreeSet<String> {
        let mut output = self.std_fields(full);
        for (_, name) in self.names.iter() {
            let mut parts = name.as_str().split('.');
            match (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) {
                (Some(kind), Some(object), Some(_), Some(child), None)
                    if full.split_once('.') == Some((kind, object)) =>
                {
                    output.insert(child.to_string());
                }
                (Some("CommonAttribute"), Some(name), None, None, None) => {
                    output.insert(name.to_string());
                }
                _ => {}
            }
        }
        output
    }
    pub(crate) fn md_children(&self, full: &str) -> &[Guid] {
        self.children
            .iter()
            .find(|entry| entry.name.as_str() == full)
            .map_or(&[], |entry| &entry.values)
    }
    pub(crate) fn common_attributes(&self) -> &[Guid] {
        &self.common
    }
    pub(crate) fn constants(&self) -> &[Guid] {
        &self.constants
    }
    pub(crate) fn by_name(&self, name: &str) -> &[Node] {
        self.by_name
            .iter()
            .find(|entry| entry.name.as_str() == name)
            .map_or(&[], |entry| &entry.values)
    }
}
fn pairs(value: Option<&'static StaticValue>) -> &'static [(&'static str, StaticValue)] {
    value.and_then(StaticValue::as_object).unwrap_or(&[])
}
fn std_kind(kind: &str) -> &'static [(&'static str, StaticValue)] {
    pairs(
        static_schema()
            .table("stdattrs.json")
            .and_then(|table| table.get(kind)),
    )
}
fn std_kinds() -> &'static [(&'static str, StaticValue)] {
    pairs(static_schema().table("stdattrs.json"))
}
fn parent(name: &str) -> Option<&str> {
    (name.split('.').count() >= 4)
        .then(|| name.rsplitn(3, '.').nth(2))
        .flatten()
}
fn metadata_key(name: &str) -> Option<&str> {
    parent(name)?;
    let mut tail = name.rsplit('.');
    let last = tail.next()?;
    BY_NAME_TAGS.contains(&tail.next()?).then_some(last)
}
#[derive(Debug, thiserror::Error)]
#[doc(hidden)]
pub enum ManagedFormSharedError<E> {
    #[error("{0}")]
    Entries(#[from] ManagedEntriesError<E>),
    #[error("{0}")]
    Memory(#[from] BudgetError),
}
type Result<T, E> = std::result::Result<T, ManagedFormSharedError<E>>;
struct Walk {
    order: BudgetedVec<Guid>,
    seen: GuidMap<()>,
    patterns: GuidMap<Option<Charged<Node>>>,
}
impl Walk {
    fn new(budget: &MemoryBudget) -> std::result::Result<Self, BudgetError> {
        Ok(Self {
            order: BudgetedVec::new(budget, CATEGORY)?,
            seen: GuidMap::new(budget)?,
            patterns: GuidMap::new(budget)?,
        })
    }
    fn name(&mut self, id: Guid) -> std::result::Result<(), BudgetError> {
        if self.seen.get(&id).is_none() {
            self.seen.insert(id, ())?;
            self.order.try_push(id)?;
        }
        Ok(())
    }
    fn children<'a>(
        &mut self,
        tree: Tree<'a>,
        specs: &'static [(&'static str, StaticValue)],
        budget: &MemoryBudget,
    ) -> std::result::Result<(), BudgetError> {
        enum Task<'a> {
            Spec(Tree<'a>, &'static str, &'static StaticValue),
            Item(Tree<'a>, &'static StaticValue),
        }
        let mut tasks = BudgetedVec::new(budget, CATEGORY)?;
        for (class, spec) in specs.iter().rev() {
            tasks.try_push(Task::Spec(tree, class, spec))?;
        }
        while let Some(task) = tasks.pop() {
            match task {
                Task::Spec(tree, class, spec) => {
                    if spec.get("header").and_then(StaticValue::as_array).is_none() {
                        continue;
                    }
                    if let Some(items) = tree.find_collection(class) {
                        for item in items.iter().rev() {
                            tasks.try_push(Task::Item(item, spec))?;
                        }
                    }
                }
                Task::Item(tree, spec) => {
                    if let Some(id) = at(tree, spec.get("uuid")).and_then(Tree::as_guid) {
                        self.name(id)?;
                        let value = first_pattern(tree, 0)
                            .map(|tree| tree.to_node(budget))
                            .transpose()?;
                        self.patterns.insert(id, value)?;
                    }
                    for (class, spec) in pairs(spec.get("children")).iter().rev() {
                        tasks.try_push(Task::Spec(tree, class, spec))?;
                    }
                }
            }
        }
        Ok(())
    }
    fn subobjects<S: ManagedEntrySource + ?Sized>(
        &mut self,
        entries: &ManagedEntries<'_, S>,
        spec: &'static StaticValue,
        kind: &str,
        root: Guid,
        tree: Tree<'_>,
        keep: bool,
    ) -> Result<(), S::Error> {
        struct Frame {
            depth: usize,
            todo: BudgetedVec<(bool, Guid)>,
        }
        let budget = entries.memory_budget();
        let mut seen = GuidMap::new(budget)?;
        seen.insert(root, ())?;
        let mut frames = BudgetedVec::new(budget, CATEGORY)?;
        frames.try_push(Frame {
            depth: 0,
            todo: pending(tree, spec, kind, budget)?,
        })?;
        while let Some(frame) = frames.last_mut() {
            let Some((same, id)) = frame.todo.pop() else {
                frames.pop();
                continue;
            };
            let entry = GuidText::new(id);
            let Some(data) = recoverable_doc(entries, entry.as_str()?, keep)? else {
                continue;
            };
            let EntryData::Brace(doc) = &*data else {
                continue;
            };
            let tree = Tree::new("", &doc.root);
            if same {
                if seen.get(&id).is_some() {
                    continue;
                }
                seen.insert(id, ())?;
                let depth = frame.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
                if depth > 64 {
                    continue;
                }
                self.name(id)?;
                frames.try_push(Frame {
                    depth,
                    todo: pending(tree, spec, kind, budget)?,
                })?;
            } else if tree
                .header_path(entry.as_str()?, budget, CATEGORY)?
                .is_some()
            {
                self.name(id)?;
            }
        }
        Ok(())
    }
}
fn at<'a>(tree: Tree<'a>, path: Option<&StaticValue>) -> Option<Tree<'a>> {
    let path = path?.as_array()?;
    let mut tree = tree;
    for index in path {
        tree = tree.get(&[usize::try_from(index.as_i64()?).ok()?])?;
    }
    Some(tree)
}
fn first_pattern(tree: Tree<'_>, depth: usize) -> Option<Tree<'_>> {
    let items = tree.as_list()?;
    if items.get(0).and_then(Tree::text) == Some("Pattern") {
        return Some(tree);
    }
    (depth < 6)
        .then(|| {
            items
                .iter()
                .find_map(|child| first_pattern(child, depth + 1))
        })
        .flatten()
}
fn pending(
    tree: Tree<'_>,
    spec: &'static StaticValue,
    kind: &str,
    budget: &MemoryBudget,
) -> std::result::Result<BudgetedVec<(bool, Guid)>, BudgetError> {
    let mut output = BudgetedVec::new(budget, CATEGORY)?;
    for (tag, class) in pairs(spec.get("subobjects")) {
        let same = *tag == kind;
        if !same && *tag != "Form" && *tag != "Template" {
            continue;
        }
        if let Some(items) = class.as_str().and_then(|class| tree.find_collection(class)) {
            for id in items.iter().filter_map(Tree::as_guid) {
                output.try_push((same, id))?;
            }
        }
    }
    output.reverse();
    Ok(output)
}
fn metadata_node(
    budget: &MemoryBudget,
    id: Guid,
) -> std::result::Result<Charged<Node>, BudgetError> {
    let value = GuidText::new(id);
    let nodes = BudgetedVec::try_with_capacity(budget, CATEGORY, 2)?.into_charged_vec();
    let nodes = append(nodes, text(budget, &["0"])?.into_string()?.map(Node::Atom))?;
    Ok(append(
        nodes,
        text(budget, &[value.as_str()?])?
            .into_string()?
            .map(Node::Atom),
    )?
    .map(Node::List))
}
fn standard_node(
    budget: &MemoryBudget,
    id: &str,
) -> std::result::Result<Charged<Node>, BudgetError> {
    let nodes = BudgetedVec::try_with_capacity(budget, CATEGORY, 1)?.into_charged_vec();
    Ok(append(nodes, text(budget, &[id])?.into_string()?.map(Node::Atom))?.map(Node::List))
}
fn named_nodes(
    budget: &MemoryBudget,
    names: &[(Guid, BudgetedText)],
    key: &str,
) -> std::result::Result<NamedNodes, BudgetError> {
    let metadata_count = names
        .iter()
        .filter(|(_, name)| metadata_key(name.as_str()) == Some(key))
        .count();
    let standard_count = std_kinds()
        .iter()
        .flat_map(|(_, value)| pairs(Some(value)))
        .filter(|(_, name)| name.as_str() == Some(key))
        .count();
    let capacity = metadata_count
        .checked_add(standard_count)
        .ok_or(BudgetError::Overflow)?;
    let mut values = BudgetedVec::try_with_capacity(budget, CATEGORY, capacity)?.into_charged_vec();
    for (id, name) in names {
        if metadata_key(name.as_str()) == Some(key) {
            values = append(values, metadata_node(budget, *id)?)?;
        }
    }
    for (_, kind) in std_kinds() {
        for (id, name) in pairs(Some(kind)) {
            if name.as_str() == Some(key)
                && !values.iter().any(|node| {
                    node.as_list().is_some_and(|list| {
                        list.len() == 1 && list.first().and_then(Node::text) == Some(*id)
                    })
                })
            {
                values = append(values, standard_node(budget, id)?)?;
            }
        }
    }
    Ok(NamedNodes {
        name: text(budget, &[key])?,
        values,
    })
}
#[doc(hidden)]
pub fn build<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
) -> Result<Shared<FormState>, S::Error> {
    build_with_retention(entries, index, true)
}

#[doc(hidden)]
pub fn build_with_retention<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
    keep: bool,
) -> Result<Shared<FormState>, S::Error> {
    let budget = entries.memory_budget();
    if !budget.same_scope(index.memory_budget()) {
        return Err(BudgetError::Overflow.into());
    }
    let entry = GuidText::new(index.cfg_id);
    let v85 = if let Some(data) = recoverable_doc(entries, entry.as_str()?, keep)? {
        match &*data {
            EntryData::Brace(doc) => doc.root.get(&[3, 1, 1]).is_some_and(|body| match body {
                Node::List(items) => items
                    .get(62)
                    .and_then(Node::text)
                    .is_some_and(|text| matches!(text, "5" | "6")),
                Node::Str(text) | Node::Atom(text) => {
                    text.chars().nth(62).is_some_and(|c| matches!(c, '5' | '6'))
                }
            }),
            _ => false,
        }
    } else {
        false
    };
    let mut walk = Walk::new(budget)?;
    for id in index.order.iter() {
        let Some((kind, _)) = index.objs.get(id) else {
            continue;
        };
        walk.name(*id)?;
        let Some(spec) = kind_spec(kind.as_str()) else {
            continue;
        };
        let entry = GuidText::new(*id);
        let Some(data) = recoverable_doc(entries, entry.as_str()?, keep)? else {
            continue;
        };
        let EntryData::Brace(doc) = &*data else {
            continue;
        };
        let tree = Tree::new("", &doc.root);
        walk.children(tree, pairs(spec.field("children")), budget)?;
        walk.subobjects(entries, spec.raw(), kind.as_str(), *id, tree, keep)?;
    }
    let mut names_order = BudgetedVec::new(budget, CATEGORY)?;
    let mut names = BudgetedVec::new(budget, CATEGORY)?;
    for id in walk.order.iter() {
        if let Some(name) = index.names.get(id) {
            names_order.try_push(*id)?;
            names.try_push((*id, text(budget, &[name.as_str()])?))?;
        }
    }
    let mut children: BudgetedVec<NamedGuids> = BudgetedVec::new(budget, CATEGORY)?;
    let mut common = BudgetedVec::new(budget, CATEGORY)?;
    let mut constants = BudgetedVec::new(budget, CATEGORY)?;
    let mut by_name: BudgetedVec<NamedNodes> = BudgetedVec::new(budget, CATEGORY)?;
    for (id, name) in names.iter() {
        let full = name.as_str();
        if let Some(parent) = parent(full)
            && !children.iter().any(|entry| entry.name.as_str() == parent)
        {
            let capacity = names
                .iter()
                .filter(|(_, name)| self::parent(name.as_str()) == Some(parent))
                .count();
            let mut values = BudgetedVec::try_with_capacity(budget, CATEGORY, capacity)?;
            for (id, name) in names.iter() {
                if self::parent(name.as_str()) == Some(parent) {
                    values.try_push(*id)?;
                }
            }
            children.try_push(NamedGuids {
                name: text(budget, &[parent])?,
                values,
            })?;
        }
        if full.starts_with("CommonAttribute.") && full.split('.').count() == 2 {
            common.try_push(*id)?;
        }
        if full.starts_with("Constant.") && full.split('.').count() == 2 {
            constants.try_push(*id)?;
        }
        if let Some(key) = metadata_key(full)
            && !by_name.iter().any(|entry| entry.name.as_str() == key)
        {
            by_name.try_push(named_nodes(budget, &names, key)?)?;
        }
    }
    for (_, kind) in std_kinds() {
        for (_, name) in pairs(Some(kind)) {
            if let Some(key) = name.as_str()
                && !by_name.iter().any(|entry| entry.name.as_str() == key)
            {
                by_name.try_push(named_nodes(budget, &names, key)?)?;
            }
        }
    }
    let value = ManagedShared {
        v85,
        names_order,
        names,
        patterns: walk.patterns,
        children,
        common,
        constants,
        by_name,
    };
    Ok(empty(budget, FormState::Managed(value))?.try_into_shared()?)
}

fn recoverable_doc<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    name: &str,
    keep: bool,
) -> Result<Option<Shared<EntryData>>, S::Error> {
    let result = entries.doc(name);
    if !keep {
        entries.forget(name);
    }
    match result {
        Ok(data) => Ok(data),
        Err(error) if entries.recoverable_content_error(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
