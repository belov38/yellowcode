use super::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedViewerContext,
    index::{GuidText, text},
    remaining_rights,
};
use crate::{EntryData, ManagedEntrySource, MdKind, parts::Rights};
use brace::Guid;
use memory_budget::Charged;

#[derive(Debug)]
pub struct SelectedRights {
    pub rights: Option<Charged<Rights>>,
    pub diagnostics: ManagedDiagnostics,
}
impl<S: ManagedEntrySource> ManagedViewerContext<S> {
    pub fn read_rights(
        &self,
        id: Guid,
    ) -> Result<Option<SelectedRights>, ManagedConfigurationError<S::Error>> {
        if !self
            .index()
            .objs
            .get(&id)
            .is_some_and(|(kind, _)| *kind == MdKind::Role)
        {
            return Ok(None);
        }
        let budget = self.index().memory_budget();
        let entries = self.entries()?;
        let mut result = SelectedRights {
            rights: None,
            diagnostics: ManagedDiagnostics::new(budget)?,
        };
        let guid = GuidText::new(id);
        let name = text(budget, &[guid.as_str()?, ".0"])?;
        let data = entries.get(name.as_str())?;
        let node = match data.as_deref() {
            None => return Ok(Some(result)),
            Some(EntryData::Brace(doc)) => &doc.root,
            Some(_) => {
                result.diagnostics.miss(&["rights:not_brace"])?;
                return Ok(Some(result));
            }
        };
        match remaining_rights::decode(self.index(), self.settings(), node, false) {
            Ok(rights) => result.rights = Some(rights),
            Err(remaining_rights::Error::Memory(error)) => return Err(error.into()),
            Err(remaining_rights::Error::Semantic(why, detail)) => {
                result
                    .diagnostics
                    .miss(&["rights:", why, detail.unwrap_or("")])?;
            }
        }
        Ok(Some(result))
    }
}
