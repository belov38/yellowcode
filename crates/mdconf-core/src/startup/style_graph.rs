use crate::startup::{
    ManagedConfigurationGraph, ManagedIndex, ManagedObjectsError, ManagedPredefined,
    ManagedValueContext,
    index::{GuidText, text},
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::BudgetError;
impl ManagedConfigurationGraph {
    pub fn load_styles<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predef: &ManagedPredefined,
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
            || !predef.memory_budget().same_scope(budget)
            || !self.parts.reservation().budget().same_scope(budget)
            || !self.props.reservation().budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        let context = ManagedValueContext::new(index, predef)?;
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::Style {
                continue;
            }
            if object.parts.style.is_some() {
                return Err(BudgetError::Overflow.into());
            }
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".0"])?;
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            match data.as_deref() {
                None => continue,
                Some(EntryData::Brace(_)) => {}
                Some(_) => {
                    self.graph.diagnostics.miss(&["misc:style:not_brace"])?;
                    continue;
                }
            }
            let decoded = context.style(data.as_ref().ok_or(BudgetError::Overflow)?)?;
            if let Some(why) = decoded.miss {
                self.graph.diagnostics.miss(&["misc:style:", why])?;
            }
            if let Some(style) = decoded.style {
                self.graph.objects = self.graph.objects.install(
                    at,
                    style,
                    |object| object.parts.style.is_none(),
                    |object, style| {
                        object.parts.style = Some(style);
                        None
                    },
                )?;
            }
        }
        Ok(self)
    }
}
