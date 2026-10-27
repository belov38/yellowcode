use super::{
    ManagedHttpError, RequestControl,
    check_cache::CheckCache,
    error, response,
    selected::{message, param, path_error},
};
use crate::transport::{Request, Response};
use mdconf::ManagedEntrySource;
use memory_budget::{BudgetError, Category, MemoryBudget, Reservation};
use std::path::{Path, PathBuf};
use yellowcode_core::viewer::{
    Lang,
    analysis_parent::AnalysisError,
    body::ViewerBody,
    module::{self, ManagedModuleError},
    names::module_label,
    snapshot::{
        ManagedNamesMetadataError, ManagedSnapshot, NamesControl, SelectedAnalysisError,
        SelectedFormError,
    },
};
#[derive(Clone, Copy)]
pub struct AnalysisPaths<'a> {
    pub executable: &'a Path,
    pub spill_dir: &'a Path,
}
pub(super) struct OwnedPaths {
    executable: PathBuf,
    spill_dir: PathBuf,
    _paths: Reservation,
}
impl OwnedPaths {
    pub(super) fn new(
        budget: &MemoryBudget,
        paths: AnalysisPaths<'_>,
    ) -> Result<Self, BudgetError> {
        if !paths.executable.is_absolute() || !paths.spill_dir.is_absolute() {
            return Err(BudgetError::Overflow);
        }
        let requested = paths
            .executable
            .as_os_str()
            .len()
            .checked_add(paths.spill_dir.as_os_str().len())
            .ok_or(BudgetError::Overflow)?;
        let reservation = budget.try_reserve(
            Category::Metadata,
            u64::try_from(requested).map_err(|_| BudgetError::Overflow)?,
        )?;
        let copy = |path: &Path| -> Result<PathBuf, BudgetError> {
            let mut owned = PathBuf::new();
            owned
                .try_reserve_exact(path.as_os_str().len())
                .map_err(|_| BudgetError::AllocationFailed)?;
            owned.push(path);
            Ok(owned)
        };
        let executable = copy(paths.executable)?;
        let spill_dir = copy(paths.spill_dir)?;
        let actual = executable
            .capacity()
            .checked_add(spill_dir.capacity())
            .ok_or(BudgetError::Overflow)?;
        if actual > requested {
            reservation
                .try_grow(u64::try_from(actual - requested).map_err(|_| BudgetError::Overflow)?)?;
        }
        Ok(Self {
            executable,
            spill_dir,
            _paths: reservation,
        })
    }
    pub(super) fn borrow(&self) -> AnalysisPaths<'_> {
        AnalysisPaths {
            executable: &self.executable,
            spill_dir: &self.spill_dir,
        }
    }
}
fn module_error(
    budget: &MemoryBudget,
    limit: u64,
    error: ManagedModuleError,
    suggestions: bool,
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    match error {
        ManagedModuleError::Memory(e) => Err(e.into()),
        ManagedModuleError::Path(e) if suggestions => path_error(budget, limit, &e),
        ManagedModuleError::InvalidInventory => message(budget, limit, 503, &error),
        _ => message(budget, limit, 404, &error),
    }
}
pub(super) fn handle<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
    paths: AnalysisPaths<'_>,
    cache: Option<&CheckCache>,
) -> Option<Result<Response<ViewerBody>, ManagedHttpError>>
where
    S::Error: std::fmt::Display,
{
    if !matches!(
        req.path.as_str(),
        "/api/module" | "/api/definition" | "/api/x/qb/literals" | "/api/check"
    ) {
        return None;
    }
    Some(run(snapshot, req, limit, control, paths, cache))
}
fn run<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
    paths: AnalysisPaths<'_>,
    cache: Option<&CheckCache>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: std::fmt::Display,
{
    let budget = snapshot.catalog().memory_budget();
    let interrupted = || control.is_canceled();
    let analysis = snapshot
        .analysis(paths.executable, paths.spill_dir, control.canceled)
        .with_control(control.deadline, &interrupted)
        .with_wire_limit(limit);
    let result = if req.path == "/api/definition" {
        let (Some(path), Some(which)) = (param(req, "path"), param(req, "module")) else {
            return Ok(error(400, b"{\"error\":\"path and module are required\"}"));
        };
        let number = |name: &str| -> Result<u32, Result<Response<ViewerBody>, ManagedHttpError>> {
            let Some(value) = param(req, name) else {
                return Err(message(
                    budget,
                    limit,
                    400,
                    &format_args!("{name} is required"),
                ));
            };
            value
                .parse()
                .map_err(|_| message(budget, limit, 400, &format_args!("{name}: not a number")))
        };
        let (line, col) = match (number("line"), number("col")) {
            (Ok(l), Ok(c)) => (l, c),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        match analysis.definition_body(path, which, line, col) {
            Ok(Some(body)) => Ok(body),
            Ok(None) => return Ok(error(404, b"{\"error\":\"no definition\"}")),
            Err(e) => Err(e),
        }
    } else {
        let Some(path) = param(req, "path") else {
            return Ok(error(400, b"{\"error\":\"path is required\"}"));
        };
        let which = param(req, "module");
        if matches!(req.path.as_str(), "/api/module" | "/api/check") {
            let keys = match module::modules_managed(snapshot.open(), &snapshot.names(), path) {
                Ok(keys) => keys,
                Err(e) => return module_error(budget, limit, e, true),
            };
            let first = keys
                .first()
                .and_then(|key| snapshot.open().modules().get(key.ordinal()))
                .map(|row| module_label(row.slot.kind, Lang::En));
            if req.path == "/api/check" {
                let key = match module::module_of_managed(
                    snapshot.open(),
                    &snapshot.names(),
                    path,
                    which.or(first),
                ) {
                    Ok(key) => key,
                    Err(e) => return module_error(budget, limit, e, false),
                };
                if let Some(cache) = cache
                    && let Some(body) = cache.lock().unwrap_or_else(|e| e.into_inner()).lookup(key)
                {
                    control.check()?;
                    return Ok(response(200, body));
                }
                let names_control =
                    NamesControl::new(control.canceled, control.deadline, &interrupted);
                let names =
                    snapshot.stage_check_names(paths.executable, paths.spill_dir, &names_control);
                control.check()?;
                let names = match names {
                    Ok(names) => names,
                    Err(ManagedNamesMetadataError::Memory(error)) => return Err(error.into()),
                    Err(ManagedNamesMetadataError::Canceled) => {
                        return Err(ManagedHttpError::Canceled);
                    }
                    Err(ManagedNamesMetadataError::FormProjection(SelectedFormError::Native(
                        AnalysisError::Memory(error),
                    ))) => return Err(error.into()),
                    Err(error) => {
                        return message(
                            budget,
                            limit,
                            503,
                            &format_args!("loading names failed: {error}"),
                        );
                    }
                };
                let result = analysis.check_staged_body(key, &names);
                control.check()?;
                match result {
                    Ok(body) => {
                        let body = if let Some(cache) = cache {
                            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                            control.check()?;
                            let body = cache.insert(key, body)?;
                            if let Err(error) = control.check() {
                                cache.clear();
                                return Err(error);
                            }
                            body
                        } else {
                            body
                        };
                        return Ok(response(200, body));
                    }
                    Err(error) => Err(error),
                }
            } else {
                analysis.module_body(path, which.or(first), param(req, "plain") == Some("1"))
            }
        } else {
            let key =
                match module::module_of_managed(snapshot.open(), &snapshot.names(), path, which) {
                    Ok(key) => key,
                    Err(e) => return module_error(budget, limit, e, false),
                };
            analysis.literals_body(key)
        }
    };
    control.check()?;
    match result {
        Ok(body) => Ok(response(200, body)),
        Err(SelectedAnalysisError::Module(error)) => {
            module_error(budget, limit, error, req.path == "/api/module")
        }
        Err(SelectedAnalysisError::Native(AnalysisError::Memory(error))) => Err(error.into()),
        Err(SelectedAnalysisError::Native(AnalysisError::Canceled | AnalysisError::Timeout)) => {
            Err(ManagedHttpError::Canceled)
        }
        Err(error) => message(
            budget,
            limit,
            503,
            &format_args!("loading analysis failed: {error}"),
        ),
    }
}
