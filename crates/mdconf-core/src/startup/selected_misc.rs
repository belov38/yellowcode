use super::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedExchangeContent, ManagedSchedule,
    ManagedValueContext, ManagedViewerContext,
    index::{GuidText, text},
    values::{append, empty},
};
use crate::{
    EntryData, ManagedEntrySource, MdKind,
    parts::{ContentItem, ObjectParts, Schedule, Style},
};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};

type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;
type Files = Vec<(String, Vec<u8>)>;
#[derive(Debug)]
pub struct SelectedPart<T> {
    pub value: Option<Charged<T>>,
    pub diagnostics: ManagedDiagnostics,
}
impl<S: ManagedEntrySource> ManagedViewerContext<S> {
    fn part<T>(&self) -> std::result::Result<SelectedPart<T>, BudgetError> {
        Ok(SelectedPart {
            value: None,
            diagnostics: ManagedDiagnostics::new(self.index().memory_budget())?,
        })
    }
    fn has_kind(&self, id: Guid, kind: MdKind) -> bool {
        self.index()
            .objs
            .get(&id)
            .is_some_and(|(actual, name)| *actual == kind && name.is_some())
    }
    pub fn read_schedule(&self, id: Guid) -> Result<Option<SelectedPart<Schedule>>, S::Error> {
        if !self.has_kind(id, MdKind::ScheduledJob) {
            return Ok(None);
        }
        let mut result = self.part()?;
        let entries = self.entries()?;
        let guid = GuidText::new(id);
        let name = text(self.index().memory_budget(), &[guid.as_str()?, ".0"])?;
        if let Some(data) = entries.get(name.as_str())? {
            let decoded = ManagedSchedule::decode(self.index().memory_budget(), &data)?;
            result.value = decoded.schedule;
            if let Some(why) = decoded.miss {
                result.diagnostics.miss(&["misc:sched:", why])?;
            }
        }
        Ok(Some(result))
    }
    pub fn read_exchange_content(
        &self,
        id: Guid,
    ) -> Result<Option<SelectedPart<Vec<ContentItem>>>, S::Error> {
        if !self.has_kind(id, MdKind::ExchangePlan) {
            return Ok(None);
        }
        let mut result = self.part()?;
        let entries = self.entries()?;
        let guid = GuidText::new(id);
        let name = text(self.index().memory_budget(), &[guid.as_str()?, ".1"])?;
        let data = entries.get(name.as_str())?;
        match data.as_deref() {
            None => {}
            Some(EntryData::Brace(doc)) => {
                let decoded = ManagedExchangeContent::decode(self.index(), &doc.root)?;
                result.value = decoded.items;
                if let Some(why) = decoded.miss {
                    result.diagnostics.miss(&["misc:content:", why])?;
                }
            }
            Some(_) => result.diagnostics.miss(&["misc:content:not_brace"])?,
        }
        Ok(Some(result))
    }
    pub fn read_style(&self, id: Guid) -> Result<Option<SelectedPart<Style>>, S::Error> {
        if !self.has_kind(id, MdKind::Style) {
            return Ok(None);
        }
        let mut result = self.part()?;
        let entries = self.entries()?;
        let guid = GuidText::new(id);
        let name = text(self.index().memory_budget(), &[guid.as_str()?, ".0"])?;
        if let Some(data) = entries.get(name.as_str())? {
            let context = ManagedValueContext::new(self.index(), self.predefined())?;
            let decoded = context.style(&data)?;
            result.value = decoded.style;
            if let Some(why) = decoded.miss {
                result.diagnostics.miss(&["misc:style:", why])?;
            }
        }
        Ok(Some(result))
    }
    pub fn read_binary_parts(
        &self,
        id: Guid,
    ) -> Result<Option<SelectedPart<ObjectParts>>, S::Error> {
        let Some((kind, Some(_))) = self.index().objs.get(&id) else {
            return Ok(None);
        };
        let budget = self.index().memory_budget();
        let mut result = self.part()?;
        let entries = self.entries()?;
        let guid = GuidText::new(id);
        let parts = empty(budget, ObjectParts::default())?;
        let parts = match kind {
            MdKind::WSReference => {
                let name = text(budget, &[guid.as_str()?, ".0"])?;
                let files = super::objects::managed_binary_parts::ws(
                    &entries,
                    name.as_str(),
                    false,
                    &mut result.diagnostics,
                )?;
                parts.try_zip(files)?.map(|(mut parts, files)| {
                    parts.ws_definition = files;
                    parts
                })
            }
            MdKind::XDTOPackage => {
                let name = text(budget, &[guid.as_str()?, ".0"])?;
                if let Some(bytes) = super::objects::managed_binary_parts::plain(
                    &entries,
                    name.as_str(),
                    "xdto",
                    false,
                    &mut result.diagnostics,
                )? {
                    parts.try_zip(bytes)?.map(|(mut parts, bytes)| {
                        parts.xdto_package = Some(bytes);
                        parts
                    })
                } else {
                    parts
                }
            }
            _ => {
                let files = super::objects::managed_binary_parts::binary_modules(
                    &entries, id, *kind, false,
                )?;
                parts.try_zip(files)?.map(|(mut parts, files)| {
                    parts.binary_modules = files;
                    parts
                })
            }
        };
        result.value = Some(parts);
        Ok(Some(result))
    }
    pub fn read_configuration_binaries(&self) -> Result<SelectedPart<Files>, S::Error> {
        let budget = self.index().memory_budget();
        let mut result = self.part()?;
        let entries = self.entries()?;
        let guid = GuidText::new(self.index().header_id);
        let mut files =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, 2)?.into_charged_vec();
        for (suffix, destination) in [
            ("4", "ParentConfigurations.bin"),
            ("10", "MobileClientSignature.bin"),
        ] {
            let name = text(budget, &[guid.as_str()?, ".", suffix])?;
            if let Some(bytes) = super::objects::managed_binary_parts::plain(
                &entries,
                name.as_str(),
                "config_bin",
                false,
                &mut result.diagnostics,
            )? {
                files = append(
                    files,
                    text(budget, &[destination])?
                        .into_string()?
                        .try_zip(bytes)?,
                )?;
            }
        }
        result.value = Some(files);
        Ok(result)
    }
}
