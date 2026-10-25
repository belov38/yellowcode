use super::ManagedConfigurationGraph;
use crate::codec::FlowchartError;
use crate::graph::Flowchart;
use crate::startup::{
    ManagedIndex, ManagedObjectsError,
    index::{GuidText, text},
};
use crate::{ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::{BudgetError, BudgetedVec, Category};

impl ManagedConfigurationGraph {
    #[doc(hidden)]
    pub fn load_c6_with<S: ManagedEntrySource + ?Sized, C: crate::codec::ManagedCodecs>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
        codecs: &C,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if self.c6_loaded
            || !self
                .graph
                .objects
                .objects
                .reservation()
                .budget()
                .same_scope(budget)
            || !self.graph.modules.reservation().budget().same_scope(budget)
            || !self.graph.diagnostics.memory_budget().same_scope(budget)
            || !self.parts.reservation().budget().same_scope(budget)
            || !self.props.reservation().budget().same_scope(budget)
            || !index.owns_scope(budget)
            || self.graph.objects.all_objects().any(|object| {
                object.template.is_some()
                    || object.flowchart.is_some()
                    || object.templates.iter().any(|sub| sub.template.is_some())
            })
        {
            return Err(BudgetError::Overflow.into());
        }
        let mut pending = BudgetedVec::new(budget, Category::Metadata)?;
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind == MdKind::CommonTemplate
                && let Some(value) = codecs.template(
                    entries,
                    index,
                    object.id,
                    true,
                    keep,
                    &mut self.graph.diagnostics,
                )?
            {
                pending.try_push((at, None, value))?;
            }
            for (sub, template) in object.templates.iter().enumerate() {
                if let Some(value) = codecs.template(
                    entries,
                    index,
                    template.id,
                    false,
                    keep,
                    &mut self.graph.diagnostics,
                )? {
                    pending.try_push((at, Some(sub), value))?;
                }
            }
        }
        pending.reverse();
        while let Some((at, sub, value)) = pending.pop() {
            self.graph.objects = self.graph.objects.install_template(at, sub, value)?;
        }
        drop(pending);
        let mut pending = BudgetedVec::new(budget, Category::Metadata)?;
        let mut failed = 0u32;
        for at in 0..self.graph.objects.locations.len() {
            if self
                .graph
                .objects
                .locations
                .get(at)
                .is_none_or(|location| location.depth != 0)
            {
                continue;
            }
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::BusinessProcess {
                continue;
            }
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".7"])?;
            let data = match entries.get(name.as_str()) {
                Ok(Some(data)) => data,
                Ok(None) => continue,
                Err(error) if entries.recoverable_content_error(&error) => {
                    failed = failed.saturating_add(1);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let xml = codecs.flowchart(entries, index, object.id, &data, keep)?;
            if !keep {
                entries.forget(name.as_str());
            }
            match xml {
                Ok(xml) => pending.try_push((at, xml.map(|xml| Flowchart { xml }).managed()?))?,
                Err(FlowchartError::Memory(error)) => return Err(error.into()),
                Err(FlowchartError::Semantic) => failed = failed.saturating_add(1),
            }
        }
        if failed > 0 {
            self.graph
                .diagnostics
                .miss_count(&["flowchart:failed"], failed)?;
        }
        pending.reverse();
        while let Some((at, value)) = pending.pop() {
            self.graph.objects = self.graph.objects.install_flowchart(at, value)?;
        }
        self.c6_loaded = true;
        Ok(self)
    }
}
