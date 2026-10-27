use super::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedIndex, ManagedPredefined,
    ManagedSettings, ManagedValueContext,
    defined_types::{DefinedType, Prefix},
    index::GuidText,
    objects::selected_shell,
    values::append,
};
use crate::{
    ConfigSettings, EntryData, ManagedEntries, ManagedEntrySource, MdKind, MdObject, RootInfo,
    SubKind, Value,
};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, Shared};

pub struct ManagedViewerContext<S: ManagedEntrySource> {
    source: Shared<S>,
    entry_index: BudgetedVec<usize>,
    resource: &'static mdschema::SchemaResource,
    root: Shared<RootInfo>,
    compat: u32,
    index: ManagedIndex,
    predefined: ManagedPredefined,
    settings: Charged<ConfigSettings>,
    sets: Charged<Vec<DefinedType>>,
}
#[derive(Debug)]
pub struct SelectedObject {
    pub object: Charged<MdObject>,
    pub diagnostics: ManagedDiagnostics,
}
type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;

impl<S: ManagedEntrySource> ManagedViewerContext<S> {
    pub fn build(source: Shared<S>) -> Result<Self, S::Error> {
        let budget = source.memory_budget().clone();
        if !source.reservation().budget().same_scope(&budget) {
            return Err(BudgetError::Overflow.into());
        }
        let entry_index = ManagedEntries::build_index(&*source)?;
        let entries = ManagedEntries::uncached_indexed(&*source, &entry_index)?;
        let (format, compat) = super::settings::read_configuration_version(&entries)?;
        let resource = mdschema::for_format(format)?;
        resource.require_embedded()?;
        let root = entries.root_info()?;
        let index = ManagedIndex::build(&entries)?;
        let predefined = ManagedPredefined::build(&entries, &index)?;
        let settings = ManagedSettings::read(&entries, &index)?;
        let count = index
            .order
            .iter()
            .filter(|id| {
                index
                    .objs
                    .get(id)
                    .is_some_and(|(kind, _)| *kind == MdKind::DefinedType)
            })
            .count();
        let sets =
            BudgetedVec::try_with_capacity(&budget, Category::Metadata, count)?.into_charged_vec();
        drop(entries);
        let mut context = Self {
            source,
            entry_index,
            resource,
            root,
            compat,
            index,
            predefined,
            settings,
            sets,
        };
        for ordinal in 0..context.index.order.len() {
            let id = *context
                .index
                .order
                .get(ordinal)
                .ok_or(BudgetError::Overflow)?;
            if !context
                .index
                .objs
                .get(&id)
                .is_some_and(|(kind, _)| *kind == MdKind::DefinedType)
            {
                continue;
            }
            let Some(selected) = context.read_object_prefix(id, context.sets.len())? else {
                continue;
            };
            let row = selected.object.map(|object| {
                let description = object
                    .props
                    .into_iter()
                    .find(|p| p.name == "Type")
                    .and_then(|p| match p.value {
                        Value::Type(value) => Some(value),
                        _ => None,
                    });
                DefinedType {
                    id: object.id,
                    name: object.name,
                    description,
                }
            });
            let released = row
                .reservation()
                .bytes()
                .checked_sub(row.retained_bytes()?)
                .ok_or(BudgetError::Overflow)?;
            row.reservation()
                .shrink(released)
                .map_err(BudgetError::from)?;
            context.sets = append(context.sets, row)?;
        }
        Ok(context)
    }
    pub fn index(&self) -> &ManagedIndex {
        &self.index
    }
    pub fn predefined(&self) -> &ManagedPredefined {
        &self.predefined
    }
    pub fn settings(&self) -> &ConfigSettings {
        &self.settings
    }
    pub fn compat(&self) -> u32 {
        self.compat
    }
    pub fn root(&self) -> &RootInfo {
        &self.root
    }
    pub fn resource(&self) -> &'static mdschema::SchemaResource {
        self.resource
    }
    pub fn entries(&self) -> Result<ManagedEntries<'_, S>, S::Error> {
        Ok(ManagedEntries::uncached_indexed(
            &*self.source,
            &self.entry_index,
        )?)
    }
    pub fn read_configuration(&self) -> Result<super::ManagedConfigurationProperties, S::Error> {
        let entries = self.entries()?;
        let name = GuidText::new(self.index.cfg_id);
        let name = name.as_str()?;
        let data = entries.get(name)?;
        if let Some(EntryData::Brace(doc)) = data.as_deref() {
            return Ok(ManagedValueContext::new(&self.index, &self.predefined)?
                .configuration_properties(&doc.root)?);
        }
        let budget = self.index.memory_budget();
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        diagnostics.miss(&[if entries.contains(name) {
            "configuration:not_brace"
        } else {
            "configuration:no_entry"
        }])?;
        Ok(super::ManagedConfigurationProperties {
            props: BudgetedVec::new(budget, Category::Metadata)?.into_charged_vec(),
            contained_objects: BudgetedVec::new(budget, Category::Metadata)?.into_charged_vec(),
            diagnostics,
        })
    }
    pub fn read_object(&self, id: Guid) -> Result<Option<SelectedObject>, S::Error> {
        let prefix = self
            .sets
            .iter()
            .position(|row| row.id == id)
            .unwrap_or(self.sets.len());
        self.read_object_prefix(id, prefix)
    }
    fn read_object_prefix(
        &self,
        id: Guid,
        prefix: usize,
    ) -> Result<Option<SelectedObject>, S::Error> {
        let Some(mut object) = selected_shell(&self.index, id)? else {
            return Ok(None);
        };
        let budget = self.source.memory_budget();
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        if !super::kind_spec(object.kind.as_str()).is_some_and(|spec| spec.header().is_some()) {
            return Ok(Some(SelectedObject {
                object,
                diagnostics,
            }));
        }
        let entries = self.entries()?;
        let context = ManagedValueContext::new(&self.index, &self.predefined)?;
        let sets = Prefix(self.sets.get(..prefix).ok_or(BudgetError::Overflow)?);
        let name = GuidText::new(id);
        if let Some(data) = entries.doc(name.as_str()?)?
            && let EntryData::Brace(doc) = &*data
        {
            let content = context.object_content_with_sets(
                object.kind,
                &object.name,
                &doc.root,
                Some(&sets),
            )?;
            diagnostics.merge(content.diagnostics)?;
            object = object
                .try_zip(content.values)?
                .map(|(mut object, content)| {
                    object.props = content.props;
                    object.synonym = content.synonym;
                    object.comment = content.comment;
                    object.generated_types = content.generated_types;
                    object.this_node = content.this_node;
                    object.children = content.children;
                    object
                });
        }
        for kind in [SubKind::Form, SubKind::Template] {
            let count = match kind {
                SubKind::Form => object.forms.len(),
                SubKind::Template => object.templates.len(),
            };
            for ordinal in 0..count {
                let sub = match kind {
                    SubKind::Form => object.forms.get(ordinal),
                    SubKind::Template => object.templates.get(ordinal),
                }
                .ok_or(BudgetError::Overflow)?;
                let id = sub.id;
                let name = GuidText::new(id);
                if let Some(data) = entries.doc(name.as_str()?)?
                    && let EntryData::Brace(doc) = &*data
                {
                    let properties = context.subobject_properties(kind, id, &doc.root)?;
                    diagnostics.merge(properties.diagnostics)?;
                    if !properties.values.synonym.0.is_empty()
                        || !properties.values.comment.is_empty()
                    {
                        return Err(BudgetError::Overflow.into());
                    }
                    object = object
                        .try_zip(properties.values)?
                        .map(|(mut object, properties)| {
                            let sub = match kind {
                                SubKind::Form => object.forms.get_mut(ordinal),
                                SubKind::Template => object.templates.get_mut(ordinal),
                            };
                            if let Some(sub) = sub {
                                sub.props = properties.props;
                            }
                            object
                        });
                }
            }
        }
        let ids = self
            .index
            .recalcs
            .get(&id)
            .map(|ids| ids.as_slice())
            .unwrap_or_default();
        let mut recalculations =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, ids.len())?
                .into_charged_vec();
        for id in ids {
            let name = GuidText::new(*id);
            if let Some(data) = entries.doc(name.as_str()?)?
                && let EntryData::Brace(doc) = &*data
                && let Some(decoded) =
                    context.recalculation((object.kind.as_str(), &object.name), *id, &doc.root)?
            {
                diagnostics.merge(decoded.diagnostics)?;
                recalculations = append(recalculations, decoded.values)?;
            }
        }
        object = object
            .try_zip(recalculations)?
            .map(|(mut object, recalculations)| {
                object.recalculations = recalculations;
                object
            });
        Ok(Some(SelectedObject {
            object,
            diagnostics,
        }))
    }
}
