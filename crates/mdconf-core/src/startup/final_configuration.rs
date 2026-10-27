use super::{ManagedConfigurationGraph, ManagedObjectIter, ManagedObjects, ManagedPartsGraph};
use crate::startup::{
    ManagedIndex, ManagedIndexError, ManagedObjectsError, ManagedPredefined, ManagedSettings,
    ManagedSettingsError,
    index::{GuidText, text},
    values::empty,
};
use crate::{ManagedEntries, ManagedEntriesError, ManagedEntrySource, RootInfo};
use brace::Guid;
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Shared,
};

#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigurationError<E> {
    #[error("{0}")]
    Entries(#[from] ManagedEntriesError<E>),
    #[error("{0}")]
    Index(#[from] ManagedIndexError<E>),
    #[error("{0}")]
    Settings(#[from] ManagedSettingsError<E>),
    #[error("{0}")]
    Objects(#[from] ManagedObjectsError<E>),
    #[error("{0}")]
    SchemaResource(#[from] mdschema::ResourceError),
    #[error("{0}")]
    Memory(#[from] BudgetError),
}

#[derive(Debug)]
pub struct ManagedConfiguration {
    resource: &'static mdschema::SchemaResource,
    root: Shared<RootInfo>,
    compat: u32,
    index: ManagedIndex,
    predefined: ManagedPredefined,
    settings: Charged<crate::ConfigSettings>,
    graph: ManagedConfigurationGraph,
    unsupported: BudgetedVec<BudgetedText>,
}

#[derive(Debug, Clone, Copy)]
pub struct GeneratedTypeView<'a> {
    pub object: &'a crate::MdObject,
    pub child: Option<&'a crate::Child>,
    pub category: &'static str,
    pub recalculation: Option<&'a crate::Recalculation>,
    pub name: &'a str,
}

impl ManagedConfiguration {
    pub fn owns_scope(&self, budget: &MemoryBudget) -> bool {
        self.index.owns_scope(budget)
    }

    #[doc(hidden)]
    pub fn load_with<S: ManagedEntrySource + ?Sized, C: crate::codec::ManagedCodecs>(
        source: &S,
        budget: &MemoryBudget,
        keep_source: bool,
        codecs: &C,
    ) -> Result<Self, ManagedConfigurationError<S::Error>> {
        if !source.memory_budget().same_scope(budget) {
            return Err(BudgetError::Overflow.into());
        }
        let entries = ManagedEntries::new(source)?;
        let (format, compat) = crate::startup::settings::read_configuration_version(&entries)?;
        let resource = mdschema::for_format(format)?;
        resource.require_embedded()?;
        let root = entries.root_info()?;
        if !root.reservation().budget().same_scope(budget) {
            return Err(BudgetError::Overflow.into());
        }
        let index = ManagedIndex::build(&entries)?;
        if root.id() != index.cfg_id || !index.owns_scope(budget) {
            return Err(BudgetError::Overflow.into());
        }
        let predefined = ManagedPredefined::build(&entries, &index)?;
        let settings = ManagedSettings::read(&entries, &index)?;
        let unsupported = unsupported(&index)?;
        let objects = ManagedObjects::shells(&index)?
            .decode(&entries, &index, &predefined, keep_source)?
            .load_modules(&entries, &index, keep_source)?
            .load_forms_with(&entries, &index, keep_source, codecs)?;
        let graph = ManagedConfigurationGraph {
            remaining_loaded: false,
            c6_loaded: false,
            graph: objects,
            parts: empty(budget, crate::parts::ConfigParts::default())?,
            props: BudgetedVec::new(budget, Category::Metadata)?.into_charged_vec(),
        }
        .install_rights(&entries, &index, &settings, keep_source)?
        .install_predefined(&entries, &index, &predefined, keep_source)?
        .install_command_interfaces(&entries, &index, keep_source)?
        .install_configuration(&entries, &index, &predefined, keep_source)?
        .install_configdump(&entries, &index, &settings, keep_source)?
        .load_schedules(&entries, &index, keep_source)?
        .load_exchange_content(&entries, &index, keep_source)?
        .load_styles(&entries, &index, &predefined, keep_source)?
        .load_binary_parts(&entries, &index, keep_source)?;
        let ManagedConfigurationGraph {
            graph,
            parts,
            props,
            ..
        } = graph;
        let ManagedPartsGraph { graph, parts } = graph
            .install_pictures(&entries, &index, keep_source, parts)?
            .load_help(&entries, &index, keep_source)?;
        let graph = ManagedConfigurationGraph {
            remaining_loaded: true,
            c6_loaded: false,
            graph,
            parts,
            props,
        }
        .load_c6_with(&entries, &index, keep_source, codecs)?;
        drop(entries);
        Ok(Self {
            resource,
            root,
            compat,
            index,
            predefined,
            settings,
            graph,
            unsupported,
        })
    }
    pub fn resource(&self) -> &'static mdschema::SchemaResource {
        self.resource
    }
    pub fn id(&self) -> Guid {
        self.root.id()
    }
    pub fn header_id(&self) -> Guid {
        self.index.header_id
    }
    pub fn compat(&self) -> u32 {
        self.compat
    }
    pub fn root_info(&self) -> &RootInfo {
        &self.root
    }
    #[doc(hidden)]
    pub fn managed_index(&self) -> &ManagedIndex {
        &self.index
    }
    #[doc(hidden)]
    pub fn managed_predefined(&self) -> &ManagedPredefined {
        &self.predefined
    }
    pub fn settings(&self) -> &crate::ConfigSettings {
        &self.settings
    }
    pub fn props(&self) -> &[crate::Prop] {
        &self.graph.props
    }
    pub fn parts(&self) -> &crate::parts::ConfigParts {
        &self.graph.parts
    }
    pub fn modules(&self) -> &[crate::Module] {
        &self.graph.graph.modules
    }
    pub fn objects(&self) -> &[crate::MdObject] {
        self.graph.graph.objects.as_slice()
    }
    pub fn all_objects(&self) -> ManagedObjectIter<'_> {
        self.graph.graph.objects.all_objects()
    }
    pub fn object(&self, id: Guid) -> Option<&crate::MdObject> {
        self.graph.graph.objects.object(id)
    }
    pub fn find(&self, kind: crate::MdKind, name: &str) -> Option<&crate::MdObject> {
        self.graph.graph.objects.find(kind, name)
    }
    pub fn of_kind(&self, kind: crate::MdKind) -> impl Iterator<Item = &crate::MdObject> {
        self.graph.graph.objects.of_kind(kind)
    }
    pub fn full_name(&self, id: Guid) -> Option<&str> {
        self.index.names.get(&id).map(BudgetedText::as_str)
    }
    pub fn type_name(&self, id: Guid) -> Option<&str> {
        self.index.types.get(&id).map(BudgetedText::as_str)
    }
    pub fn predefined_name(&self, id: Guid) -> Option<&str> {
        self.predefined.get(&id)
    }
    pub fn misses(&self) -> impl Iterator<Item = (&str, u32)> {
        self.graph.graph.diagnostics.iter()
    }
    pub fn unsupported(&self) -> impl Iterator<Item = &str> {
        self.unsupported.iter().map(BudgetedText::as_str)
    }
    pub fn generated_type(&self, id: Guid) -> Option<GeneratedTypeView<'_>> {
        let owner = self.index.type_owners.get(&id)?;
        let object = self.object(owner.object)?;
        Some(GeneratedTypeView {
            object,
            child: owner.child.and_then(|id| find_child(&object.children, id)),
            category: owner.category,
            recalculation: owner
                .recalc
                .and_then(|id| object.recalculations.iter().find(|r| r.id == id)),
            name: self.type_name(id)?,
        })
    }
}

fn find_child(children: &[crate::Child], id: Guid) -> Option<&crate::Child> {
    for child in children {
        if child.id == id {
            return Some(child);
        }
        if let Some(found) = find_child(&child.children, id) {
            return Some(found);
        }
    }
    None
}

pub(in crate::startup) fn unsupported(
    index: &ManagedIndex,
) -> Result<BudgetedVec<BudgetedText>, BudgetError> {
    let budget = index.memory_budget();
    let mut missing_schema = BudgetedVec::new(budget, Category::Metadata)?;
    for (id, (kind, name)) in index.objs.iter() {
        if name.is_none() {
            missing_schema.try_push((GuidText::new(*id), *kind))?;
        }
    }
    for (id, _) in missing_schema.iter() {
        id.as_str()?;
    }
    missing_schema
        .sort_unstable_by(|(a, _), (b, _)| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    let mut result = BudgetedVec::new(budget, Category::Metadata)?;
    for (id, kind) in missing_schema.iter() {
        result.try_push(text(
            budget,
            &[kind.as_str(), " ", id.as_str()?, ": no schema"],
        )?)?;
    }
    for id in index.missing.iter() {
        result.try_push(text(
            budget,
            &[GuidText::new(*id).as_str()?, ": missing entry"],
        )?)?;
    }
    for id in index.too_deep.iter() {
        result.try_push(text(
            budget,
            &[GuidText::new(*id).as_str()?, ": nesting too deep"],
        )?)?;
    }
    for skipped in index.skipped.iter() {
        result.try_push(text(budget, &[skipped.as_str()])?)?;
    }
    Ok(result)
}
