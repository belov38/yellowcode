use super::{Tree, kind_spec};
use crate::objects::recalc::{
    DIM_HEADER, DIM_UUID, GENERATED, RECALC_CLASS, RECALC_DIM_CLASS, RECALC_HEADER,
};
use crate::{EntryData, ManagedEntries, ManagedEntriesError, ManagedEntrySource, SubKind};
use brace::{Guid, Node};
use mdschema::{MdKind, StaticValue, static_schema};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};

const CATEGORY: Category = Category::Metadata;
const MAX_NESTING: usize = 64;

#[derive(Debug)]
pub struct GuidMap<T>(BudgetedVec<(Guid, T)>);
impl<T> GuidMap<T> {
    pub fn new(budget: &MemoryBudget) -> std::result::Result<Self, BudgetError> {
        Ok(Self(BudgetedVec::new(budget, CATEGORY)?))
    }
    fn owns_scope(&self, budget: &MemoryBudget) -> bool {
        self.0.reservation().budget().same_scope(budget)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, key: &Guid) -> Option<&T> {
        let at = self.0.binary_search_by_key(key, |(key, _)| *key).ok()?;
        self.0.get(at).map(|(_, value)| value)
    }
    pub fn get_mut(&mut self, key: &Guid) -> Option<&mut T> {
        let at = self.0.binary_search_by_key(key, |(key, _)| *key).ok()?;
        self.0.get_mut(at).map(|(_, value)| value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Guid, &T)> {
        self.0.iter().map(|(key, value)| (key, value))
    }
    pub fn insert(&mut self, key: Guid, value: T) -> std::result::Result<(), BudgetError> {
        match self.0.binary_search_by_key(&key, |(key, _)| *key) {
            Ok(at) => {
                let slot = self.0.get_mut(at).ok_or(BudgetError::Overflow)?;
                slot.1 = value;
            }
            Err(at) => {
                self.0.try_push((key, value))?;
                self.0
                    .get_mut(at..)
                    .ok_or(BudgetError::Overflow)?
                    .rotate_right(1);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedTypeOwner {
    pub object: Guid,
    pub child: Option<Guid>,
    pub category: &'static str,
    pub recalc: Option<Guid>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedIndexError<E> {
    #[error("{0}")]
    Entries(#[from] ManagedEntriesError<E>),
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("entry {entry}: {what}")]
    Format { entry: Guid, what: &'static str },
}
type Result<T, E> = std::result::Result<T, ManagedIndexError<E>>;

#[derive(Debug)]
pub struct ManagedIndex {
    budget: MemoryBudget,
    pub cfg_id: Guid,
    pub header_id: Guid,
    pub order: BudgetedVec<Guid>,
    pub objs: GuidMap<(MdKind, Option<BudgetedText>)>,
    pub names: GuidMap<BudgetedText>,
    pub types: GuidMap<BudgetedText>,
    pub type_owners: GuidMap<ManagedTypeOwner>,
    pub sub_hdr: GuidMap<Charged<Vec<usize>>>,
    pub subs: GuidMap<BudgetedVec<(SubKind, Guid)>>,
    pub recalcs: GuidMap<BudgetedVec<Guid>>,
    pub nested: BudgetedVec<(Guid, Guid)>,
    pub missing: BudgetedVec<Guid>,
    pub too_deep: BudgetedVec<Guid>,
    pub skipped: BudgetedVec<BudgetedText>,
}
impl ManagedIndex {
    pub fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub(super) fn owns_scope(&self, budget: &MemoryBudget) -> bool {
        self.budget.same_scope(budget)
            && self.order.reservation().budget().same_scope(budget)
            && self.objs.owns_scope(budget)
            && self.names.owns_scope(budget)
            && self.types.owns_scope(budget)
            && self.type_owners.owns_scope(budget)
            && self.sub_hdr.owns_scope(budget)
            && self.subs.owns_scope(budget)
            && self.recalcs.owns_scope(budget)
            && self.nested.reservation().budget().same_scope(budget)
            && self.missing.reservation().budget().same_scope(budget)
            && self.too_deep.reservation().budget().same_scope(budget)
            && self.skipped.reservation().budget().same_scope(budget)
            && self.objs.iter().all(|(_, (_, name))| {
                name.as_ref()
                    .is_none_or(|name| name.memory_budget().same_scope(budget))
            })
            && self
                .names
                .iter()
                .all(|(_, name)| name.memory_budget().same_scope(budget))
            && self
                .types
                .iter()
                .all(|(_, name)| name.memory_budget().same_scope(budget))
            && self
                .sub_hdr
                .iter()
                .all(|(_, path)| path.reservation().budget().same_scope(budget))
            && self
                .subs
                .iter()
                .all(|(_, items)| items.reservation().budget().same_scope(budget))
            && self
                .recalcs
                .iter()
                .all(|(_, items)| items.reservation().budget().same_scope(budget))
            && self
                .skipped
                .iter()
                .all(|name| name.memory_budget().same_scope(budget))
    }
    fn new(
        cfg_id: Guid,
        header_id: Guid,
        budget: &MemoryBudget,
    ) -> std::result::Result<Self, BudgetError> {
        Ok(Self {
            budget: budget.clone(),
            cfg_id,
            header_id,
            order: BudgetedVec::new(budget, CATEGORY)?,
            objs: GuidMap::new(budget)?,
            names: GuidMap::new(budget)?,
            types: GuidMap::new(budget)?,
            type_owners: GuidMap::new(budget)?,
            sub_hdr: GuidMap::new(budget)?,
            subs: GuidMap::new(budget)?,
            recalcs: GuidMap::new(budget)?,
            nested: BudgetedVec::new(budget, CATEGORY)?,
            missing: BudgetedVec::new(budget, CATEGORY)?,
            too_deep: BudgetedVec::new(budget, CATEGORY)?,
            skipped: BudgetedVec::new(budget, CATEGORY)?,
        })
    }
    pub fn build<S: ManagedEntrySource + ?Sized>(
        entries: &ManagedEntries<'_, S>,
    ) -> Result<Self, S::Error> {
        let budget = entries.memory_budget();
        let cfg_id = entries.root_info()?.id();
        let cfg_name = GuidText::new(cfg_id);
        let cfg = entries
            .doc(cfg_name.as_str()?)?
            .ok_or(ManagedIndexError::Format {
                entry: cfg_id,
                what: "missing configuration brace document",
            })?;
        let raw = doc_tree(&cfg).ok_or(ManagedIndexError::Format {
            entry: cfg_id,
            what: "configuration is not a brace document",
        })?;
        let tree = Tree::new("Configuration", raw);
        let header_id = tree
            .get(&[3, 1, 1, 1, 1, 1, 2])
            .and_then(Tree::as_guid)
            .ok_or(ManagedIndexError::Format {
                entry: cfg_id,
                what: "no configuration header uuid",
            })?;
        let mut index = Self::new(cfg_id, header_id, budget)?;
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        stack.try_push(tree)?;
        while let Some(node) = stack.pop() {
            let Some(list) = node.as_list() else { continue };
            let kind = list
                .get(0)
                .and_then(Tree::text)
                .and_then(|class| static_schema().class_kind(class))
                .and_then(MdKind::from_name);
            let count = list
                .get(1)
                .filter(|node| !node.is_list())
                .and_then(Tree::text)
                .and_then(|text| text.trim().parse::<usize>().ok());
            if let (Some(kind), Some(count)) = (kind, count) {
                for item in list.iter().skip(2).take(count) {
                    if let Some(uuid) = item.as_guid() {
                        index.index_object(entries, kind, uuid)?;
                    }
                }
                continue;
            }
            for child in list.iter().rev() {
                stack.try_push(child)?;
            }
        }
        Ok(index)
    }
    fn index_object<S: ManagedEntrySource + ?Sized>(
        &mut self,
        entries: &ManagedEntries<'_, S>,
        kind: MdKind,
        uuid: Guid,
    ) -> Result<(), S::Error> {
        let budget = entries.memory_budget();
        let entry = GuidText::new(uuid);
        let entry = entry.as_str()?;
        if !entries.contains(entry) {
            self.missing.try_push(uuid)?;
            return Ok(());
        }
        let Some(spec) = kind_spec(kind.as_str()).filter(|spec| spec.header().is_some()) else {
            self.objs.insert(uuid, (kind, None))?;
            return Ok(());
        };
        let Some(data) = recoverable_doc(entries, entry)? else {
            self.missing.try_push(uuid)?;
            return Ok(());
        };
        let Some(raw) = doc_tree(&data) else {
            self.missing.try_push(uuid)?;
            return Ok(());
        };
        let tree = Tree::new(kind.as_str(), raw);
        let name = name_at(tree, spec.field("header"));
        let full = text(budget, &[kind.as_str(), ".", name])?;
        self.objs
            .insert(uuid, (kind, Some(text(budget, &[name])?)))?;
        self.names.insert(uuid, text(budget, &[full.as_str()])?)?;
        self.order.try_push(uuid)?;
        for info in array(spec.field("internal_info")) {
            let Some(tid) = at(tree, info.get("type_id")).and_then(Tree::as_guid) else {
                continue;
            };
            let category = info
                .get("category")
                .and_then(StaticValue::as_str)
                .unwrap_or("");
            let known = static_schema()
                .table("gen.json")
                .and_then(|value| value.get("gt_name"))
                .and_then(|value| value.get(category))
                .and_then(StaticValue::as_str);
            let value = if let Some(prefix) = known {
                text(budget, &[prefix, ".", name])?
            } else {
                text(budget, &[kind.as_str(), category, ".", name])?
            };
            self.types.insert(tid, value)?;
            self.type_owners.insert(
                tid,
                ManagedTypeOwner {
                    object: uuid,
                    child: None,
                    category,
                    recalc: None,
                },
            )?;
        }
        self.index_children(
            tree,
            pairs(spec.field("children")),
            full.as_str(),
            (kind, name, uuid),
            budget,
        )?;
        self.index_subobjects(entries, spec.raw(), kind, uuid, raw, name)?;
        if kind == MdKind::CalculationRegister {
            self.index_recalcs(entries, tree, uuid, full.as_str(), name)?;
        }
        Ok(())
    }
    fn index_children(
        &mut self,
        tree: Tree<'_>,
        children: &'static [(&'static str, StaticValue)],
        prefix: &str,
        (kind, name, owner): (MdKind, &str, Guid),
        budget: &MemoryBudget,
    ) -> std::result::Result<(), BudgetError> {
        for (class, spec) in children {
            let Some(header) = spec.get("header") else {
                continue;
            };
            if header.as_array().is_none() {
                continue;
            }
            let tag = spec.get("tag").and_then(StaticValue::as_str).unwrap_or("");
            let Some(collection) = tree.find_collection(class) else {
                continue;
            };
            for item in collection.iter() {
                let child_name = name_at(item, Some(header));
                let full = text(budget, &[prefix, ".", tag, ".", child_name])?;
                let child = at(item, spec.get("uuid")).and_then(Tree::as_guid);
                if let Some(uuid) = child {
                    self.names.insert(uuid, text(budget, &[full.as_str()])?)?;
                }
                for info in array(spec.get("internal_info")) {
                    let Some(tid) = at(item, info.get("type_id")).and_then(Tree::as_guid) else {
                        continue;
                    };
                    let category = info
                        .get("category")
                        .and_then(StaticValue::as_str)
                        .unwrap_or("");
                    self.types.insert(
                        tid,
                        text(
                            budget,
                            &[
                                md_xmlout::codec::objects::child_generated_kind(kind.as_str(), tag),
                                category,
                                ".",
                                name,
                                ".",
                                child_name,
                            ],
                        )?,
                    )?;
                    self.type_owners.insert(
                        tid,
                        ManagedTypeOwner {
                            object: owner,
                            child,
                            category,
                            recalc: None,
                        },
                    )?;
                }
                self.index_children(
                    item,
                    pairs(spec.get("children")),
                    full.as_str(),
                    (kind, name, owner),
                    budget,
                )?;
            }
        }
        Ok(())
    }
    fn index_recalcs<S: ManagedEntrySource + ?Sized>(
        &mut self,
        entries: &ManagedEntries<'_, S>,
        tree: Tree<'_>,
        owner: Guid,
        full: &str,
        name: &str,
    ) -> Result<(), S::Error> {
        let budget = entries.memory_budget();
        let Some(collection) = tree.find_collection(RECALC_CLASS) else {
            return Ok(());
        };
        for item in collection.iter() {
            let Some(uuid) = item.as_guid() else { continue };
            let entry = GuidText::new(uuid);
            let Some(data) = recoverable_doc(entries, entry.as_str()?)? else {
                continue;
            };
            let Some(raw) = doc_tree(&data) else { continue };
            let tree = Tree::new("Recalculation", raw);
            let Some(header) = tree.get(&RECALC_HEADER).filter(|node| node.is_list()) else {
                continue;
            };
            let rn = header.get(&[2]).and_then(Tree::text).unwrap_or("");
            let rfull = text(budget, &[full, ".Recalculation.", rn])?;
            if let Some(dimensions) = tree.find_collection(RECALC_DIM_CLASS) {
                for dim in dimensions.iter() {
                    let Some(id) = dim.get(&DIM_UUID).and_then(Tree::as_guid) else {
                        continue;
                    };
                    let dn = dim
                        .get(&DIM_HEADER)
                        .and_then(|node| node.get(&[2]))
                        .and_then(Tree::text)
                        .unwrap_or("");
                    self.names
                        .insert(id, text(budget, &[rfull.as_str(), ".Dimension.", dn])?)?;
                }
            }
            for (category, tid, _) in GENERATED {
                let Some(tid) = tree.get(&tid).and_then(Tree::as_guid) else {
                    continue;
                };
                self.types.insert(
                    tid,
                    text(budget, &["Recalculation", category, ".", name, ".", rn])?,
                )?;
                self.type_owners.insert(
                    tid,
                    ManagedTypeOwner {
                        object: owner,
                        child: None,
                        category,
                        recalc: Some(uuid),
                    },
                )?;
            }
            self.names.insert(uuid, rfull)?;
            append(&mut self.recalcs, owner, uuid, budget)?;
        }
        Ok(())
    }
    fn index_subobjects<S: ManagedEntrySource + ?Sized>(
        &mut self,
        entries: &ManagedEntries<'_, S>,
        spec: &'static StaticValue,
        kind: MdKind,
        root_owner: Guid,
        raw: &Node,
        root_name: &str,
    ) -> Result<(), S::Error> {
        struct Frame {
            owner: Guid,
            depth: usize,
            todo: BudgetedVec<(&'static str, Guid)>,
            full: BudgetedText,
            name: BudgetedText,
        }
        let budget = entries.memory_budget();
        let tree = Tree::new("", raw);
        for (tag, class) in pairs(spec.get("subobjects")) {
            if matches!(*tag, "Form" | "Template") || *tag == kind.as_str() {
                continue;
            }
            if class
                .as_str()
                .and_then(|class| tree.find_collection(class))
                .is_some_and(|list| !list.is_empty())
            {
                let owner = GuidText::new(root_owner);
                self.skipped.try_push(text(
                    budget,
                    &[
                        kind.as_str(),
                        " ",
                        owner.as_str()?,
                        ": ",
                        tag,
                        " sub-objects are not modelled (C3f)",
                    ],
                )?)?;
            }
        }
        let mut seen = GuidMap::new(budget)?;
        seen.insert(root_owner, ())?;
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        stack.try_push(Frame {
            owner: root_owner,
            depth: 0,
            todo: pending(tree, spec, kind, budget)?,
            full: text(
                budget,
                &[self
                    .names
                    .get(&root_owner)
                    .map(BudgetedText::as_str)
                    .unwrap_or("")],
            )?,
            name: text(budget, &[root_name])?,
        })?;
        while let Some(top) = stack.last_mut() {
            let Some((tag, uuid)) = top.todo.pop() else {
                stack.pop();
                continue;
            };
            let entry = GuidText::new(uuid);
            let entry = entry.as_str()?;
            let Some(data) = recoverable_doc(entries, entry)? else {
                continue;
            };
            let Some(raw) = doc_tree(&data) else { continue };
            let tree = Tree::new("", raw);
            if tag == "Same" {
                if seen.get(&uuid).is_some() {
                    continue;
                }
                seen.insert(uuid, ())?;
                let depth = top.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
                if depth > MAX_NESTING {
                    self.too_deep.try_push(uuid)?;
                    continue;
                }
                let name = name_at(tree, spec.get("header"));
                let full = text(budget, &[top.full.as_str(), ".", kind.as_str(), ".", name])?;
                self.objs
                    .insert(uuid, (kind, Some(text(budget, &[name])?)))?;
                self.names.insert(uuid, text(budget, &[full.as_str()])?)?;
                self.nested.try_push((uuid, top.owner))?;
                let todo = pending(tree, spec, kind, budget)?;
                stack.try_push(Frame {
                    owner: uuid,
                    depth,
                    todo,
                    full,
                    name: text(budget, &[name])?,
                })?;
                continue;
            }
            let Some(header) = tree.header_path(entry, budget, CATEGORY)? else {
                continue;
            };
            let name = tree
                .get(&header)
                .and_then(|node| node.get(&[2]))
                .and_then(Tree::text)
                .unwrap_or("");
            let subkind = if tag == "Form" {
                SubKind::Form
            } else {
                SubKind::Template
            };
            self.names.insert(
                uuid,
                text(
                    budget,
                    &[kind.as_str(), ".", top.name.as_str(), ".", tag, ".", name],
                )?,
            )?;
            self.sub_hdr.insert(uuid, header)?;
            append(&mut self.subs, top.owner, (subkind, uuid), budget)?;
        }
        Ok(())
    }
}

fn pending(
    tree: Tree<'_>,
    spec: &'static StaticValue,
    kind: MdKind,
    budget: &MemoryBudget,
) -> std::result::Result<BudgetedVec<(&'static str, Guid)>, BudgetError> {
    let mut pending = BudgetedVec::new(budget, CATEGORY)?;
    for (tag, class) in pairs(spec.get("subobjects")) {
        let tag = match *tag {
            "Form" => "Form",
            "Template" => "Template",
            same if same == kind.as_str() => "Same",
            _ => continue,
        };
        let Some(items) = class.as_str().and_then(|class| tree.find_collection(class)) else {
            continue;
        };
        for item in items.iter() {
            if let Some(uuid) = item.as_guid() {
                pending.try_push((tag, uuid))?;
            }
        }
    }
    pending.reverse();
    Ok(pending)
}
fn append<T>(
    map: &mut GuidMap<BudgetedVec<T>>,
    key: Guid,
    value: T,
    budget: &MemoryBudget,
) -> std::result::Result<(), BudgetError> {
    if let Some(items) = map.get_mut(&key) {
        items.try_push(value)?;
    } else {
        let mut items = BudgetedVec::new(budget, CATEGORY)?;
        items.try_push(value)?;
        map.insert(key, items)?;
    }
    Ok(())
}
pub(super) fn doc_tree(data: &EntryData) -> Option<&Node> {
    if let EntryData::Brace(doc) = data {
        Some(&doc.root)
    } else {
        None
    }
}
fn array(value: Option<&StaticValue>) -> &'static [StaticValue] {
    value.and_then(StaticValue::as_array).unwrap_or(&[])
}
fn pairs(value: Option<&StaticValue>) -> &'static [(&'static str, StaticValue)] {
    value.and_then(StaticValue::as_object).unwrap_or(&[])
}
fn at<'a>(mut tree: Tree<'a>, path: Option<&StaticValue>) -> Option<Tree<'a>> {
    for index in path?.as_array()? {
        tree = tree.get(&[usize::try_from(index.as_u64()?).ok()?])?;
    }
    Some(tree)
}
fn name_at<'a>(tree: Tree<'a>, header: Option<&StaticValue>) -> &'a str {
    at(tree, header)
        .and_then(|node| node.get(&[2]))
        .and_then(Tree::text)
        .unwrap_or("")
}
pub fn text(
    budget: &MemoryBudget,
    parts: &[&str],
) -> std::result::Result<BudgetedText, BudgetError> {
    let capacity = parts.iter().try_fold(0usize, |sum, part| {
        sum.checked_add(part.len()).ok_or(BudgetError::Overflow)
    })?;
    let mut result = BudgetedText::try_with_capacity(budget, CATEGORY, u64::MAX, capacity)?;
    for part in parts {
        result.try_push_str(part)?;
    }
    Ok(result)
}
pub struct GuidText([u8; 36]);
impl GuidText {
    pub fn new(uuid: Guid) -> Self {
        let mut bytes = [b'-'; 36];
        let digits = uuid
            .as_bytes()
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 15])
            .map(|digit| {
                if digit < 10 {
                    b'0' + digit
                } else {
                    b'a' + digit - 10
                }
            });
        let positions = bytes
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| !matches!(index, 8 | 13 | 18 | 23))
            .map(|(_, byte)| byte);
        for (slot, digit) in positions.zip(digits) {
            *slot = digit;
        }
        Self(bytes)
    }
    pub fn as_str(&self) -> std::result::Result<&str, BudgetError> {
        std::str::from_utf8(&self.0).map_err(|_| BudgetError::Overflow)
    }
}

fn recoverable_doc<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    name: &str,
) -> Result<Option<memory_budget::Shared<EntryData>>, S::Error> {
    match entries.doc(name) {
        Ok(data) => Ok(data),
        Err(error) if entries.recoverable_content_error(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
