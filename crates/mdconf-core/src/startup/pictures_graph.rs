use super::{ManagedObjectsError, managed_modules::ManagedModuleGraph};
use crate::parts::{ConfigParts, Picture};
use crate::startup::{
    ManagedDiagnostics, ManagedIndex, ManagedPicture,
    index::{GuidText, text},
    values::append,
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
#[derive(Debug)]
pub struct ManagedPartsGraph {
    pub graph: ManagedModuleGraph,
    pub parts: Charged<ConfigParts>,
}
pub(in crate::startup) fn picture<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    name: &str,
    which: &str,
    common: bool,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<Picture>>, ManagedObjectsError<S::Error>> {
    let data = entries.get(name)?;
    if !keep {
        entries.forget(name);
    }
    let Some(data) = data else {
        return Ok(None);
    };
    let EntryData::Brace(doc) = &*data else {
        diagnostics.miss(&["pictures:", which, ":not_brace"])?;
        return Ok(None);
    };
    if common && !(doc.bom && doc.lead.is_empty()) {
        return Ok(None);
    }
    let decoded = ManagedPicture::decode(entries.memory_budget(), &doc.root)?;
    if let Some(why) = decoded.miss {
        diagnostics.miss(&["pictures:", which, ":", why])?;
    }
    Ok(decoded.picture)
}
impl ManagedModuleGraph {
    pub fn load_pictures<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> Result<ManagedPartsGraph, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if !self
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(budget)
            || !index.memory_budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        let parts = crate::startup::values::empty(entries.memory_budget(), ConfigParts::default())?;
        self.install_pictures(entries, index, keep, parts)
    }
    pub(super) fn install_pictures<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
        parts: Charged<ConfigParts>,
    ) -> Result<ManagedPartsGraph, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if !self
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(budget)
            || !index.memory_budget().same_scope(budget)
            || !parts.reservation().budget().same_scope(budget)
            || !parts.pictures.is_empty()
        {
            return Err(BudgetError::Overflow.into());
        }
        for at in 0..self.objects.locations.len() {
            let object = self.objects.resolve(at).ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::CommonPicture {
                continue;
            }
            if object.parts.picture.is_some() {
                return Err(BudgetError::Overflow.into());
            }
            let id = GuidText::new(object.id);
            let which = id.as_str()?;
            let name = text(budget, &[which, ".0"])?;
            if let Some(picture) = picture(
                entries,
                name.as_str(),
                which,
                true,
                keep,
                &mut self.diagnostics,
            )? {
                self.objects = self.objects.install(
                    at,
                    picture,
                    |object| object.parts.picture.is_none(),
                    |object, picture| {
                        object.parts.picture = Some(picture);
                        None
                    },
                )?;
            }
        }
        let header = GuidText::new(index.header_id);
        let mut pictures = BudgetedVec::try_with_capacity(budget, CATEGORY, 3)?.into_charged_vec();
        for (suffix, which) in [("1", "Logo"), ("2", "Splash"), ("c", "MainSectionPicture")] {
            let name = text(budget, &[header.as_str()?, ".", suffix])?;
            if let Some(picture) = picture(
                entries,
                name.as_str(),
                which,
                false,
                keep,
                &mut self.diagnostics,
            )? {
                let named = text(budget, &[which])?.into_string()?.try_zip(picture)?;
                pictures = append(pictures, named)?;
            }
        }
        let parts = parts.try_zip(pictures)?.map(|(mut parts, pictures)| {
            parts.pictures = pictures;
            parts
        });
        Ok(ManagedPartsGraph { graph: self, parts })
    }
}
