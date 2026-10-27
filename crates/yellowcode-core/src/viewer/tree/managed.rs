use super::{CHILD_ORDER, module_rank, static_order as order};
use crate::viewer::{
    Lang,
    icons::NodeRef,
    names::{self, ManagedNames},
};
use mdconf::{
    ChildTag, LocalString, ManagedEntrySource, MdKind, TypeDescription,
    startup::{ChildFacts, ManagedViewerOpen, ModuleLocation, ObjectFacts},
};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, LimitReason, MemoryBudget, MemoryLimit,
    TextCount,
};
use platform_catalog::ManagedCatalog;

type Result<T> = std::result::Result<T, BudgetError>;
#[derive(Debug)]
pub struct ManagedFlatNode {
    pub label: BudgetedText,
    pub path: Option<BudgetedText>,
    pub module: Option<BudgetedText>,
    pub synonym: Option<BudgetedText>,
    pub type_text: Option<BudgetedText>,
    pub icon: BudgetedText,
    pub children: BudgetedVec<usize>,
}
#[derive(Debug)]
pub struct ManagedTree {
    nodes: BudgetedVec<ManagedFlatNode>,
}
impl ManagedTree {
    pub fn build<S: ManagedEntrySource>(
        open: &ManagedViewerOpen<S>,
        catalog: &ManagedCatalog,
        lang: Lang,
    ) -> Result<Self> {
        let budget = open.context().index().memory_budget();
        if !budget.same_scope(catalog.memory_budget()) {
            return Err(BudgetError::Limit(MemoryLimit {
                category: Category::Metadata,
                requested: 0,
                reason: LimitReason::InvalidSize,
            }));
        }
        let mut builder = Builder {
            open,
            names: ManagedNames::new(catalog),
            budget,
            lang,
            nodes: BudgetedVec::new(budget, Category::Metadata)?,
        };
        builder.root()?;
        Ok(Self {
            nodes: builder.nodes,
        })
    }
    pub fn nodes(&self) -> &[ManagedFlatNode] {
        &self.nodes
    }
}
struct Builder<'a, S: ManagedEntrySource> {
    open: &'a ManagedViewerOpen<S>,
    names: ManagedNames<'a>,
    budget: &'a MemoryBudget,
    lang: Lang,
    nodes: BudgetedVec<ManagedFlatNode>,
}
fn text(budget: &MemoryBudget, parts: &[&str]) -> Result<BudgetedText> {
    let capacity = parts.iter().try_fold(0usize, |n, s| {
        n.checked_add(s.len()).ok_or(BudgetError::Overflow)
    })?;
    let mut out = BudgetedText::try_with_capacity(
        budget,
        Category::Metadata,
        u64::try_from(capacity).map_err(|_| BudgetError::Overflow)?,
        capacity,
    )?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn synonym(value: &LocalString, lang: Lang) -> Option<&str> {
    let code = if lang == Lang::Ru { "ru" } else { "en" };
    value
        .0
        .iter()
        .find(|(language, _)| language == code)
        .or_else(|| value.0.first())
        .map(|(_, s)| s.as_str())
        .filter(|s| !s.is_empty())
}
impl<S: ManagedEntrySource> Builder<'_, S> {
    fn node(&self, label: &str, icon: NodeRef<'_>) -> Result<ManagedFlatNode> {
        let icon = match icon {
            NodeRef::Root => text(self.budget, &["root"]),
            NodeRef::Group(key) => text(self.budget, &["group:", key]),
            NodeRef::Section(kind) => text(self.budget, &["section:", kind.as_str()]),
            NodeRef::Object(kind) => text(self.budget, &["object:", kind.as_str()]),
            NodeRef::Folder(key) => text(
                self.budget,
                &["folder:", key, if key.ends_with('s') { "" } else { "s" }],
            ),
            NodeRef::Child(tag) => text(self.budget, &["child:", names::child_tag_en(tag)]),
            NodeRef::Sub(kind) => text(self.budget, &["sub:", names::sub_kind_en(kind)]),
            NodeRef::Recalc => text(self.budget, &["recalc"]),
            NodeRef::Module(kind) => text(
                self.budget,
                &["module:", names::module_label(kind, Lang::En)],
            ),
        }?;
        Ok(ManagedFlatNode {
            label: text(self.budget, &[label])?,
            path: None,
            module: None,
            synonym: None,
            type_text: None,
            icon,
            children: BudgetedVec::new(self.budget, Category::Metadata)?,
        })
    }
    fn append(&mut self, parent: Option<usize>, node: ManagedFlatNode) -> Result<usize> {
        let at = self.nodes.len();
        self.nodes.try_push(node)?;
        if let Some(parent) = parent {
            self.nodes
                .get_mut(parent)
                .ok_or(BudgetError::Overflow)?
                .children
                .try_push(at)?;
        }
        Ok(at)
    }
    fn path(&self, owner: usize, parts: &[&str]) -> Result<BudgetedText> {
        let prefix = self
            .nodes
            .get(owner)
            .and_then(|n| n.path.as_ref())
            .ok_or(BudgetError::Overflow)?
            .as_str();
        let extra = parts.iter().try_fold(prefix.len(), |n, s| {
            n.checked_add(s.len()).ok_or(BudgetError::Overflow)
        })?;
        let mut out = BudgetedText::try_with_capacity(
            self.budget,
            Category::Metadata,
            u64::try_from(extra).map_err(|_| BudgetError::Overflow)?,
            extra,
        )?;
        out.try_push_str(prefix)?;
        for part in parts {
            out.try_push_str(part)?;
        }
        Ok(out)
    }
    fn description(&self, value: Option<&TypeDescription>) -> Result<Option<BudgetedText>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let mut count = TextCount::new(Category::Metadata, u64::MAX);
        self.names.write_type_text(value, self.lang, &mut count)?;
        let capacity = usize::try_from(count.len()).map_err(|_| BudgetError::Overflow)?;
        let mut out = BudgetedText::try_with_capacity(
            self.budget,
            Category::Metadata,
            count.len(),
            capacity,
        )?;
        self.names.write_type_text(value, self.lang, &mut out)?;
        Ok(Some(out))
    }
    fn copy_synonym(&self, value: &LocalString) -> Result<Option<BudgetedText>> {
        synonym(value, self.lang)
            .map(|s| text(self.budget, &[s]))
            .transpose()
    }
    fn present(&self, kind: MdKind) -> bool {
        self.open
            .objects()
            .iter()
            .any(|r| r.parent.is_none() && r.object.kind == kind)
    }
    fn root(&mut self) -> Result<()> {
        let mut node = self.node(self.open.identity().name.as_str(), NodeRef::Root)?;
        node.path = Some(text(self.budget, &["Configuration"])?);
        let root = self.append(None, node)?;
        if order::COMMON
            .iter()
            .filter_map(|k| MdKind::from_name(k))
            .any(|k| self.present(k))
        {
            let common = self.node(
                names::group_label("Common", self.lang),
                NodeRef::Group("Common"),
            )?;
            let common = self.append(Some(root), common)?;
            for kind in order::COMMON.iter().filter_map(|k| MdKind::from_name(k)) {
                self.section(common, kind)?;
            }
        }
        for kind in top().chain(rest()).filter(|kind| !is_nested(*kind)) {
            self.section(root, kind)?;
        }
        Ok(())
    }
    fn section(&mut self, parent: usize, kind: MdKind) -> Result<()> {
        if !self.present(kind)
            && !nested(kind)
                .iter()
                .filter_map(|k| MdKind::from_name(k))
                .any(|k| self.present(k))
        {
            return Ok(());
        }
        let node = self.node(
            self.names.section_name(kind, self.lang),
            NodeRef::Section(kind),
        )?;
        let section = self.append(Some(parent), node)?;
        for kind in nested(kind).iter().filter_map(|k| MdKind::from_name(k)) {
            if !self.present(kind) {
                continue;
            }
            let node = self.node(
                self.names.section_name(kind, self.lang),
                NodeRef::Section(kind),
            )?;
            let group = self.append(Some(section), node)?;
            self.section_objects(group, kind)?;
        }
        self.section_objects(section, kind)
    }
    fn section_objects(&mut self, parent: usize, kind: MdKind) -> Result<()> {
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            if row.parent.is_none() && row.object.kind == kind {
                self.object(parent, ordinal, None)?;
            }
        }
        Ok(())
    }
    fn object(&mut self, parent: usize, ordinal: usize, owner: Option<usize>) -> Result<()> {
        let row = self
            .open
            .objects()
            .get(ordinal)
            .ok_or(BudgetError::Overflow)?;
        let object = &row.object;
        let mut node = self.node(&object.name, NodeRef::Object(object.kind))?;
        node.path = Some(match owner {
            Some(owner) => self.path(owner, &[".Subsystem.", &object.name])?,
            None => text(self.budget, &[object.kind.as_str(), ".", &object.name])?,
        });
        node.synonym = self.copy_synonym(&object.synonym)?;
        node.type_text = self.description(object.description.as_ref())?;
        let at = self.append(Some(parent), node)?;
        for (child, row) in self
            .open
            .objects()
            .iter()
            .enumerate()
            .skip(ordinal.checked_add(1).ok_or(BudgetError::Overflow)?)
            .take_while(|(_, r)| r.parent.is_some_and(|p| p >= ordinal))
        {
            if row.parent == Some(ordinal) {
                self.object(at, child, Some(at))?;
            }
        }
        let mut groups: [Group; 19] = std::array::from_fn(|i| {
            CHILD_ORDER
                .get(i)
                .copied()
                .map(Group::Child)
                .unwrap_or(match i {
                    14 => Group::Forms,
                    15 => Group::Child(ChildTag::Command),
                    16 => Group::Child(ChildTag::Function),
                    17 => Group::Templates,
                    _ => Group::Recalculations,
                })
        });
        for i in 1..groups.len() {
            let mut j = i;
            while j > 0
                && rank(object.kind, *groups.get(j).ok_or(BudgetError::Overflow)?)
                    < rank(
                        object.kind,
                        *groups.get(j - 1).ok_or(BudgetError::Overflow)?,
                    )
            {
                groups.swap(j, j - 1);
                j -= 1;
            }
        }
        for group in groups {
            if group.present(object) {
                self.group(at, ordinal, group)?;
            }
        }
        self.modules(at, ordinal, ModuleLocation::Object)
    }
    fn group(&mut self, parent: usize, ordinal: usize, group: Group) -> Result<()> {
        let object = &self
            .open
            .objects()
            .get(ordinal)
            .ok_or(BudgetError::Overflow)?
            .object;
        let node = self.node(
            names::group_label(group.key(), self.lang),
            NodeRef::Folder(group.key()),
        )?;
        let at = self.append(Some(parent), node)?;
        match group {
            Group::Child(tag) => {
                for child in object.children.iter().filter(|c| c.tag == tag) {
                    self.child(at, parent, child)?;
                }
            }
            Group::Forms | Group::Templates => {
                let subs = if matches!(group, Group::Forms) {
                    &object.forms
                } else {
                    &object.templates
                };
                for sub in subs.iter() {
                    let mut node = self.node(&sub.name, NodeRef::Sub(sub.kind))?;
                    node.path = Some(
                        self.path(parent, &[".", names::sub_kind_en(sub.kind), ".", &sub.name])?,
                    );
                    node.synonym = sub
                        .synonym
                        .as_ref()
                        .map(|s| self.copy_synonym(s))
                        .transpose()?
                        .flatten();
                    self.append(Some(at), node)?;
                }
            }
            Group::Recalculations => {
                for (index, recalc) in object.recalculations.iter().enumerate() {
                    let mut node = self.node(&recalc.name, NodeRef::Recalc)?;
                    node.path = Some(self.path(parent, &[".Recalculation.", &recalc.name])?);
                    node.synonym = self.copy_synonym(&recalc.synonym)?;
                    let recalc_at = self.append(Some(at), node)?;
                    self.modules(recalc_at, ordinal, ModuleLocation::Recalculation(index))?;
                }
            }
        }
        Ok(())
    }
    fn child(&mut self, parent: usize, owner: usize, child: &ChildFacts) -> Result<()> {
        let mut node = self.node(&child.name, NodeRef::Child(child.tag))?;
        node.path = Some(self.path(
            owner,
            &[".", names::child_tag_en(child.tag), ".", &child.name],
        )?);
        node.synonym = self.copy_synonym(&child.synonym)?;
        node.type_text = self.description(child.description.as_ref())?;
        let at = self.append(Some(parent), node)?;
        for child in child.children.iter() {
            self.child(at, at, child)?;
        }
        Ok(())
    }
    fn modules(&mut self, parent: usize, ordinal: usize, location: ModuleLocation) -> Result<()> {
        let all = self.open.modules();
        let start = all.partition_point(|m| m.object.is_some_and(|owner| owner < ordinal));
        let tail = all.get(start..).ok_or(BudgetError::Overflow)?;
        let end = tail.partition_point(|m| m.object == Some(ordinal));
        let modules = tail.get(..end).ok_or(BudgetError::Overflow)?;
        if !modules.iter().any(|m| m.location == location) {
            return Ok(());
        }
        let node = self.node(
            names::group_label("Modules", self.lang),
            NodeRef::Folder("Modules"),
        )?;
        let at = self.append(Some(parent), node)?;
        for rank in 0..=10 {
            for module in modules
                .iter()
                .filter(|m| m.location == location && module_rank(m.slot.kind) == rank)
            {
                let mut node = self.node(
                    names::module_label(module.slot.kind, self.lang),
                    NodeRef::Module(module.slot.kind),
                )?;
                node.path = Some(self.path(parent, &[])?);
                node.module = Some(text(
                    self.budget,
                    &[names::module_label(module.slot.kind, Lang::En)],
                )?);
                self.append(Some(at), node)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Group {
    Child(ChildTag),
    Forms,
    Templates,
    Recalculations,
}
impl Group {
    fn key(self) -> &'static str {
        match self {
            Self::Child(tag) => names::child_tag_en(tag),
            Self::Forms => "Forms",
            Self::Templates => "Templates",
            Self::Recalculations => "Recalculations",
        }
    }
    fn present(self, object: &ObjectFacts) -> bool {
        match self {
            Self::Child(tag) => object.children.iter().any(|c| c.tag == tag),
            Self::Forms => !object.forms.is_empty(),
            Self::Templates => !object.templates.is_empty(),
            Self::Recalculations => !object.recalculations.is_empty(),
        }
    }
}
fn rank(kind: MdKind, group: Group) -> usize {
    let key = group.key();
    let key = key.strip_suffix('s').unwrap_or(key);
    order::CHILDREN
        .iter()
        .find(|(owner, _)| *owner == kind.as_str())
        .and_then(|(_, list)| list.iter().position(|name| *name == key))
        .unwrap_or(usize::MAX)
}
fn nested(kind: MdKind) -> &'static [&'static str] {
    order::AFTER
        .iter()
        .find(|(owner, _)| *owner == kind.as_str())
        .map_or(&[], |(_, kinds)| *kinds)
}
fn is_nested(kind: MdKind) -> bool {
    order::AFTER
        .iter()
        .any(|(_, kinds)| kinds.contains(&kind.as_str()))
}
fn top() -> impl Iterator<Item = MdKind> {
    order::ROOT
        .iter()
        .filter_map(|kind| MdKind::from_name(kind))
        .flat_map(|kind| {
            std::iter::once(kind).chain(
                nested(kind)
                    .iter()
                    .filter_map(|kind| MdKind::from_name(kind)),
            )
        })
}
fn rest() -> impl Iterator<Item = MdKind> {
    MdKind::all().iter().copied().filter(|kind| {
        *kind != MdKind::Configuration
            && !order::COMMON.contains(&kind.as_str())
            && !top().any(|k| k == *kind)
    })
}
