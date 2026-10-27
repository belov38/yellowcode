use super::{
    ConfigurationIdentity, ManagedConfigurationError, ManagedDiagnostics, ManagedPredefinedReader,
    ManagedViewerContext, ManagedViewerFacts, ModuleFacts, ModuleSlot, ResidentObject,
    SelectedPart, ViewerCounters, index::GuidText,
};
use crate::{ChildTag, ManagedEntries, ManagedEntrySource, MdKind, ModuleKind};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Shared};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationKind {
    Module(ModuleSlot),
    Rights,
    Predefined,
    Configuration,
    Schedule,
    ExchangeContent,
    Style,
    BinaryParts,
    ConfigurationBinaries,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationReceipt {
    pub owner: Guid,
    pub kind: ValidationKind,
    pub returned_value: bool,
    pub semantic_miss: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleLocation {
    Configuration,
    Object,
    Command(usize),
    Recalculation(usize),
    Form(usize),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleDescriptor {
    pub object: Option<usize>,
    pub location: ModuleLocation,
    pub slot: ModuleSlot,
    pub facts: ModuleFacts,
}
pub struct ManagedViewerOpen<S: ManagedEntrySource> {
    pub(super) data_id: super::ViewerDataIdentity,
    identity: ConfigurationIdentity,
    stats: ViewerCounters,
    context: Shared<ManagedViewerContext<S>>,
    predefined: ManagedPredefinedReader<S>,
    objects: BudgetedVec<ResidentObject>,
    modules: BudgetedVec<ModuleDescriptor>,
    receipts: BudgetedVec<ValidationReceipt>,
    diagnostics: ManagedDiagnostics,
    unsupported: BudgetedVec<BudgetedText>,
}
type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;
impl<S: ManagedEntrySource> ManagedViewerOpen<S> {
    pub fn build(source: Shared<S>) -> Result<Self, S::Error> {
        let data_id = super::ViewerDataIdentity::allocate()?;
        let budget = source.memory_budget().clone();
        let context = ManagedViewerContext::build(source)?;
        let context = Shared::try_new(
            context,
            budget
                .try_reserve(Category::Metadata, 0)
                .map_err(BudgetError::from)?,
        )?;
        let ManagedViewerFacts {
            objects,
            mut diagnostics,
        } = ManagedViewerFacts::build(&context)?;
        let unsupported = super::objects::unsupported(context.index())?;
        let mut modules = BudgetedVec::new(&budget, Category::Metadata)?;
        let mut receipts = BudgetedVec::new(&budget, Category::Metadata)?;
        let entries = context.entries()?;
        for (ordinal, row) in objects.iter().enumerate() {
            let object = &row.object;
            for (suffix, file) in super::modules(object.kind.as_str()) {
                if let Some(kind) = module_kind(file) {
                    validate_module(
                        &entries,
                        ModuleSlot::separate(object.id, suffix, kind),
                        Some(ordinal),
                        ModuleLocation::Object,
                        &mut modules,
                        &mut receipts,
                        &mut diagnostics,
                    )?;
                }
            }
            for (child_index, child) in object
                .children
                .iter()
                .enumerate()
                .filter(|(_, c)| c.tag == ChildTag::Command)
            {
                validate_module(
                    &entries,
                    ModuleSlot::separate(child.id, "2", ModuleKind::CommandModule),
                    Some(ordinal),
                    ModuleLocation::Command(child_index),
                    &mut modules,
                    &mut receipts,
                    &mut diagnostics,
                )?;
            }
            for (recalc_index, recalc) in object.recalculations.iter().enumerate() {
                validate_module(
                    &entries,
                    ModuleSlot::separate(recalc.id, "0", ModuleKind::RecordSetModule),
                    Some(ordinal),
                    ModuleLocation::Recalculation(recalc_index),
                    &mut modules,
                    &mut receipts,
                    &mut diagnostics,
                )?;
            }
            if object.kind == MdKind::CommonForm {
                validate_module(
                    &entries,
                    ModuleSlot::form(object.id),
                    Some(ordinal),
                    ModuleLocation::Object,
                    &mut modules,
                    &mut receipts,
                    &mut diagnostics,
                )?;
            }
            for (form_index, form) in object.forms.iter().enumerate() {
                validate_module(
                    &entries,
                    ModuleSlot::form(form.id),
                    Some(ordinal),
                    ModuleLocation::Form(form_index),
                    &mut modules,
                    &mut receipts,
                    &mut diagnostics,
                )?;
            }
        }
        for (suffix, kind) in [
            ("0", ModuleKind::OrdinaryApplicationModule),
            ("5", ModuleKind::ExternalConnectionModule),
            ("6", ModuleKind::ManagedApplicationModule),
            ("7", ModuleKind::SessionModule),
        ] {
            validate_module(
                &entries,
                ModuleSlot::separate(context.index().header_id, suffix, kind),
                None,
                ModuleLocation::Configuration,
                &mut modules,
                &mut receipts,
                &mut diagnostics,
            )?;
        }
        drop(entries);
        for row in objects.iter().filter(|row| row.parent.is_none()) {
            if let Some(selected) = context.read_rights(row.object.id)? {
                record(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::Rights,
                    selected.rights.is_some(),
                    selected.diagnostics,
                )?;
            }
        }
        let predefined = ManagedPredefinedReader::build(context.clone())?;
        for row in objects.iter().filter(|row| row.parent.is_none()) {
            if let Some(selected) = predefined.read(row.object.id)? {
                record(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::Predefined,
                    selected.predefined.is_some(),
                    selected.diagnostics,
                )?;
            }
        }
        let configuration = context.read_configuration()?;
        let identity = ConfigurationIdentity::from_properties(&configuration.props)?;
        record(
            &mut receipts,
            &mut diagnostics,
            context.index().cfg_id,
            ValidationKind::Configuration,
            !configuration.props.is_empty(),
            configuration.diagnostics,
        )?;
        drop(configuration.props);
        drop(configuration.contained_objects);
        for row in objects.iter() {
            if let Some(selected) = context.read_schedule(row.object.id)? {
                record_part(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::Schedule,
                    selected,
                )?;
            }
        }
        for row in objects.iter() {
            if let Some(selected) = context.read_exchange_content(row.object.id)? {
                record_part(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::ExchangeContent,
                    selected,
                )?;
            }
        }
        for row in objects.iter() {
            if let Some(selected) = context.read_style(row.object.id)? {
                record_part(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::Style,
                    selected,
                )?;
            }
        }
        for row in objects.iter() {
            if let Some(selected) = context.read_binary_parts(row.object.id)? {
                record_part(
                    &mut receipts,
                    &mut diagnostics,
                    row.object.id,
                    ValidationKind::BinaryParts,
                    selected,
                )?;
            }
        }
        record_part(
            &mut receipts,
            &mut diagnostics,
            context.index().header_id,
            ValidationKind::ConfigurationBinaries,
            context.read_configuration_binaries()?,
        )?;
        let stats = ViewerCounters::build(&objects, &modules, &budget)?;
        Ok(Self {
            data_id,
            identity,
            stats,
            context,
            predefined,
            objects,
            modules,
            receipts,
            diagnostics,
            unsupported,
        })
    }
    pub fn identity(&self) -> &ConfigurationIdentity {
        &self.identity
    }
    pub fn stats(&self) -> &ViewerCounters {
        &self.stats
    }
    pub fn context(&self) -> &ManagedViewerContext<S> {
        &self.context
    }
    pub fn predefined(&self) -> &ManagedPredefinedReader<S> {
        &self.predefined
    }
    pub fn objects(&self) -> &[ResidentObject] {
        &self.objects
    }
    pub fn modules(&self) -> &[ModuleDescriptor] {
        &self.modules
    }
    pub fn receipts(&self) -> &[ValidationReceipt] {
        &self.receipts
    }
    pub fn diagnostics(&self) -> &ManagedDiagnostics {
        &self.diagnostics
    }
    pub fn unsupported(&self) -> impl Iterator<Item = &str> {
        self.unsupported.iter().map(BudgetedText::as_str)
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
fn validate_module<S: ManagedEntrySource>(
    entries: &ManagedEntries<'_, S>,
    slot: ModuleSlot,
    object: Option<usize>,
    location: ModuleLocation,
    modules: &mut BudgetedVec<ModuleDescriptor>,
    receipts: &mut BudgetedVec<ValidationReceipt>,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<(), S::Error> {
    let selected = slot.read(entries)?;
    if selected.lenient {
        diagnostics.miss(&["lenient:form-module ", GuidText::new(slot.owner).as_str()?])?;
    }
    if let Some(facts) = selected.facts() {
        modules.try_push(ModuleDescriptor {
            object,
            location,
            slot,
            facts,
        })?;
    }
    receipts.try_push(ValidationReceipt {
        owner: slot.owner,
        kind: ValidationKind::Module(slot),
        returned_value: selected.module.is_some(),
        semantic_miss: selected.lenient,
    })?;
    Ok(())
}
fn record(
    receipts: &mut BudgetedVec<ValidationReceipt>,
    diagnostics: &mut ManagedDiagnostics,
    owner: Guid,
    kind: ValidationKind,
    returned_value: bool,
    misses: ManagedDiagnostics,
) -> std::result::Result<(), BudgetError> {
    let semantic_miss = !misses.is_empty();
    diagnostics.merge(misses)?;
    receipts.try_push(ValidationReceipt {
        owner,
        kind,
        returned_value,
        semantic_miss,
    })?;
    Ok(())
}
fn record_part<T>(
    receipts: &mut BudgetedVec<ValidationReceipt>,
    diagnostics: &mut ManagedDiagnostics,
    owner: Guid,
    kind: ValidationKind,
    selected: SelectedPart<T>,
) -> std::result::Result<(), BudgetError> {
    record(
        receipts,
        diagnostics,
        owner,
        kind,
        selected.value.is_some(),
        selected.diagnostics,
    )
}
