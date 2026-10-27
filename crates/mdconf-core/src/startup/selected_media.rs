use super::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedViewerContext, SelectedPart,
    index::{GuidText, text},
    objects::{managed_help::help, managed_pictures::picture},
};
use crate::{
    ManagedEntrySource, MdKind, SubKind,
    parts::{Help, Picture},
};
use brace::Guid;

type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;
impl<S: ManagedEntrySource> ManagedViewerContext<S> {
    pub fn read_object_help(&self, id: Guid) -> Result<Option<SelectedPart<Help>>, S::Error> {
        if !self.index().objs.get(&id).is_some_and(|(kind, name)| {
            *kind != MdKind::CommonTemplate && kind.folder().is_some() && name.is_some()
        }) {
            return Ok(None);
        }
        self.read_help_at(id).map(Some)
    }
    pub fn read_form_help(
        &self,
        owner: Guid,
        form: Guid,
    ) -> Result<Option<SelectedPart<Help>>, S::Error> {
        if !self.index().subs.get(&owner).is_some_and(|forms| {
            forms
                .iter()
                .any(|(kind, id)| *kind == SubKind::Form && *id == form)
        }) {
            return Ok(None);
        }
        self.read_help_at(form).map(Some)
    }
    pub fn read_configuration_help(&self) -> Result<SelectedPart<Help>, S::Error> {
        self.read_help_at(self.index().header_id)
    }
    fn read_help_at(&self, id: Guid) -> Result<SelectedPart<Help>, S::Error> {
        let entries = self.entries()?;
        let mut diagnostics = ManagedDiagnostics::new(self.index().memory_budget())?;
        let value = help(&entries, id, false, &mut diagnostics)?;
        Ok(SelectedPart { value, diagnostics })
    }

    pub fn read_common_picture(&self, id: Guid) -> Result<Option<SelectedPart<Picture>>, S::Error> {
        if !self
            .index()
            .objs
            .get(&id)
            .is_some_and(|(kind, name)| *kind == MdKind::CommonPicture && name.is_some())
        {
            return Ok(None);
        }
        self.read_picture_at(id, "0", None).map(Some)
    }
    pub fn read_configuration_picture(
        &self,
        ordinal: usize,
    ) -> Result<Option<SelectedPart<Picture>>, S::Error> {
        let (suffix, label) = match ordinal {
            0 => ("1", "Logo"),
            1 => ("2", "Splash"),
            2 => ("c", "MainSectionPicture"),
            _ => return Ok(None),
        };
        self.read_picture_at(self.index().header_id, suffix, Some(label))
            .map(Some)
    }
    fn read_picture_at(
        &self,
        id: Guid,
        suffix: &str,
        label: Option<&str>,
    ) -> Result<SelectedPart<Picture>, S::Error> {
        let budget = self.index().memory_budget();
        let entries = self.entries()?;
        let id = GuidText::new(id);
        let guid = id.as_str()?;
        let name = text(budget, &[guid, ".", suffix])?;
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        let value = picture(
            &entries,
            name.as_str(),
            label.unwrap_or(guid),
            label.is_none(),
            false,
            &mut diagnostics,
        )?;
        Ok(SelectedPart { value, diagnostics })
    }
}
