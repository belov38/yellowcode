use super::{
    AnalysisPaths, ManagedHttpError, RequestControl, error, response,
    selected::{message, param, path_error},
};
use crate::transport::{Request, Response};
use mdconf::{ManagedEntrySource, startup::ManagedConfigurationError};
use memory_budget::{BudgetedWriter, MemoryBudget};
use yellowcode_core::viewer::{
    analysis_parent::{AnalysisError, PictureLease},
    body::ViewerBody,
    path::{self, ManagedResolveError},
    snapshot::{
        GeneratedKind, ManagedSnapshot, PreviewFormat, PreviewOptions, PreviewProjection,
        SelectedFormError,
    },
};
const FORM_CSP: &str =
    "default-src 'none'; img-src 'self'; font-src 'self'; style-src 'unsafe-inline'";
const SVG_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";
const IMMUTABLE: &str = "max-age=31536000, immutable";

fn failure<E: std::fmt::Display>(
    budget: &MemoryBudget,
    limit: u64,
    error: SelectedFormError<E>,
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    match error {
        SelectedFormError::Path(ManagedResolveError::Memory(e))
        | SelectedFormError::Model(ManagedConfigurationError::Memory(e))
        | SelectedFormError::Native(AnalysisError::Memory(e)) => Err(e.into()),
        SelectedFormError::Path(ManagedResolveError::Path(e)) => path_error(budget, limit, &e),
        SelectedFormError::Native(AnalysisError::Canceled) => Err(ManagedHttpError::Canceled),
        SelectedFormError::Native(AnalysisError::Timeout) => Err(ManagedHttpError::Deadline),
        error => message(
            budget,
            limit,
            503,
            &format_args!("loading form failed: {error}"),
        ),
    }
}
fn binary(body: ViewerBody, content_type: &'static str) -> Response<ViewerBody> {
    Response {
        content_type,
        cache_control: Some(IMMUTABLE),
        ..response(200, body)
    }
}
fn picture(
    budget: &MemoryBudget,
    limit: u64,
    control: &RequestControl<'_>,
    lease: Option<PictureLease>,
    missing: &'static [u8],
) -> Result<Response<ViewerBody>, ManagedHttpError> {
    let image = match lease.as_ref().map(PictureLease::view).transpose() {
        Ok(image) => image.flatten(),
        Err(error) => return failure::<std::convert::Infallible>(budget, limit, error.into()),
    };
    let Some(image) = image else {
        return Ok(error(404, missing));
    };
    let mut output = BudgetedWriter::new(budget, limit)?;
    for piece in image.bytes.chunks(64 << 10) {
        control.check()?;
        output.try_write(piece)?;
        control.check()?;
    }
    control.check()?;
    let mime = image.mime.as_str();
    Ok(Response {
        csp: (mime == "image/svg+xml").then_some(SVG_CSP),
        cache_control: Some("no-cache"),
        ..binary(ViewerBody::Owned(output.into_buffer()), mime)
    })
}
pub(super) fn handle<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
    paths: Option<AnalysisPaths<'_>>,
) -> Option<Result<Response<ViewerBody>, ManagedHttpError>>
where
    S::Error: std::fmt::Display,
{
    if req.path == "/api/form-structure" {
        return Some(structure(snapshot, req, limit, control));
    }
    if let Some(file) = req.path.strip_prefix("/res/font/") {
        return Some(Ok(
            match ui_style::FONT_FILES.iter().find(|(n, _)| *n == file) {
                Some((_, bytes)) => binary(ViewerBody::Static(bytes), "font/ttf"),
                None => error(404, b"{\"error\":\"no such font\"}"),
            },
        ));
    }
    if let Some(id) = req.path.strip_prefix("/res/pic/") {
        return Some(Ok(match super::static_pictures::lookup(id) {
            Some((mime, bytes)) => Response {
                csp: (mime == "image/svg+xml").then_some(SVG_CSP),
                ..binary(ViewerBody::Static(bytes), mime)
            },
            None => error(404, b"{\"error\":\"no such picture\"}"),
        }));
    }
    if !matches!(req.path.as_str(), "/api/form/roles" | "/api/form" | "/form")
        && !req.path.starts_with("/res/cpic/")
        && !req.path.starts_with("/res/fpic/")
    {
        return None;
    }
    Some(run(snapshot, req, limit, control, paths?))
}
fn run<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
    paths: AnalysisPaths<'_>,
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
    if matches!(req.path.as_str(), "/api/form" | "/form") {
        let Some(path) = param(req, "path").filter(|p| !p.is_empty()) else {
            return Ok(error(400, b"{\"error\":\"path: required\"}"));
        };
        let dim = |name, default, range: std::ops::RangeInclusive<u32>| match param(req, name) {
            None => Some(default),
            Some(value) => value.parse().ok().filter(|v| range.contains(v)),
        };
        let (Some(w), Some(h)) = (dim("w", 1440, 320..=3840), dim("h", 900, 240..=2160)) else {
            return Ok(error(
                400,
                b"{\"error\":\"w, h: expected 320-3840 by 240-2160\"}",
            ));
        };
        if let Err(error) = path::resolve_managed(snapshot.open(), &snapshot.names(), path) {
            return failure::<S::Error>(budget, limit, error.into());
        }
        let kind = if let Some(role) = param(req, "generated") {
            let role = role.trim();
            let kind = [
                ("object", GeneratedKind::Object),
                ("group", GeneratedKind::Group),
                ("list", GeneratedKind::List),
                ("choice", GeneratedKind::Choice),
                ("record", GeneratedKind::Record),
            ]
            .into_iter()
            .find(|(name, _)| role.eq_ignore_ascii_case(name))
            .map(|(_, kind)| kind)
            .or_else(|| GeneratedKind::from_ru(role));
            let Some(kind) = kind else {
                return message(
                    budget,
                    limit,
                    400,
                    &format_args!(
                        "bad role `{role}`: expected object, group, list, choice or record"
                    ),
                );
            };
            Some(kind)
        } else {
            None
        };
        let result = analysis.preview_body(
            path,
            PreviewOptions {
                kind,
                viewport: [w, h],
                format: if req.path == "/form" {
                    PreviewFormat::HtmlUrls
                } else {
                    PreviewFormat::Json
                },
            },
        );
        control.check()?;
        return match result {
            Ok(PreviewProjection::Rendered(body)) => Ok(if req.path == "/form" {
                Response {
                    content_type: "text/html; charset=utf-8",
                    csp: Some(FORM_CSP),
                    ..response(200, body)
                }
            } else {
                response(200, body)
            }),
            Ok(PreviewProjection::Refused(body)) => match std::str::from_utf8(body.as_ref()) {
                Ok(text) => message(budget, limit, 422, &text),
                Err(_) => failure::<S::Error>(budget, limit, AnalysisError::Protocol.into()),
            },
            Err(error) => failure(budget, limit, error),
        };
    }
    if req.path == "/api/form/roles" {
        let Some(path) = param(req, "path").filter(|p| !p.is_empty()) else {
            return Ok(error(400, b"{\"error\":\"path: required\"}"));
        };
        let result = analysis.generated_roles_body(path);
        control.check()?;
        let roles = match result {
            Ok(body) => body,
            Err(e) => return failure(budget, limit, e),
        };
        let mut output = BudgetedWriter::new(budget, limit)?;
        output.try_write(b"{\"roles\":")?;
        for piece in roles.as_ref().chunks(64 << 10) {
            control.check()?;
            output.try_write(piece)?;
        }
        output.try_write(b"}")?;
        control.check()?;
        return Ok(response(200, ViewerBody::Owned(output.into_buffer())));
    }
    let (result, missing) = if let Some(name) = req.path.strip_prefix("/res/cpic/") {
        (
            analysis.configuration_picture_projection(name),
            b"{\"error\":\"no such picture or supported variant\"}".as_slice(),
        )
    } else {
        let name = req.path.strip_prefix("/res/fpic/").unwrap_or_default();
        if !ui_style::package::valid_path(name) {
            return Ok(error(400, b"{\"error\":\"invalid picture path\"}"));
        }
        let Some(path) = param(req, "path") else {
            return Ok(error(400, b"{\"error\":\"path: required\"}"));
        };
        let form = analysis.form_xml_projection(path);
        control.check()?;
        let result = match form {
            Ok(Some(form)) => analysis.embedded_picture_projection(&form, name),
            Ok(None) | Err(SelectedFormError::Path(ManagedResolveError::Path(_))) => Ok(None),
            Err(e) => Err(e),
        };
        (
            result,
            b"{\"error\":\"no such form picture or supported variant\"}".as_slice(),
        )
    };
    control.check()?;
    match result {
        Ok(lease) => picture(budget, limit, control, lease, missing),
        Err(e) => failure(budget, limit, e),
    }
}

fn structure<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    req: &Request,
    limit: u64,
    control: &RequestControl<'_>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: std::fmt::Display,
{
    use yellowcode_core::viewer::snapshot::{FormStructureProjection, NamesControl};
    let Some(path) = param(req, "path").filter(|p| !p.is_empty()) else {
        return Ok(error(400, b"{\"error\":\"path is required\"}"));
    };
    let interrupted = || control.is_canceled();
    let checked = NamesControl::new(control.canceled, control.deadline, &interrupted);
    let result = snapshot.form_structure_body(path, limit, &checked);
    control.check()?;
    match result {
        Ok(FormStructureProjection::Body(body)) => Ok(response(200, body)),
        Ok(FormStructureProjection::NotForm) => Ok(error(404, b"{\"error\":\"not a form\"}")),
        Ok(FormStructureProjection::Unavailable) => Ok(error(404, b"{\"error\":\"form structure unavailable: this build could not load a supported logical form\"}")),
        Err(error) => failure(snapshot.catalog().memory_budget(), limit, error),
    }
}
