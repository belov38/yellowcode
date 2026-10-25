use crate::startup::{
    ManagedConfigurationGraph, ManagedDiagnostics, ManagedIndex, ManagedObjectsError,
    index::{GuidText, text},
    modules,
    values::{append, empty},
};
use crate::{EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
type Files = Vec<(String, Vec<u8>)>;
fn file(
    budget: &MemoryBudget,
    name: &str,
    bytes: Charged<Vec<u8>>,
) -> Result<Charged<(String, Vec<u8>)>, BudgetError> {
    text(budget, &[name])?.into_string()?.try_zip(bytes)
}
fn plain<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    name: &str,
    which: &str,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<Vec<u8>>>, ManagedObjectsError<S::Error>> {
    let data = entries.get(name)?;
    if !keep {
        entries.forget(name);
    }
    match data.as_deref() {
        None => Ok(None),
        Some(EntryData::Nested(_)) => {
            diagnostics.miss(&["misc:", which, ":container"])?;
            Ok(None)
        }
        Some(_) => Ok(entries.raw(name)?),
    }
}
fn plain_path(name: &str) -> bool {
    name.split('/').all(|part| {
        !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':', '\0'])
    })
}
fn ws<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    name: &str,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Charged<Files>, ManagedObjectsError<S::Error>> {
    let budget = entries.memory_budget();
    let mut output = BudgetedVec::new(budget, CATEGORY)?.into_charged_vec();
    let data = entries.get(name)?;
    if !keep {
        entries.forget(name);
    }
    let Some(EntryData::Nested(items)) = data.as_deref() else {
        return Ok(output);
    };
    output = BudgetedVec::try_with_capacity(budget, CATEGORY, items.len())?.into_charged_vec();
    let mut sorted = BudgetedVec::try_with_capacity(budget, CATEGORY, items.len())?;
    for (index, item) in items.iter().enumerate() {
        sorted.try_push((index, item))?;
    }
    sorted.sort_unstable_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));
    for (_, (name, bytes)) in sorted.iter() {
        let renamed = if name == "0.wsdl" {
            "WSDefinition.xml"
        } else if plain_path(name) {
            name.as_str()
        } else {
            diagnostics.miss(&["misc:ws:path"])?;
            continue;
        };
        let bom = name == "0.wsdl" && !bytes.starts_with(b"\xef\xbb\xbf");
        let capacity = bytes
            .len()
            .checked_add(if bom { 3 } else { 0 })
            .ok_or(BudgetError::Overflow)?;
        let mut copy = BudgetedVec::try_with_capacity(budget, CATEGORY, capacity)?;
        if bom {
            copy.try_extend_from_slice(b"\xef\xbb\xbf")?;
        }
        copy.try_extend_from_slice(bytes)?;
        output = append(output, file(budget, renamed, copy.into_charged_vec())?)?;
    }
    Ok(output)
}
fn binary_modules<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: brace::Guid,
    kind: MdKind,
    keep: bool,
) -> Result<Charged<Files>, ManagedObjectsError<S::Error>> {
    let budget = entries.memory_budget();
    let id = GuidText::new(id);
    let mut wanted = BudgetedVec::new(budget, CATEGORY)?;
    for (suffix, destination) in modules(kind.as_str()) {
        let name = text(budget, &[id.as_str()?, ".", suffix])?;
        let data = entries.get(name.as_str())?;
        if !keep {
            entries.forget(name.as_str());
        }
        let Some(EntryData::Nested(items)) = data.as_deref() else {
            continue;
        };
        if items.iter().any(|(name, _)| name == "text")
            || !items.iter().any(|(name, _)| name == "image")
        {
            continue;
        }
        wanted.try_push((name, destination))?;
    }
    wanted.sort_unstable_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then_with(|| a.1.cmp(b.1)));
    let mut output =
        BudgetedVec::try_with_capacity(budget, CATEGORY, wanted.len())?.into_charged_vec();
    for (name, destination) in wanted.iter() {
        if let Some(bytes) = entries.raw(name.as_str())? {
            let name = text(
                budget,
                &[
                    destination.strip_suffix(".bsl").unwrap_or(destination),
                    ".bin",
                ],
            )?;
            output = append(output, name.into_string()?.try_zip(bytes)?)?;
        }
    }
    Ok(output)
}
impl ManagedConfigurationGraph {
    pub fn load_binary_parts<S: ManagedEntrySource + ?Sized>(
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
            || !self.props.reservation().budget().same_scope(budget)
            || !index.memory_budget().same_scope(budget)
            || self.parts.binaries.capacity() != 0
        {
            return Err(BudgetError::Overflow.into());
        }
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.parts.binary_modules.capacity() != 0
                || object.parts.ws_definition.capacity() != 0
                || object.parts.xdto_package.is_some()
            {
                return Err(BudgetError::Overflow.into());
            }
            let (id, kind) = (object.id, object.kind);
            if kind == MdKind::WSReference || kind == MdKind::XDTOPackage {
                let id = GuidText::new(id);
                let name = text(budget, &[id.as_str()?, ".0"])?;
                if kind == MdKind::WSReference {
                    let files = ws(entries, name.as_str(), keep, &mut self.graph.diagnostics)?;
                    if !files.is_empty() {
                        self.graph.objects = self.graph.objects.install(
                            at,
                            files,
                            |object| object.parts.ws_definition.capacity() == 0,
                            |object, files| {
                                object.parts.ws_definition = files;
                                None
                            },
                        )?;
                    }
                } else if let Some(bytes) = plain(
                    entries,
                    name.as_str(),
                    "xdto",
                    keep,
                    &mut self.graph.diagnostics,
                )? {
                    self.graph.objects = self.graph.objects.install(
                        at,
                        bytes,
                        |object| object.parts.xdto_package.is_none(),
                        |object, bytes| {
                            object.parts.xdto_package = Some(bytes);
                            None
                        },
                    )?;
                }
            } else {
                let files = binary_modules(entries, id, kind, keep)?;
                if !files.is_empty() {
                    self.graph.objects = self.graph.objects.install(
                        at,
                        files,
                        |object| object.parts.binary_modules.capacity() == 0,
                        |object, files| {
                            object.parts.binary_modules = files;
                            None
                        },
                    )?;
                }
            }
        }
        let header = GuidText::new(index.header_id);
        let mut output = BudgetedVec::try_with_capacity(budget, CATEGORY, 2)?.into_charged_vec();
        for (suffix, destination) in [
            ("4", "ParentConfigurations.bin"),
            ("10", "MobileClientSignature.bin"),
        ] {
            let name = text(budget, &[header.as_str()?, ".", suffix])?;
            if let Some(bytes) = plain(
                entries,
                name.as_str(),
                "config_bin",
                keep,
                &mut self.graph.diagnostics,
            )? {
                output = append(output, file(budget, destination, bytes)?)?;
            }
        }
        if !output.is_empty() {
            let parts = std::mem::replace(
                &mut self.parts,
                empty(budget, crate::parts::ConfigParts::default())?,
            );
            self.parts = parts.try_zip(output)?.map(|(mut parts, binaries)| {
                parts.binaries = binaries;
                parts
            });
        }
        Ok(self)
    }
}
