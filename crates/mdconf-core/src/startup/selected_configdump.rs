use super::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedViewerContext, SelectedPart,
    remaining_configdump,
};
use crate::{EntryData, ManagedEntrySource, parts::ConfigDumpInfo};
use brace::Guid;

impl<S: ManagedEntrySource> ManagedViewerContext<S> {
    pub fn read_configdump_info(
        &self,
    ) -> Result<SelectedPart<ConfigDumpInfo>, ManagedConfigurationError<S::Error>> {
        let entries = self.entries()?;
        let mut diagnostics = ManagedDiagnostics::new(self.index().memory_budget())?;
        let data = entries.get("versions")?;
        let value = match data.as_deref() {
            Some(EntryData::Brace(doc)) => remaining_configdump::decode(
                self.index(),
                self.settings(),
                &doc.root,
                false,
                &mut diagnostics,
            )?,
            _ => {
                diagnostics.miss(&[if entries.contains("versions") {
                    "configdump:not_brace"
                } else {
                    "configdump:no_entry"
                }])?;
                None
            }
        };
        Ok(SelectedPart { value, diagnostics })
    }
}
impl<S: ManagedEntrySource> md_xmlout::ManagedDumpNames for ManagedViewerContext<S> {
    fn configuration(&self) -> (Guid, Guid, &str) {
        (
            self.index().cfg_id,
            self.index().header_id,
            &self.settings().name,
        )
    }
    fn name(&self, id: Guid) -> Option<&str> {
        self.index().names.get(&id).map(|name| name.as_str())
    }
    fn names(&self) -> impl Iterator<Item = (Guid, &str)> {
        self.index()
            .names
            .iter()
            .map(|(id, name)| (*id, name.as_str()))
    }
    fn owners(&self) -> &[Guid] {
        &self.index().order
    }
    fn recalculations(&self) -> impl Iterator<Item = Guid> {
        self.index()
            .recalcs
            .iter()
            .flat_map(|(_, ids)| ids.iter().copied())
    }
    fn is_object(&self, id: Guid) -> bool {
        self.index().objs.get(&id).is_some()
    }
    fn is_subobject(&self, id: Guid) -> bool {
        self.index().sub_hdr.get(&id).is_some()
    }
}
