use super::{ManagedObjectsError, managed_pictures::ManagedPartsGraph};
use crate::parts::Help;
use crate::startup::{
    ManagedDiagnostics, ManagedHelp, ManagedIndex,
    index::{GuidText, text},
    values::{append, empty},
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
const SUFFIXES: [&str; 20] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19",
];
pub(in crate::startup) fn help<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: Guid,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<Help>>, ManagedObjectsError<S::Error>> {
    let id = GuidText::new(id);
    for suffix in SUFFIXES {
        let name = text(entries.memory_budget(), &[id.as_str()?, ".", suffix])?;
        if !entries.contains(name.as_str()) {
            continue;
        }
        let data = match entries.get(name.as_str()) {
            Ok(Some(data)) => data,
            Ok(None) => continue,
            Err(error) if entries.recoverable_content_error(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if !keep {
            entries.forget(name.as_str());
        }
        match &*data {
            EntryData::Brace(doc)
                if doc.bom
                    && doc.lead.is_empty()
                    && doc.root.as_list().is_some_and(|list| list.len() >= 2)
                    && matches!(doc.root.at(0),Some(Node::Atom(marker)) if marker == "5") =>
            {
                let decoded = ManagedHelp::decode(entries.memory_budget(), &doc.root)?;
                if let Some(why) = decoded.miss {
                    diagnostics.miss(&["help:", why])?;
                }
                return Ok(decoded.help);
            }
            EntryData::Bytes(raw) if raw.starts_with(b"\xef\xbb\xbf{5,") => {
                diagnostics.miss(&["help:not_brace"])?;
                return Ok(None);
            }
            _ => {}
        }
    }
    Ok(None)
}
impl ManagedPartsGraph {
    pub fn load_help<S: ManagedEntrySource + ?Sized>(
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
            || !self.parts.reservation().budget().same_scope(budget)
            || !index.memory_budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        if self.parts.help.is_some() {
            return Err(BudgetError::Overflow.into());
        }
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            let (id, kind, count) = (object.id, object.kind, object.forms.len());
            if object.parts.help.is_some() || !object.parts.form_help.is_empty() {
                return Err(BudgetError::Overflow.into());
            }
            if kind != MdKind::CommonTemplate
                && kind.folder().is_some()
                && let Some(help) = help(entries, id, keep, &mut self.graph.diagnostics)?
            {
                self.graph.objects = self.graph.objects.install(
                    at,
                    help,
                    |object| object.parts.help.is_none(),
                    |object, help| {
                        object.parts.help = Some(help);
                        None
                    },
                )?;
            }
            let mut forms =
                BudgetedVec::try_with_capacity(budget, CATEGORY, count)?.into_charged_vec();
            for form_at in 0..count {
                let id = self
                    .graph
                    .objects
                    .resolve(at)
                    .and_then(|object| object.forms.get(form_at))
                    .ok_or(BudgetError::Overflow)?
                    .id;
                if let Some(help) = help(entries, id, keep, &mut self.graph.diagnostics)? {
                    forms = append(forms, help.map(|help| (id, help)))?;
                }
            }
            self.graph.objects = self.graph.objects.install(
                at,
                forms,
                |object| object.parts.form_help.is_empty(),
                |object, forms| {
                    object.parts.form_help = forms;
                    None
                },
            )?;
        }
        if let Some(help) = help(entries, index.header_id, keep, &mut self.graph.diagnostics)? {
            let blank = empty(budget, crate::parts::ConfigParts::default())?;
            let parts = std::mem::replace(&mut self.parts, blank);
            self.parts = parts.try_zip(help)?.map(|(mut parts, help)| {
                parts.help = Some(help);
                parts
            });
        }
        Ok(self)
    }
}
