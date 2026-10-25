use crate::startup::{
    ManagedConfigurationGraph, ManagedExchangeContent, ManagedIndex, ManagedObjectsError,
    index::{GuidText, text},
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::BudgetError;
impl ManagedConfigurationGraph {
    pub fn load_exchange_content<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if !self
            .graph
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(budget)
            || !index.memory_budget().same_scope(budget)
            || !self.parts.reservation().budget().same_scope(budget)
            || !self.props.reservation().budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::ExchangePlan {
                continue;
            }
            if object.parts.exchange_content.is_some() {
                return Err(BudgetError::Overflow.into());
            }
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".1"])?;
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            let raw = match data.as_deref() {
                None => continue,
                Some(EntryData::Brace(doc)) => &doc.root,
                Some(_) => {
                    self.graph.diagnostics.miss(&["misc:content:not_brace"])?;
                    continue;
                }
            };
            let decoded = ManagedExchangeContent::decode(index, raw)?;
            if let Some(why) = decoded.miss {
                self.graph.diagnostics.miss(&["misc:content:", why])?;
            }
            if let Some(items) = decoded.items {
                self.graph.objects = self.graph.objects.install(
                    at,
                    items,
                    |object| object.parts.exchange_content.is_none(),
                    |object, items| {
                        object.parts.exchange_content = Some(items);
                        None
                    },
                )?;
            }
        }
        Ok(self)
    }
}
