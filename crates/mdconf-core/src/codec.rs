use crate::allocation::Owned;
use crate::entries::{EntrySource, ManagedEntries, ManagedEntrySource};
use crate::error::{Error, Result};
pub use crate::load::LoadCtx;
use crate::load::{LoadOptions, Stages};
use crate::model::Configuration;
use crate::startup::{ManagedDiagnostics, ManagedIndex, ManagedObjectsError};
use crate::{LoadProfile, RootInfo};
use brace::Guid;
use memory_budget::{BudgetError, Charged, Shared};

pub const CODEC_STAGES: Stages = Stages::FORMS
    .union(Stages::TEMPLATES)
    .union(Stages::FLOWCHARTS);

pub trait StageRunner {
    fn covers(&self) -> Stages;
    fn probe(&self) -> bool {
        false
    }
    fn forms(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()>;
    fn templates(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()>;
    fn flowcharts(&self, cx: &LoadCtx<'_>, cfg: &mut Configuration) -> Result<()>;
}

pub struct NoCodecs;

impl StageRunner for NoCodecs {
    fn covers(&self) -> Stages {
        Stages::NONE
    }
    fn probe(&self) -> bool {
        true
    }
    fn forms(&self, _: &LoadCtx<'_>, _: &mut Configuration) -> Result<()> {
        Ok(())
    }
    fn templates(&self, _: &LoadCtx<'_>, _: &mut Configuration) -> Result<()> {
        Ok(())
    }
    fn flowcharts(&self, _: &LoadCtx<'_>, _: &mut Configuration) -> Result<()> {
        Ok(())
    }
}

pub fn load_with(
    src: &dyn EntrySource,
    opts: &LoadOptions,
    runner: &dyn StageRunner,
) -> Result<Configuration> {
    crate::load::load_with(src, opts, runner)
}

pub trait ConfigurationCore {
    fn root_info(&self) -> Option<&RootInfo>;
    fn require_full(&self) -> Result<()>;
}

impl ConfigurationCore for Configuration {
    fn root_info(&self) -> Option<&RootInfo> {
        self.root_info.as_deref().and_then(crate::state::<RootInfo>)
    }

    fn require_full(&self) -> Result<()> {
        match self.profile {
            LoadProfile::Full => Ok(()),
            LoadProfile::Query => Err(Error::ProfileQuery),
        }
    }
}

pub type FlowchartXml<E> = std::result::Result<
    std::result::Result<Owned<Vec<u8>>, FlowchartError>,
    ManagedObjectsError<E>,
>;

#[derive(Debug)]
pub enum FlowchartError {
    Memory(BudgetError),
    Semantic,
}

pub trait ManagedCodecs {
    type FormState: Send + Sync + 'static;
    fn form_state<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
    ) -> std::result::Result<Shared<Self::FormState>, ManagedObjectsError<S::Error>>;
    fn form_state_with_retention<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        _keep: bool,
    ) -> std::result::Result<Shared<Self::FormState>, ManagedObjectsError<S::Error>> {
        self.form_state(entries, index)
    }
    fn form<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        id: Guid,
        shared: &Shared<Self::FormState>,
        keep: bool,
        diagnostics: &mut ManagedDiagnostics,
    ) -> std::result::Result<Option<Charged<crate::forms::Form>>, ManagedObjectsError<S::Error>>;
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
    >;
    fn flowchart<S: ManagedEntrySource + ?Sized>(
        &self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        id: Guid,
        data: &crate::EntryData,
        keep: bool,
    ) -> crate::codec::FlowchartXml<S::Error>;
}

pub mod base {
    pub use crate::state;
    pub mod allocation {
        pub use crate::allocation::*;
    }
    pub mod codec_base64 {
        pub use crate::codec_base64::*;
    }
    pub mod error {
        pub use crate::error::*;
    }
    pub mod index {
        pub use crate::index::*;
        pub use md_model::index::*;
    }
    pub mod load {
        pub use crate::load::*;
    }
    pub mod modules {
        pub use crate::modules::*;
    }
    pub mod objects {
        pub use crate::objects::*;
    }
    pub mod predefined {
        pub use crate::predefined::*;
    }
    pub mod startup {
        pub mod index {
            pub use crate::startup::index::*;
        }
        pub mod values {
            pub use crate::startup::values::*;
        }
    }
}
