use brace::Guid;
use md_forms::startup::forms_shared;
use mdconf_core::codec::{ConfigurationCore, LoadCtx, ManagedCodecs, StageRunner};
use mdconf_core::entries::{CfFile, EntrySink, EntrySource, ManagedEntries, ManagedEntrySource};
use mdconf_core::startup::{
    ManagedConfiguration, ManagedConfigurationError, ManagedConfigurationGraph, ManagedDiagnostics,
    ManagedIndex, ManagedModuleGraph, ManagedObjectsError,
};
use mdconf_core::{
    Configuration, ConfigurationInspection, EntryData, LoadOptions, Result, RootInfo, Stages,
};
use memory_budget::{Charged, MemoryBudget, Shared};

pub(crate) struct FullRunner;

impl StageRunner for FullRunner {
    fn covers(&self) -> Stages {
        mdconf_core::codec::CODEC_STAGES
    }
    fn forms(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()> {
        md_forms::forms::load(cx, cfg)
    }
    fn templates(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()> {
        md_graphics::templates::load(cx, cfg)
    }
    fn flowcharts(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()> {
        md_graphics::graph::load(cx, cfg)
    }
}

pub(crate) struct FullCodecs;

impl ManagedCodecs for FullCodecs {
    type FormState = md_forms::forms::shared::Shared;

    fn form_state<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
    ) -> std::result::Result<Shared<Self::FormState>, ManagedObjectsError<S::Error>> {
        self.form_state_with_retention(entries, index, true)
    }

    fn form_state_with_retention<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> std::result::Result<Shared<Self::FormState>, ManagedObjectsError<S::Error>> {
        forms_shared::build_with_retention(entries, index, keep).map_err(|error| match error {
            forms_shared::ManagedFormSharedError::Entries(error) => {
                ManagedObjectsError::Entries(error)
            }
            forms_shared::ManagedFormSharedError::Memory(error) => {
                ManagedObjectsError::Memory(error)
            }
        })
    }

    fn form<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        id: Guid,
        shared: &Shared<Self::FormState>,
        keep: bool,
        diagnostics: &mut ManagedDiagnostics,
    ) -> std::result::Result<Option<Charged<crate::forms::Form>>, ManagedObjectsError<S::Error>>
    {
        md_forms::startup::forms_codec::form(entries, id, shared, keep, diagnostics)
    }

    fn template<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        id: Guid,
        common: bool,
        keep: bool,
        diagnostics: &mut ManagedDiagnostics,
    ) -> std::result::Result<
        Option<Charged<crate::templates::Template>>,
        ManagedObjectsError<S::Error>,
    > {
        md_graphics::startup::c6_codec::one_template(entries, index, id, common, keep, diagnostics)
    }

    fn flowchart<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        id: Guid,
        data: &EntryData,
        keep: bool,
    ) -> mdconf_core::codec::FlowchartXml<S::Error> {
        md_graphics::startup::c6_codec::flowchart(entries, index, id, data, keep)
    }
}

pub trait ConfigurationExt: Sized {
    fn load(src: &dyn EntrySource, opts: &LoadOptions) -> Result<Self>;

    fn load_xml(dir: &std::path::Path, options: &crate::xml_in::PartsOptions) -> Result<Self>;

    fn root_info(&self) -> Option<&RootInfo>;

    fn require_full(&self) -> Result<()>;
}

impl ConfigurationExt for Configuration {
    fn load_xml(dir: &std::path::Path, options: &crate::xml_in::PartsOptions) -> Result<Self> {
        crate::xml_in::load_xml(dir, options)
    }

    fn load(src: &dyn EntrySource, opts: &LoadOptions) -> Result<Configuration> {
        mdconf_core::codec::load_with(src, opts, &FullRunner)
    }

    fn root_info(&self) -> Option<&RootInfo> {
        ConfigurationCore::root_info(self)
    }

    fn require_full(&self) -> Result<()> {
        ConfigurationCore::require_full(self)
    }
}

pub trait CfFileExt {
    fn inspect_configuration(&self, opts: &LoadOptions) -> Result<ConfigurationInspection>;
}

impl CfFileExt for CfFile {
    fn inspect_configuration(&self, opts: &LoadOptions) -> Result<ConfigurationInspection> {
        self.inspect_with(opts, &FullRunner)
    }
}

pub fn check(src: &dyn EntrySource) -> Result<mdconf_core::build::CheckReport> {
    mdconf_core::build::check_with(src, &FullRunner)
}

pub fn build_edit(
    model: &Configuration,
    base: &dyn EntrySource,
    sink: &mut dyn EntrySink,
    opts: &mdconf_core::build::BuildOptions,
) -> Result<mdconf_core::build::BuildReport> {
    mdconf_core::build::build_edit_with(model, base, sink, opts, &FullRunner)
}

pub trait FormsGraph: Sized {
    fn load_forms<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> std::result::Result<Self, ManagedObjectsError<S::Error>>;
}

impl FormsGraph for ManagedModuleGraph {
    fn load_forms<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> std::result::Result<Self, ManagedObjectsError<S::Error>> {
        self.load_forms_with(entries, index, keep, &FullCodecs)
    }
}

pub trait C6Graph: Sized {
    fn load_c6<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> std::result::Result<Self, ManagedObjectsError<S::Error>>;
}

impl C6Graph for ManagedConfigurationGraph {
    fn load_c6<S: ManagedEntrySource + ?Sized>(
        self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
    ) -> std::result::Result<Self, ManagedObjectsError<S::Error>> {
        self.load_c6_with(entries, index, keep, &FullCodecs)
    }
}

pub trait ManagedConfigurationLoad: Sized {
    fn load<S: ManagedEntrySource + ?Sized>(
        source: &S,
        budget: &MemoryBudget,
        keep_source: bool,
    ) -> std::result::Result<Self, ManagedConfigurationError<S::Error>>;
}

impl ManagedConfigurationLoad for ManagedConfiguration {
    fn load<S: ManagedEntrySource + ?Sized>(
        source: &S,
        budget: &MemoryBudget,
        keep_source: bool,
    ) -> std::result::Result<Self, ManagedConfigurationError<S::Error>> {
        ManagedConfiguration::load_with(source, budget, keep_source, &FullCodecs)
    }
}
