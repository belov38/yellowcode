use super::index::{GuidText, text};
use super::{GuidMap, ManagedDiagnostics, ManagedIndex, ManagedPredefined, ManagedValueContext};
use crate::{
    EntryData, LocalString, ManagedEntries, ManagedEntriesError, ManagedEntrySource, MdKind,
    MdObject, SubKind, SubObject,
};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug)]
pub struct ManagedObjects {
    objects: Charged<Vec<MdObject>>,
    locations: BudgetedVec<Location>,
    by_id: GuidMap<usize>,
}
#[derive(Debug, thiserror::Error)]
pub enum ManagedObjectsError<E> {
    #[error("{0}")]
    Entries(#[from] ManagedEntriesError<E>),
    #[error("{0}")]
    Memory(#[from] BudgetError),
}
#[derive(Debug)]
pub struct ManagedDecodedObjects {
    pub objects: ManagedObjects,
    pub diagnostics: ManagedDiagnostics,
}
impl ManagedObjects {
    pub fn shells(index: &ManagedIndex) -> Result<Self, BudgetError> {
        let budget = index.memory_budget();
        let count = index
            .order
            .iter()
            .filter(|id| name_of(index, id).is_some())
            .count();
        let mut objects = vector(budget, count)?;
        let mut top = GuidMap::new(budget)?;
        for id in index.order.iter() {
            let Some((kind, name)) = name_of(index, id) else {
                continue;
            };
            if top.get(id).is_none() {
                top.insert(*id, objects.len())?;
            }
            objects = push(objects, shell(index, kind, *id, name)?)?;
        }
        let mut pool = GuidMap::new(budget)?;
        for (child, _) in index.nested.iter() {
            if let Some((kind, name)) = name_of(index, child) {
                pool.insert(*child, Some(shell(index, kind, *child, name)?))?;
            }
        }
        for (child, parent) in index.nested.iter().rev() {
            let Some(child) = pool.get_mut(child).and_then(Option::take) else {
                continue;
            };
            let child = child.map(|mut child| {
                child.subsystems.reverse();
                child
            });
            if let Some(owner) = pool.get_mut(parent).and_then(Option::take) {
                if owner.subsystems.len() >= owner.subsystems.capacity() {
                    return Err(BudgetError::Overflow);
                }
                let owner = owner.try_zip(child)?.map(|(mut owner, child)| {
                    owner.subsystems.push(child);
                    owner
                });
                let slot = pool.get_mut(parent).ok_or(BudgetError::Overflow)?;
                *slot = Some(owner);
            } else if let Some(at) = top.get(parent).copied() {
                let parent = objects.get(at).ok_or(BudgetError::Overflow)?;
                if parent.subsystems.len() >= parent.subsystems.capacity() {
                    return Err(BudgetError::Overflow);
                }
                objects = objects.try_zip(child)?.map(|(mut objects, child)| {
                    if let Some(parent) = objects.get_mut(at) {
                        parent.subsystems.push(child);
                    }
                    objects
                });
            }
        }
        let objects = objects.map(|mut objects| {
            for object in &mut objects {
                object.subsystems.reverse();
            }
            objects
        });
        let (locations, by_id) = build_locations(&objects, budget)?;
        Ok(Self {
            objects,
            locations,
            by_id,
        })
    }

    pub fn decode<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predefined: &ManagedPredefined,
        keep_source: bool,
    ) -> Result<ManagedDecodedObjects, ManagedObjectsError<S::Error>> {
        if !self
            .objects
            .reservation()
            .budget()
            .same_scope(index.memory_budget())
            || !entries.memory_budget().same_scope(index.memory_budget())
        {
            return Err(BudgetError::Overflow.into());
        }
        let context = ManagedValueContext::new(index, predefined)?;
        let mut diagnostics = ManagedDiagnostics::new(index.memory_budget())?;
        for sets_first in [true, false] {
            for at in 0..self.locations.len() {
                let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
                let is_set = self.locations.get(at).is_some_and(|p| p.depth == 0)
                    && object.kind == MdKind::DefinedType;
                if is_set != sets_first {
                    continue;
                }
                self = self.decode_one(at, entries, &context, keep_source, &mut diagnostics)?;
            }
        }
        Ok(ManagedDecodedObjects {
            objects: self,
            diagnostics,
        })
    }
    fn decode_one<S: ManagedEntrySource + ?Sized>(
        mut self,
        at: usize,
        entries: &ManagedEntries<'_, S>,
        context: &ManagedValueContext<'_>,
        keep_source: bool,
        diagnostics: &mut ManagedDiagnostics,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
        if !super::kind_spec(object.kind.as_str()).is_some_and(|spec| spec.header().is_some()) {
            return Ok(self);
        }
        let id = object.id;
        let entry = GuidText::new(id);
        if let Some(data) = entries.doc(entry.as_str()?)?
            && let EntryData::Brace(doc) = &*data
        {
            let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
            let content = context.object_content_with_sets(
                object.kind,
                &object.name,
                &doc.root,
                Some(&self),
            )?;
            diagnostics.merge(content.diagnostics)?;
            self = self.install(at, content.values, metadata_empty, |object, content| {
                object.props = content.props;
                object.synonym = content.synonym;
                object.comment = content.comment;
                object.generated_types = content.generated_types;
                object.this_node = content.this_node;
                object.children = content.children;
                None
            })?;
        }
        if !keep_source {
            entries.forget(entry.as_str()?);
        }
        for kind in [SubKind::Form, SubKind::Template] {
            let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
            let count = match kind {
                SubKind::Form => object.forms.len(),
                SubKind::Template => object.templates.len(),
            };
            for sub_at in 0..count {
                let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
                let sub = match kind {
                    SubKind::Form => object.forms.get(sub_at),
                    SubKind::Template => object.templates.get(sub_at),
                }
                .ok_or(BudgetError::Overflow)?;
                let id = sub.id;
                let entry = GuidText::new(id);
                if let Some(data) = entries.doc(entry.as_str()?)?
                    && let EntryData::Brace(doc) = &*data
                {
                    let properties = context.subobject_properties(kind, id, &doc.root)?;
                    diagnostics.merge(properties.diagnostics)?;
                    if properties.values.synonym.0.capacity() != 0
                        || properties.values.comment.capacity() != 0
                    {
                        return Err(BudgetError::Overflow.into());
                    }
                    self = self.install(
                        at,
                        properties.values.map(|p| p.props),
                        |object| {
                            match kind {
                                SubKind::Form => object.forms.get(sub_at),
                                SubKind::Template => object.templates.get(sub_at),
                            }
                            .is_some_and(|sub| sub.props.capacity() == 0)
                        },
                        |object, props| {
                            let Some(sub) = (match kind {
                                SubKind::Form => object.forms.get_mut(sub_at),
                                SubKind::Template => object.templates.get_mut(sub_at),
                            }) else {
                                return Some(props);
                            };
                            sub.props = props;
                            None
                        },
                    )?;
                }
                if !keep_source {
                    entries.forget(entry.as_str()?);
                }
            }
        }
        let ids = context
            .index
            .recalcs
            .get(&id)
            .map(|ids| ids.as_slice())
            .unwrap_or_default();
        let mut recalculations = vector(context.memory_budget(), ids.len())?;
        for id in ids {
            let entry = GuidText::new(*id);
            if let Some(data) = entries.doc(entry.as_str()?)?
                && let EntryData::Brace(doc) = &*data
            {
                let object = self.resolve(at).ok_or(BudgetError::Overflow)?;
                if let Some(decoded) =
                    context.recalculation((object.kind.as_str(), &object.name), *id, &doc.root)?
                {
                    diagnostics.merge(decoded.diagnostics)?;
                    recalculations = push(recalculations, decoded.values)?;
                }
            }
            if !keep_source {
                entries.forget(entry.as_str()?);
            }
        }
        Ok(self.install(
            at,
            recalculations,
            |object| object.recalculations.capacity() == 0,
            |object, recalculations| {
                object.recalculations = recalculations;
                None
            },
        )?)
    }
    fn install<T>(
        self,
        at: usize,
        value: Charged<T>,
        check: impl FnOnce(&MdObject) -> bool,
        apply: impl FnOnce(&mut MdObject, T) -> Option<T>,
    ) -> Result<Self, BudgetError> {
        if !check(self.resolve(at).ok_or(BudgetError::Overflow)?) {
            return Err(BudgetError::Overflow);
        }
        let Self {
            objects,
            locations,
            by_id,
        } = self;
        let installed = objects.try_zip(value)?.map(|(mut objects, value)| {
            let refused = match resolve_mut(&mut objects, &locations, at) {
                Some(object) => apply(object, value),
                None => Some(value),
            };
            (objects, refused)
        });
        if installed.1.is_some() {
            return Err(BudgetError::Overflow);
        }
        let objects = installed.map(|(objects, _)| objects);
        Ok(Self {
            objects,
            locations,
            by_id,
        })
    }
    fn install_template(
        self,
        at: usize,
        sub: Option<usize>,
        value: Charged<crate::templates::Template>,
    ) -> Result<Self, BudgetError> {
        self.install(
            at,
            value,
            |object| match sub {
                None => object.template.is_none(),
                Some(sub) => object
                    .templates
                    .get(sub)
                    .is_some_and(|sub| sub.template.is_none()),
            },
            |object, value| {
                match sub {
                    None => object.template = Some(value),
                    Some(sub) => match object.templates.get_mut(sub) {
                        Some(sub) => sub.template = Some(value),
                        None => return Some(value),
                    },
                }
                None
            },
        )
    }
    fn install_flowchart(
        self,
        at: usize,
        value: Charged<crate::graph::Flowchart>,
    ) -> Result<Self, BudgetError> {
        self.install(
            at,
            value,
            |object| object.flowchart.is_none(),
            |object, value| {
                object.flowchart = Some(value);
                None
            },
        )
    }
    pub fn as_slice(&self) -> &[MdObject] {
        &self.objects
    }
    pub fn all_objects(&self) -> ManagedObjectIter<'_> {
        ManagedObjectIter {
            graph: self,
            next: 0,
        }
    }
    pub fn object(&self, id: Guid) -> Option<&MdObject> {
        self.resolve(*self.by_id.get(&id)?)
    }
    fn resolve(&self, at: usize) -> Option<&MdObject> {
        let depth = self.locations.get(at)?.depth;
        let mut objects = self.as_slice();
        let mut found = None;
        for remaining in (0..=depth).rev() {
            let mut ancestor = at;
            for _ in 0..remaining {
                ancestor = self.locations.get(ancestor)?.parent?;
            }
            let slot = self.locations.get(ancestor)?.slot;
            let object = objects.get(slot)?;
            found = Some(object);
            objects = &object.subsystems;
        }
        found
    }
    pub fn find(&self, kind: MdKind, name: &str) -> Option<&MdObject> {
        let hit = |object: &&MdObject| object.kind == kind && object.name == name;
        self.objects
            .iter()
            .find(hit)
            .or_else(|| self.all_objects().find(hit))
    }
    pub fn of_kind(&self, kind: MdKind) -> impl Iterator<Item = &MdObject> {
        self.all_objects().filter(move |object| object.kind == kind)
    }
    pub fn into_charged(self) -> Charged<Vec<MdObject>> {
        self.objects
    }
}

fn metadata_empty(object: &MdObject) -> bool {
    object.props.capacity() == 0
        && object.synonym.0.capacity() == 0
        && object.comment.capacity() == 0
        && object.generated_types.capacity() == 0
        && object.children.capacity() == 0
}
fn resolve_mut<'a>(
    mut objects: &'a mut [MdObject],
    locations: &[Location],
    at: usize,
) -> Option<&'a mut MdObject> {
    let depth = locations.get(at)?.depth;
    for remaining in (0..=depth).rev() {
        let mut ancestor = at;
        for _ in 0..remaining {
            ancestor = locations.get(ancestor)?.parent?;
        }
        let slot = locations.get(ancestor)?.slot;
        let object = objects.get_mut(slot)?;
        if remaining == 0 {
            return Some(object);
        }
        objects = &mut object.subsystems;
    }
    None
}

pub struct ManagedObjectIter<'a> {
    graph: &'a ManagedObjects,
    next: usize,
}
impl<'a> Iterator for ManagedObjectIter<'a> {
    type Item = &'a MdObject;
    fn next(&mut self) -> Option<Self::Item> {
        let at = self.next;
        self.next = self.next.checked_add(1)?;
        self.graph.resolve(at)
    }
}
#[derive(Debug)]
struct Location {
    parent: Option<usize>,
    slot: usize,
    depth: usize,
}
fn build_locations(
    objects: &[MdObject],
    budget: &MemoryBudget,
) -> Result<(BudgetedVec<Location>, GuidMap<usize>), BudgetError> {
    struct Frame<'a> {
        objects: &'a [MdObject],
        next: usize,
        parent: Option<usize>,
        depth: usize,
    }
    let mut stack = BudgetedVec::new(budget, Category::Metadata)?;
    let mut locations = BudgetedVec::new(budget, Category::Metadata)?;
    let mut by_id = GuidMap::new(budget)?;
    stack.try_push(Frame {
        objects,
        next: 0,
        parent: None,
        depth: 0,
    })?;
    while let Some(frame) = stack.last_mut() {
        let Some(object) = frame.objects.get(frame.next) else {
            stack.pop();
            continue;
        };
        let slot = frame.next;
        frame.next = frame.next.checked_add(1).ok_or(BudgetError::Overflow)?;
        let row = locations.len();
        let depth = frame.depth;
        locations.try_push(Location {
            parent: frame.parent,
            slot,
            depth,
        })?;
        if by_id.get(&object.id).is_none() {
            by_id.insert(object.id, row)?;
        }
        if !object.subsystems.is_empty() {
            stack.try_push(Frame {
                objects: &object.subsystems,
                next: 0,
                parent: Some(row),
                depth: depth.checked_add(1).ok_or(BudgetError::Overflow)?,
            })?;
        }
    }
    Ok((locations, by_id))
}

fn name_of<'a>(index: &'a ManagedIndex, id: &Guid) -> Option<(MdKind, &'a str)> {
    let (kind, name) = index.objs.get(id)?;
    Some((*kind, name.as_ref()?.as_str()))
}
fn shell(
    index: &ManagedIndex,
    kind: MdKind,
    id: Guid,
    name: &str,
) -> Result<Charged<MdObject>, BudgetError> {
    let budget = index.memory_budget();
    let name = text(budget, &[name])?.into_string()?;
    let listed = index
        .subs
        .get(&id)
        .map(|items| items.as_slice())
        .unwrap_or(&[]);
    let mut forms = vector(
        budget,
        listed
            .iter()
            .filter(|(kind, _)| *kind == SubKind::Form)
            .count(),
    )?;
    let mut templates = vector(
        budget,
        listed
            .iter()
            .filter(|(kind, _)| *kind == SubKind::Template)
            .count(),
    )?;
    for (subkind, uuid) in listed {
        let full = index
            .names
            .get(uuid)
            .map(|name| name.as_str())
            .unwrap_or("");
        let name = full.rsplit('.').next().unwrap_or("");
        let sub = text(budget, &[name])?.into_string()?.map(|name| SubObject {
            kind: *subkind,
            id: *uuid,
            name,
            props: Vec::new(),
            module: None,
            form: None,
            template: None,
        });
        match subkind {
            SubKind::Form => forms = push(forms, sub)?,
            SubKind::Template => templates = push(templates, sub)?,
        }
    }
    let subsystems = vector(
        budget,
        index
            .nested
            .iter()
            .filter(|(_, parent)| *parent == id)
            .count(),
    )?;
    Ok(name
        .try_zip(forms)?
        .try_zip(templates)?
        .try_zip(subsystems)?
        .map(|(((name, forms), templates), subsystems)| MdObject {
            kind,
            id,
            name,
            synonym: LocalString::default(),
            comment: String::new(),
            generated_types: Vec::new(),
            this_node: None,
            props: Vec::new(),
            children: Vec::new(),
            forms,
            templates,
            recalculations: Vec::new(),
            subsystems,
            modules: Vec::new(),
            parts: Default::default(),
            form: None,
            template: None,
            flowchart: None,
        }))
}
fn vector<T>(budget: &MemoryBudget, count: usize) -> Result<Charged<Vec<T>>, BudgetError> {
    Ok(BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?.into_charged_vec())
}
fn push<T>(values: Charged<Vec<T>>, value: Charged<T>) -> Result<Charged<Vec<T>>, BudgetError> {
    if values.len() >= values.capacity() {
        return Err(BudgetError::Overflow);
    }
    Ok(values.try_zip(value)?.map(|(mut values, value)| {
        values.push(value);
        values
    }))
}

#[path = "modules.rs"]
mod managed_modules;
pub use managed_modules::{ManagedFormModule, ManagedModuleGraph};

#[path = "forms_graph.rs"]
mod managed_forms;

#[path = "pictures_graph.rs"]
mod managed_pictures;
pub use managed_pictures::ManagedPartsGraph;

#[path = "help_graph.rs"]
mod managed_help;

#[path = "configuration_graph.rs"]
mod managed_configuration;
pub use managed_configuration::ManagedConfigurationGraph;

#[path = "binary_parts.rs"]
mod managed_binary_parts;

#[path = "exchange_graph.rs"]
mod managed_exchange_content;

#[path = "schedule_graph.rs"]
mod managed_schedules;

#[path = "style_graph.rs"]
mod managed_styles;

#[path = "remaining_parts.rs"]
mod remaining_parts;

#[path = "c6.rs"]
mod managed_c6;

#[path = "final_configuration.rs"]
mod final_configuration;
pub use final_configuration::{GeneratedTypeView, ManagedConfiguration, ManagedConfigurationError};
