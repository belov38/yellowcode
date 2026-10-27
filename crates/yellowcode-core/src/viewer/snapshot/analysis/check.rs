use super::{
    SelectedAnalysis, SelectedAnalysisError,
    definition::{Cursor, unsigned},
};
use crate::viewer::{Lang, analysis_parent::AnalysisError, body::ViewerBody, names::module_label};
use mdconf::{
    ManagedEntrySource, ModuleKind,
    startup::{ManagedModuleKey, ModuleLocation},
};
use memory_budget::{BudgetedText, BudgetedVec, Category};
use serde::Serialize;
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct ManagedCheckItem {
    pub key: ManagedModuleKey,
    pub name: BudgetedText,
    pub rel: BudgetedText,
}
#[derive(Serialize)]
struct Options<'a> {
    name: &'a str,
    rel: &'a str,
    names_dir: &'a Path,
    lang: &'static str,
    contexts: &'static str,
    own_contexts: bool,
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    pub fn check_staged_body(
        &self,
        key: ManagedModuleKey,
        names: &super::super::ManagedNamesDirectory,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.check()?;
        if names.data_identity() != self.snapshot.open().data_identity() {
            return Err(AnalysisError::Protocol.into());
        }
        self.check_body(key, names.path(), names.warnings())
    }
    pub fn check_item(
        &self,
        key: ManagedModuleKey,
    ) -> Result<ManagedCheckItem, SelectedAnalysisError<S::Error>> {
        self.check()?;
        let open = self.snapshot.open();
        let descriptor = open.module_descriptor(key)?;
        let budget = self.snapshot.catalog.memory_budget();
        let mut name =
            BudgetedText::new(budget, Category::Metadata, u64::MAX).map_err(AnalysisError::from)?;
        let mut rel =
            BudgetedText::new(budget, Category::Metadata, u64::MAX).map_err(AnalysisError::from)?;
        let kind = descriptor.slot.kind;
        if let Some(ordinal) = descriptor.object {
            let row = open.objects().get(ordinal).ok_or(AnalysisError::Protocol)?;
            let object = &row.object;
            name.try_push_str(self.snapshot.names().kind_name(object.kind, Lang::Ru))
                .map_err(AnalysisError::from)?;
            name.try_push_str(".").map_err(AnalysisError::from)?;
            name.try_push_str(&object.name)
                .map_err(AnalysisError::from)?;
            let mut chain =
                BudgetedVec::new(budget, Category::Metadata).map_err(AnalysisError::from)?;
            let mut at = Some(ordinal);
            while let Some(ordinal) = at {
                if chain.len() >= open.objects().len() {
                    return Err(AnalysisError::Protocol.into());
                }
                let row = open.objects().get(ordinal).ok_or(AnalysisError::Protocol)?;
                chain.try_push(ordinal).map_err(AnalysisError::from)?;
                at = row.parent;
            }
            for &ordinal in chain.iter().rev() {
                let object = &open
                    .objects()
                    .get(ordinal)
                    .ok_or(AnalysisError::Protocol)?
                    .object;
                if let Some(folder) = object.kind.folder() {
                    rel.try_push_str(folder).map_err(AnalysisError::from)?;
                    rel.try_push_str("/").map_err(AnalysisError::from)?;
                    rel.try_push_str(&object.name)
                        .map_err(AnalysisError::from)?;
                    rel.try_push_str("/").map_err(AnalysisError::from)?;
                }
            }
            match descriptor.location {
                ModuleLocation::Object => {
                    name.try_push_str(".").map_err(AnalysisError::from)?;
                    name.try_push_str(module_label(kind, Lang::Ru))
                        .map_err(AnalysisError::from)?;
                }
                ModuleLocation::Command(at) => {
                    let child = object.children.get(at).ok_or(AnalysisError::Protocol)?;
                    add_nested(
                        &mut name,
                        &mut rel,
                        "Команда",
                        "Commands",
                        &child.name,
                        "МодульКоманды",
                    )?;
                }
                ModuleLocation::Form(at) => {
                    let form = object.forms.get(at).ok_or(AnalysisError::Protocol)?;
                    add_nested(&mut name, &mut rel, "Форма", "Forms", &form.name, "Форма")?;
                }
                ModuleLocation::Recalculation(at) => {
                    let recalc = object
                        .recalculations
                        .get(at)
                        .ok_or(AnalysisError::Protocol)?;
                    add_nested(
                        &mut name,
                        &mut rel,
                        "Перерасчет",
                        "Recalculations",
                        &recalc.name,
                        "МодульНабораЗаписей",
                    )?;
                }
                ModuleLocation::Configuration => return Err(AnalysisError::Protocol.into()),
            }
        } else {
            if descriptor.location != ModuleLocation::Configuration {
                return Err(AnalysisError::Protocol.into());
            }
            name.try_push_str(module_label(kind, Lang::Ru))
                .map_err(AnalysisError::from)?;
        }
        rel.try_push_str("Ext/").map_err(AnalysisError::from)?;
        if kind == ModuleKind::FormModule {
            rel.try_push_str("Form/Module.bsl")
                .map_err(AnalysisError::from)?;
        } else {
            rel.try_push_str(module_label(kind, Lang::En))
                .map_err(AnalysisError::from)?;
            rel.try_push_str(".bsl").map_err(AnalysisError::from)?;
        }
        Ok(ManagedCheckItem { key, name, rel })
    }
    pub fn check_body(
        &self,
        key: ManagedModuleKey,
        names_dir: &Path,
        warnings: &[&str],
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        let deadline = Instant::now() + Duration::from_secs(120);
        let item = self.check_item(key)?;
        let response = self.run_before(
            key,
            "check",
            Options {
                name: item.name.as_str(),
                rel: item.rel.as_str(),
                names_dir,
                lang: if self.snapshot.lang == Lang::En {
                    "en"
                } else {
                    "ru"
                },
                contexts: "server",
                own_contexts: true,
            },
            Some(deadline),
        )?;
        let mut cursor = Cursor::new(response.as_ref(), || self.check_before(deadline));
        let [report, worker_warnings] = cursor.fields([b"report".as_slice(), b"warnings"])?;
        cursor.finish()?;
        let mut cursor = Cursor::new(report, || self.check_before(deadline));
        let [modules, compiles, skipped, diagnostics] =
            cursor.fields([b"modules".as_slice(), b"compiles", b"skipped", b"errors"])?;
        cursor.finish()?;
        for count in [modules, compiles, skipped] {
            unsigned(
                count,
                u64::try_from(usize::MAX).map_err(|_| AnalysisError::Protocol)?,
            )?;
        }
        let worker_warnings = worker_warnings
            .strip_prefix(b"[")
            .and_then(|s| s.strip_suffix(b"]"))
            .ok_or(AnalysisError::Protocol)?;
        if !diagnostics.starts_with(b"[") || !diagnostics.ends_with(b"]") {
            return Err(AnalysisError::Protocol.into());
        }
        let budget = self.snapshot.catalog.memory_budget();
        let mut out = BudgetedVec::new(budget, Category::Response).map_err(AnalysisError::from)?;
        out.try_extend_from_slice(b"{\"diagnostics\":")
            .map_err(AnalysisError::from)?;
        self.append(&mut out, diagnostics, deadline)?;
        out.try_extend_from_slice(b",\"warnings\":[")
            .map_err(AnalysisError::from)?;
        for (at, warning) in warnings.iter().enumerate() {
            self.check_before(deadline)?;
            if at != 0 {
                out.try_push(b',').map_err(AnalysisError::from)?;
            }
            let body = crate::viewer::body::json(budget, u64::MAX, warning)
                .map_err(AnalysisError::from)?;
            self.append(&mut out, body.as_ref(), deadline)?;
        }
        if !warnings.is_empty() && !worker_warnings.trim_ascii().is_empty() {
            out.try_push(b',').map_err(AnalysisError::from)?;
        }
        self.append(&mut out, worker_warnings, deadline)?;
        out.try_extend_from_slice(b"]}")
            .map_err(AnalysisError::from)?;
        self.check_before(deadline)?;
        Ok(ViewerBody::Owned(out))
    }
    fn append(
        &self,
        out: &mut BudgetedVec<u8>,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), AnalysisError> {
        for chunk in bytes.chunks(64 << 10) {
            self.check_before(deadline)?;
            out.try_extend_from_slice(chunk)?;
        }
        Ok(())
    }
}
fn add_nested(
    name: &mut BudgetedText,
    rel: &mut BudgetedText,
    ru: &str,
    folder: &str,
    child: &str,
    module: &str,
) -> Result<(), AnalysisError> {
    for text in [".", ru, ".", child, ".", module] {
        name.try_push_str(text)?;
    }
    for text in [folder, "/", child, "/"] {
        rel.try_push_str(text)?;
    }
    Ok(())
}
