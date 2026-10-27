use super::{ManagedDecodedObjects, ManagedObjects, ManagedObjectsError, push, vector};
use crate::startup::{
    ManagedDiagnostics, ManagedIndex,
    index::{GuidText, text},
    modules,
};
use crate::{ChildTag, EntryData, ManagedEntries, ManagedEntrySource, MdKind, Module, ModuleKind};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget, Shared};
const CATEGORY: Category = Category::Metadata;
const BOM: &[u8] = b"\xef\xbb\xbf";
#[derive(Debug)]
pub struct ManagedModuleGraph {
    pub objects: ManagedObjects,
    pub diagnostics: ManagedDiagnostics,
    pub modules: Charged<Vec<Module>>,
}
#[derive(Debug)]
pub struct ManagedFormModule {
    pub module: Option<Charged<Module>>,
    pub lenient: bool,
}
impl ManagedFormModule {
    pub fn decode(budget: &MemoryBudget, data: &EntryData) -> Result<Self, BudgetError> {
        let (module, lenient) = match data {
            EntryData::Brace(doc) if doc.bom && doc.lead.is_empty() => match doc
                .root
                .at(2)
                .and_then(brace::Node::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(source) => (Some(form_source(budget, source.as_bytes())?), false),
                None => (None, false),
            },
            EntryData::Bytes(raw) => match quoted_module(raw) {
                Some((raw, length)) if length != 0 => {
                    let capacity = length.checked_add(BOM.len()).ok_or(BudgetError::Overflow)?;
                    let mut source = BudgetedVec::try_with_capacity(budget, CATEGORY, capacity)?;
                    source.try_extend_from_slice(BOM)?;
                    let mut at = 0usize;
                    while let Some(byte) = raw.get(at).copied() {
                        source.try_push(byte)?;
                        at = at
                            .checked_add(if byte == b'"' { 2 } else { 1 })
                            .ok_or(BudgetError::Overflow)?;
                    }
                    (
                        Some(source.into_charged_vec().map(|source| Module {
                            kind: ModuleKind::FormModule,
                            source,
                        })),
                        true,
                    )
                }
                _ => (None, false),
            },
            _ => (None, false),
        };
        Ok(Self { module, lenient })
    }
}
fn form_source(budget: &MemoryBudget, bytes: &[u8]) -> Result<Charged<Module>, BudgetError> {
    let length = bytes
        .len()
        .checked_add(BOM.len())
        .ok_or(BudgetError::Overflow)?;
    let mut source = BudgetedVec::try_with_capacity(budget, CATEGORY, length)?;
    source.try_extend_from_slice(BOM)?;
    source.try_extend_from_slice(bytes)?;
    Ok(source.into_charged_vec().map(|source| Module {
        kind: ModuleKind::FormModule,
        source,
    }))
}
fn quoted_module(raw: &[u8]) -> Option<(&[u8], usize)> {
    let body = raw.strip_prefix(BOM)?;
    if body.first() != Some(&b'{') {
        return None;
    }
    let (mut depth, mut in_string, mut commas, mut at) = (0usize, false, 0usize, 0usize);
    loop {
        match (in_string, *body.get(at)?) {
            (true, b'"') => in_string = false,
            (true, _) => {}
            (false, b'"') => in_string = true,
            (false, b'{') => depth = depth.checked_add(1)?,
            (false, b'}') => depth = depth.checked_sub(1)?,
            (false, b',') if depth == 1 => {
                commas = commas.checked_add(1)?;
                if commas == 2 {
                    break;
                }
            }
            _ => {}
        }
        at = at.checked_add(1)?;
    }
    at = at.checked_add(1)?;
    while body.get(at).is_some_and(u8::is_ascii_whitespace) {
        at = at.checked_add(1)?;
    }
    if body.get(at) != Some(&b'"') {
        return None;
    }
    at = at.checked_add(1)?;
    let start = at;
    let mut length = 0usize;
    loop {
        match *body.get(at)? {
            b'"' if body.get(at.checked_add(1)?) == Some(&b'"') => at = at.checked_add(2)?,
            b'"' => {
                let span = body.get(start..at)?;
                std::str::from_utf8(span).ok()?;
                return Some((span, length));
            }
            _ => at = at.checked_add(1)?,
        }
        length = length.checked_add(1)?;
    }
}
fn module_kind(file: &str) -> Option<ModuleKind> {
    Some(match file {
        "Module.bsl" => ModuleKind::Module,
        "ObjectModule.bsl" => ModuleKind::ObjectModule,
        "ManagerModule.bsl" => ModuleKind::ManagerModule,
        "RecordSetModule.bsl" => ModuleKind::RecordSetModule,
        "ValueManagerModule.bsl" => ModuleKind::ValueManagerModule,
        "CommandModule.bsl" => ModuleKind::CommandModule,
        _ => return None,
    })
}
fn entry<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: Guid,
    suffix: &str,
    keep: bool,
) -> Result<Option<Shared<EntryData>>, ManagedObjectsError<S::Error>> {
    let guid = GuidText::new(id);
    let name = text(entries.memory_budget(), &[guid.as_str()?, ".", suffix])?;
    let data = entries.get(name.as_str())?;
    if !keep {
        entries.forget(name.as_str());
    }
    Ok(data)
}
fn container<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: Guid,
    suffix: &str,
    kind: ModuleKind,
    keep: bool,
) -> Result<Option<Charged<Module>>, ManagedObjectsError<S::Error>> {
    let Some(data) = entry(entries, id, suffix, keep)? else {
        return Ok(None);
    };
    Ok(crate::startup::ModuleRead::separate(entries.memory_budget(), &data, kind)?.module)
}
fn form<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: Guid,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<Module>>, ManagedObjectsError<S::Error>> {
    let Some(data) = entry(entries, id, "0", keep)? else {
        return Ok(None);
    };
    let decoded = ManagedFormModule::decode(entries.memory_budget(), &data)?;
    if decoded.lenient {
        let guid = GuidText::new(id);
        diagnostics.miss(&["lenient:form-module ", guid.as_str()?])?;
    }
    Ok(decoded.module)
}
impl ManagedDecodedObjects {
    pub fn load_modules<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> Result<ManagedModuleGraph, ManagedObjectsError<S::Error>> {
        if !self
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(entries.memory_budget())
            || !index.memory_budget().same_scope(entries.memory_budget())
        {
            return Err(BudgetError::Overflow.into());
        }
        let budget = entries.memory_budget();
        for at in 0..self.objects.locations.len() {
            let object = self.objects.resolve(at).ok_or(BudgetError::Overflow)?;
            let (id, kind) = (object.id, object.kind);
            let capacity = modules(kind.as_str())
                .count()
                .checked_add(usize::from(kind == MdKind::CommonForm))
                .ok_or(BudgetError::Overflow)?;
            let mut output = vector(budget, capacity)?;
            for (suffix, file) in modules(kind.as_str()) {
                if let Some(kind) = module_kind(file)
                    && let Some(module) = container(entries, id, suffix, kind, keep)?
                {
                    output = push(output, module)?;
                }
            }
            let count = self
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?
                .children
                .len();
            for child_at in 0..count {
                let child = self
                    .objects
                    .resolve(at)
                    .and_then(|object| object.children.get(child_at))
                    .ok_or(BudgetError::Overflow)?;
                if child.tag != ChildTag::Command {
                    continue;
                }
                if let Some(module) =
                    container(entries, child.id, "2", ModuleKind::CommandModule, keep)?
                {
                    self.objects = self.objects.install(
                        at,
                        module,
                        |object| {
                            object
                                .children
                                .get(child_at)
                                .is_some_and(|child| child.module.is_none())
                        },
                        |object, module| {
                            let Some(child) = object.children.get_mut(child_at) else {
                                return Some(module);
                            };
                            child.module = Some(module);
                            None
                        },
                    )?;
                }
            }
            let count = self
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?
                .recalculations
                .len();
            for recalc_at in 0..count {
                let id = self
                    .objects
                    .resolve(at)
                    .and_then(|object| object.recalculations.get(recalc_at))
                    .ok_or(BudgetError::Overflow)?
                    .id;
                if let Some(module) =
                    container(entries, id, "0", ModuleKind::RecordSetModule, keep)?
                {
                    self.objects = self.objects.install(
                        at,
                        module,
                        |object| {
                            object
                                .recalculations
                                .get(recalc_at)
                                .is_some_and(|recalc| recalc.module.is_none())
                        },
                        |object, module| {
                            let Some(recalc) = object.recalculations.get_mut(recalc_at) else {
                                return Some(module);
                            };
                            recalc.module = Some(module);
                            None
                        },
                    )?;
                }
            }
            if kind == MdKind::CommonForm
                && let Some(module) = form(entries, id, keep, &mut self.diagnostics)?
            {
                output = push(output, module)?;
            }
            self.objects = self.objects.install(
                at,
                output,
                |object| object.modules.capacity() == 0,
                |object, output| {
                    object.modules = output;
                    None
                },
            )?;
            let count = self
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?
                .forms
                .len();
            for form_at in 0..count {
                let id = self
                    .objects
                    .resolve(at)
                    .and_then(|object| object.forms.get(form_at))
                    .ok_or(BudgetError::Overflow)?
                    .id;
                if let Some(module) = form(entries, id, keep, &mut self.diagnostics)? {
                    self.objects = self.objects.install(
                        at,
                        module,
                        |object| {
                            object
                                .forms
                                .get(form_at)
                                .is_some_and(|form| form.module.is_none())
                        },
                        |object, module| {
                            let Some(form) = object.forms.get_mut(form_at) else {
                                return Some(module);
                            };
                            form.module = Some(module);
                            None
                        },
                    )?;
                }
            }
        }
        let mut output = vector(budget, 4)?;
        for (suffix, kind) in [
            ("0", ModuleKind::OrdinaryApplicationModule),
            ("5", ModuleKind::ExternalConnectionModule),
            ("6", ModuleKind::ManagedApplicationModule),
            ("7", ModuleKind::SessionModule),
        ] {
            if let Some(module) = container(entries, index.header_id, suffix, kind, keep)? {
                output = push(output, module)?;
            }
        }
        Ok(ManagedModuleGraph {
            objects: self.objects,
            diagnostics: self.diagnostics,
            modules: output,
        })
    }
}
