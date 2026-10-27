use super::{
    GuidMap, ManagedConfigurationError, ManagedDiagnostics, ManagedPredefined,
    ManagedViewerContext,
    index::{GuidText, text},
    remaining_predefined,
};
use crate::{EntryData, ManagedEntrySource, parts::Predefined};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedText, Charged, Shared};

type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;

pub struct ManagedPredefinedReader<S: ManagedEntrySource> {
    context: Shared<ManagedViewerContext<S>>,
    full: GuidMap<BudgetedText>,
}
#[derive(Debug)]
pub struct SelectedPredefined {
    pub predefined: Option<Charged<Predefined>>,
    pub diagnostics: ManagedDiagnostics,
}
impl<S: ManagedEntrySource> ManagedPredefinedReader<S> {
    pub fn build(context: Shared<ManagedViewerContext<S>>) -> Result<Self, S::Error> {
        let budget = context.index().memory_budget();
        if !context.reservation().budget().same_scope(budget) {
            return Err(BudgetError::Overflow.into());
        }
        let mut full = GuidMap::new(budget)?;
        for id in context.index().order.iter() {
            let Some((kind, Some(owner))) = context.index().objs.get(id) else {
                continue;
            };
            let Some(suffix) = crate::predefined::table_suffix(*kind) else {
                continue;
            };
            let entries = context.entries()?;
            let guid = GuidText::new(*id);
            let name = text(budget, &[guid.as_str()?, ".", suffix])?;
            let data = entries.get(name.as_str())?;
            let Some(EntryData::Brace(doc)) = data.as_deref() else {
                continue;
            };
            let mut local = ManagedPredefined::new(budget)?;
            local.index_table(*kind, &doc.root)?;
            for (id, item) in local.iter() {
                full.insert(
                    *id,
                    text(budget, &[kind.as_str(), ".", owner.as_str(), ".", item])?,
                )?;
            }
        }
        Ok(Self { context, full })
    }
    pub fn context(&self) -> &ManagedViewerContext<S> {
        &self.context
    }
    pub fn read(&self, id: Guid) -> Result<Option<SelectedPredefined>, S::Error> {
        let context = &self.context;
        if !context.index().order.contains(&id) {
            return Ok(None);
        }
        let Some((kind, Some(_))) = context.index().objs.get(&id) else {
            return Ok(None);
        };
        let Some(suffix) = crate::predefined::table_suffix(*kind) else {
            return Ok(None);
        };
        let budget = context.index().memory_budget();
        let mut result = SelectedPredefined {
            predefined: None,
            diagnostics: ManagedDiagnostics::new(budget)?,
        };
        let entries = context.entries()?;
        let guid = GuidText::new(id);
        let name = text(budget, &[guid.as_str()?, ".", suffix])?;
        let data = entries.get(name.as_str())?;
        let node = match data.as_deref() {
            None => return Ok(Some(result)),
            Some(EntryData::Brace(doc)) => &doc.root,
            Some(_) => {
                result.diagnostics.miss(&["predef:not_brace"])?;
                return Ok(Some(result));
            }
        };
        match remaining_predefined::decode(
            context.index(),
            context.predefined(),
            &self.full,
            node,
            false,
        ) {
            Ok(value) => result.predefined = Some(value),
            Err(remaining_predefined::Error::Memory(error)) => return Err(error.into()),
            Err(remaining_predefined::Error::Semantic(why, detail)) => {
                result
                    .diagnostics
                    .miss(&["predef:", why, detail.unwrap_or("")])?;
            }
        }
        Ok(Some(result))
    }
}
