mod check;
mod definition;
mod forms;
mod pictures;
mod preview;
mod preview_body;
mod roles;
use super::ManagedSnapshot;
use crate::viewer::{
    Lang,
    analysis_parent::{AnalysisError, Supervisor},
    body::ViewerBody,
    module::{self, ManagedModuleError},
    names::module_label,
};
pub use check::ManagedCheckItem;
pub use forms::SelectedFormError;
use mdconf::{
    ManagedEntrySource,
    startup::{ManagedModuleKey, ManagedSelectionError, ManagedViewerOpen, ViewerDataIdentity},
};
pub use preview_body::{GeneratedKind, PreviewFormat, PreviewOptions, PreviewProjection};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, thiserror::Error)]
pub enum SelectedAnalysisError<E> {
    #[error("{0}")]
    Selection(#[from] ManagedSelectionError<E>),
    #[error("{0}")]
    Module(#[from] ManagedModuleError),
    #[error("{0}")]
    Native(#[from] AnalysisError),
}
pub struct SelectedAnalysis<'a, S: ManagedEntrySource> {
    snapshot: &'a ManagedSnapshot<S>,
    parent: Supervisor<'a>,
    canceled: &'a AtomicBool,
}
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn analysis<'a>(
        &'a self,
        executable: &'a Path,
        spill_dir: &'a Path,
        canceled: &'a AtomicBool,
    ) -> SelectedAnalysis<'a, S> {
        SelectedAnalysis {
            snapshot: self,
            parent: Supervisor::new(
                executable,
                spill_dir,
                self.catalog.memory_budget(),
                canceled,
            ),
            canceled,
        }
    }
}
#[derive(Serialize)]
struct Metadata<'a, T> {
    op: &'static str,
    platform: &'a str,
    snapshot: ViewerDataIdentity,
    module_ordinal: usize,
    compat: u32,
    kind: &'static str,
    #[serde(flatten)]
    options: T,
}
#[derive(Serialize)]
struct Empty {}
#[derive(Serialize)]
#[serde(bound(serialize = ""))]
struct ModuleOptions<'a, S: ManagedEntrySource> {
    lang: &'static str,
    plain: bool,
    modules: Labels<'a, S>,
}
struct Labels<'a, S: ManagedEntrySource> {
    open: &'a ManagedViewerOpen<S>,
    keys: &'a [ManagedModuleKey],
}
impl<S: ManagedEntrySource> Serialize for Labels<'_, S> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let mut seq = s.serialize_seq(Some(self.keys.len()))?;
        for key in self.keys {
            let row = self.open.module_descriptor(*key).map_err(|_| {
                serde::ser::Error::custom("validated module inventory is inconsistent")
            })?;
            seq.serialize_element(module_label(row.slot.kind, Lang::En))?;
        }
        seq.end()
    }
}
impl<'a, S: ManagedEntrySource> SelectedAnalysis<'a, S> {
    pub fn with_wire_limit(mut self, limit: u64) -> Self {
        self.parent = self.parent.with_json_limit(limit);
        self
    }
    pub fn with_control(
        mut self,
        deadline: Option<std::time::Instant>,
        interrupted: &'a dyn Fn() -> bool,
    ) -> Self {
        self.parent = self.parent.with_control(deadline, interrupted);
        self
    }
    fn check(&self) -> Result<(), SelectedAnalysisError<S::Error>> {
        if self.canceled.load(Ordering::Acquire) {
            return Err(AnalysisError::Canceled.into());
        }
        self.parent
            .check(std::time::Instant::now() + std::time::Duration::from_secs(120))?;
        Ok(())
    }
    fn run<T: Serialize>(
        &self,
        key: ManagedModuleKey,
        op: &'static str,
        options: T,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.run_before(key, op, options, None)
    }
    fn run_before<T: Serialize>(
        &self,
        key: ManagedModuleKey,
        op: &'static str,
        options: T,
        deadline: Option<std::time::Instant>,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.check()?;
        let open = self.snapshot.open();
        let row = open.module_descriptor(key)?;
        let selected = open.read_selected_module(key)?;
        let module = selected
            .module
            .as_ref()
            .ok_or(ManagedSelectionError::MissingValidated)?;
        self.check()?;
        let metadata = Metadata {
            op,
            platform: self.snapshot.catalog.platform(),
            snapshot: open.data_identity(),
            module_ordinal: key.ordinal(),
            compat: open.context().settings().compatibility_mode.0,
            kind: module_label(row.slot.kind, Lang::En),
            options,
        };
        Ok(match deadline {
            Some(deadline) => self
                .parent
                .run_before(&metadata, &module.source, deadline)?,
            None => self.parent.run(&metadata, &module.source)?,
        })
    }
    pub fn literals_body(
        &self,
        key: ManagedModuleKey,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.run(key, "literals", Empty {})
    }
    pub fn module_body(
        &self,
        path: &str,
        which: Option<&str>,
        plain: bool,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.check()?;
        let open = self.snapshot.open();
        let names = self.snapshot.names();
        let key = module::module_of_managed(open, &names, path, which)?;
        let keys = module::modules_managed(open, &names, path)?;
        let options = ModuleOptions {
            lang: if self.snapshot.lang == Lang::En {
                "en"
            } else {
                "ru"
            },
            plain,
            modules: Labels {
                open,
                keys: keys.as_slice(),
            },
        };
        self.run(key, "module_view", options)
    }
}
