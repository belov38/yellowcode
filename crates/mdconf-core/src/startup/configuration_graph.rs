use crate::startup::{
    ManagedIndex, ManagedObjectsError, ManagedPartsGraph, ManagedPredefined, ManagedValueContext,
    index::GuidText, values::empty,
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
#[derive(Debug)]
pub struct ManagedConfigurationGraph {
    pub(super) remaining_loaded: bool,
    pub(super) c6_loaded: bool,
    pub graph: crate::startup::ManagedModuleGraph,
    pub parts: Charged<crate::parts::ConfigParts>,
    pub props: Charged<Vec<crate::Prop>>,
}
impl ManagedPartsGraph {
    pub fn load_configuration<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predef: &ManagedPredefined,
        keep: bool,
    ) -> Result<ManagedConfigurationGraph, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if !self
            .graph
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(budget)
            || !self.parts.reservation().budget().same_scope(budget)
            || !index.memory_budget().same_scope(budget)
            || !predef.memory_budget().same_scope(budget)
            || !self.parts.contained_objects.is_empty()
        {
            return Err(BudgetError::Overflow.into());
        }
        ManagedConfigurationGraph {
            remaining_loaded: false,
            c6_loaded: false,
            graph: self.graph,
            parts: self.parts,
            props: BudgetedVec::new(budget, Category::Metadata)?.into_charged_vec(),
        }
        .install_configuration(entries, index, predef, keep)
    }
}
impl ManagedConfigurationGraph {
    pub(super) fn install_configuration<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predef: &ManagedPredefined,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        let name = GuidText::new(index.cfg_id);
        let name = name.as_str()?;
        let data = entries.get(name)?;
        if !keep {
            entries.forget(name);
        }
        let raw = match data.as_deref() {
            Some(EntryData::Brace(doc)) => &doc.root,
            _ => {
                self.graph.diagnostics.miss(&[if entries.contains(name) {
                    "configuration:not_brace"
                } else {
                    "configuration:no_entry"
                }])?;
                return Ok(self);
            }
        };
        let decoded = ManagedValueContext::new(index, predef)?.configuration_properties(raw)?;
        self.graph.diagnostics.merge(decoded.diagnostics)?;
        let blank = empty(budget, crate::parts::ConfigParts::default())?;
        let parts = std::mem::replace(&mut self.parts, blank);
        self.parts = parts
            .try_zip(decoded.contained_objects)?
            .map(|(mut parts, contained)| {
                parts.contained_objects = contained;
                parts
            });
        self.props = decoded.props;
        Ok(self)
    }
}
