use super::{SelectedAnalysis, SelectedAnalysisError, SelectedFormError};
use crate::viewer::{
    analysis_parent::{AnalysisError, FormXmlLease, PictureAssets},
    body::ViewerBody,
    path::{self, ManagedTarget},
};
pub use form_model::autoform::GeneratedKind;
use form_model::{
    autoform::facts,
    preview_layout::{self, Input, Picture},
    preview_render, preview_wire,
};
use mdconf::{ManagedEntrySource, MdKind, SubKind, startup::ViewerDataIdentity};
use memory_budget::BudgetedWriter;
use serde::Serialize;
use std::{
    fmt,
    io::Write,
    time::{Duration, Instant},
};
const WIRE_LIMIT: u64 = 256 << 20;
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    HtmlInline,
    HtmlUrls,
    Json,
    Text,
}
#[derive(Clone, Copy, Debug)]
pub struct PreviewOptions {
    pub kind: Option<GeneratedKind>,
    pub viewport: [u32; 2],
    pub format: PreviewFormat,
}
#[derive(Debug)]
pub enum PreviewProjection {
    Rendered(ViewerBody),
    Refused(ViewerBody),
}
impl PreviewProjection {
    pub fn body(&self) -> &[u8] {
        match self {
            Self::Rendered(body) | Self::Refused(body) => body.as_ref(),
        }
    }
}
#[derive(Serialize)]
struct Metadata<'a, T> {
    op: &'static str,
    platform: &'a str,
    snapshot: ViewerDataIdentity,
    form_ordinal: usize,
    kind: Option<&'static str>,
    lang: &'static str,
    #[serde(flatten)]
    options: T,
}
#[derive(Serialize)]
struct Empty {}
#[derive(Clone, Copy, Serialize)]
struct Layout<'a> {
    name: &'a str,
    viewport: [u32; 2],
}
#[derive(Serialize)]
struct Render<'a> {
    #[serde(flatten)]
    layout: Layout<'a>,
    form_path: Option<&'a str>,
    millis: u64,
    format: PreviewFormat,
}
fn kind_name(kind: GeneratedKind) -> &'static str {
    match kind {
        GeneratedKind::Object => "object",
        GeneratedKind::Group => "group",
        GeneratedKind::List => "list",
        GeneratedKind::Choice => "choice",
        GeneratedKind::Record => "record",
    }
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    fn preview_metadata<T>(
        &self,
        op: &'static str,
        ordinal: usize,
        kind: Option<GeneratedKind>,
        options: T,
    ) -> Metadata<'_, T> {
        Metadata {
            op,
            platform: self.snapshot.catalog.platform(),
            snapshot: self.snapshot.open().data_identity(),
            form_ordinal: ordinal,
            kind: kind.map(kind_name),
            lang: match self.snapshot.open().context().settings().script_variant {
                mdconf::ScriptVariant::English => "en",
                mdconf::ScriptVariant::Russian => "ru",
            },
            options,
        }
    }
    pub fn preview_body(
        &self,
        path: &str,
        options: PreviewOptions,
    ) -> Result<PreviewProjection, SelectedFormError<S::Error>> {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(120);
        let result = self.preview_body_before(path, options, started, deadline)?;
        self.check_before(deadline)?;
        Ok(result)
    }
    fn preview_body_before(
        &self,
        path: &str,
        options: PreviewOptions,
        started: Instant,
        deadline: Instant,
    ) -> Result<PreviewProjection, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let open = self.snapshot.open();
        let target = path::resolve_managed(open, &self.snapshot.names(), path)?;
        self.check_before(deadline)?;
        if options.kind.is_some() {
            let ManagedTarget::Object { ordinal, object } = target else {
                return self
                    .preview_refusal(format_args!("{path}: not a metadata object"), deadline);
            };
            let selected = open
                .context()
                .read_object(object.id)?
                .ok_or(AnalysisError::Protocol)?;
            self.check_before(deadline)?;
            let mut source = BudgetedWriter::new(self.snapshot.catalog.memory_budget(), WIRE_LIMIT)
                .map_err(AnalysisError::from)?;
            facts::encode(&*selected.object, |bytes| {
                self.check_before(deadline)?;
                source.try_write(bytes)?;
                self.check_before(deadline)
            })
            .map_err(|error| match error {
                facts::FactWriteError::Sink(error) => error,
                facts::FactWriteError::TooDeep | facts::FactWriteError::Overflow => {
                    AnalysisError::Resource
                }
            })?;
            drop(selected);
            self.check_before(deadline)?;
            return self.preview_source(
                path,
                ordinal,
                &object.name,
                source.as_slice(),
                None,
                options,
                started,
                deadline,
            );
        }
        let (ordinal, owner, child, name) = match target {
            ManagedTarget::Object { ordinal, object } if object.kind == MdKind::CommonForm => {
                (ordinal, object.id, None, object.name.as_str())
            }
            ManagedTarget::Sub {
                ordinal,
                owner,
                sub,
            } if sub.kind == SubKind::Form => (ordinal, owner.id, Some(sub.id), sub.name.as_str()),
            _ => return self.preview_refusal(format_args!("{path}: not a form"), deadline),
        };
        let form = self.form_xml_at(ordinal, owner, child, deadline)?;
        self.check_before(deadline)?;
        if let Some(form) = form.as_ref() {
            let projection = form.view()?;
            self.check_before(deadline)?;
            if let mdconf::forms::wire::Outcome::Present(present) = projection.outcome {
                return self.preview_source(
                    path,
                    ordinal,
                    name,
                    present.xml.as_bytes(),
                    Some(form),
                    options,
                    started,
                    deadline,
                );
            }
            if let Some(serialization) = projection.serialization.filter(|s| *s != "4/59") {
                let code = match projection.outcome {
                    mdconf::forms::wire::Outcome::Refused(code) => code.unwrap_or_default(),
                    _ => "",
                };
                return self.preview_refusal(format_args!("{path}: form preview unavailable: managed form serialization {serialization} is not translated: {code}"), deadline);
            }
        }
        self.preview_refusal(format_args!("{path}: form preview unavailable: this build could not load or convert the form for rendering"), deadline)
    }
    fn preview_refusal(
        &self,
        text: fmt::Arguments<'_>,
        deadline: Instant,
    ) -> Result<PreviewProjection, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let mut writer = BudgetedWriter::new(self.snapshot.catalog.memory_budget(), WIRE_LIMIT)
            .map_err(AnalysisError::from)?;
        let formatted = writer.serialization_sink().write_fmt(text);
        if let Some(error) = writer.error() {
            return Err(AnalysisError::from(*error).into());
        }
        formatted.map_err(|_| AnalysisError::Io)?;
        self.check_before(deadline)?;
        Ok(PreviewProjection::Refused(ViewerBody::Owned(
            writer.into_buffer(),
        )))
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "borrowed input owners and one original deadline remain explicit"
    )]
    fn preview_source(
        &self,
        path: &str,
        ordinal: usize,
        name: &str,
        source: &[u8],
        form: Option<&FormXmlLease>,
        options: PreviewOptions,
        started: Instant,
        deadline: Instant,
    ) -> Result<PreviewProjection, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let metadata = self.preview_metadata("prepare_preview", ordinal, options.kind, Empty {});
        let prepared = self
            .parent
            .prepare_preview_before(&metadata, source, deadline)?;
        self.check_before(deadline)?;
        let requests = match prepared.view()? {
            preview_wire::Prepared::Requests(requests) => requests,
            preview_wire::Prepared::ModelError(error) => {
                return self.preview_refusal(format_args!("{error}"), deadline);
            }
            preview_wire::Prepared::AutoformError(error) => {
                return self.preview_refusal(format_args!("{path}: {error}"), deadline);
            }
        };
        let answers = self
            .preview_type_answers_before(requests, WIRE_LIMIT, deadline)
            .map_err(form_error)?;
        let input = Input {
            source,
            requests: prepared.bytes(),
            answers: answers.as_ref(),
        };
        let layout = Layout {
            name,
            viewport: options.viewport,
        };
        let layout_meta = self.preview_metadata("layout_preview", ordinal, options.kind, layout);
        let laid_out = self
            .parent
            .layout_preview_before(&layout_meta, input, deadline)?;
        self.check_before(deadline)?;
        let millis =
            u64::try_from(started.elapsed().as_millis()).map_err(|_| AnalysisError::Resource)?;
        let pictures = match laid_out.view()? {
            preview_layout::Outcome::Pictures(pictures) => pictures,
            preview_layout::Outcome::ModelError(error)
            | preview_layout::Outcome::LayoutError(error) => {
                return self.preview_refusal(format_args!("{error}"), deadline);
            }
            preview_layout::Outcome::AutoformError(error) => {
                return self.preview_refusal(format_args!("{path}: {error}"), deadline);
            }
        };
        let assets = PictureAssets::collect(
            self.snapshot.catalog.memory_budget(),
            pictures,
            |picture| match picture {
                Picture::Std(_) => Err(AnalysisError::Protocol.into()),
                Picture::Config(name) => self.configuration_picture_before(name, deadline),
                Picture::Embedded(name) => match form {
                    Some(form) => self.embedded_picture_before(form, name, deadline),
                    None => Err(AnalysisError::Protocol.into()),
                },
            },
            || self.check_before(deadline),
        )?;
        drop(laid_out);
        self.check_before(deadline)?;
        let render_meta = self.preview_metadata(
            "render_preview",
            ordinal,
            options.kind,
            Render {
                layout,
                form_path: form.map(|_| path),
                millis,
                format: options.format,
            },
        );
        let rendered = self
            .parent
            .render_preview_before(&render_meta, input, &assets, deadline)?;
        self.check_before(deadline)?;
        let success = match rendered.view()? {
            preview_render::Outcome::Body(_) => true,
            preview_render::Outcome::ModelError(_) | preview_render::Outcome::LayoutError(_) => {
                false
            }
            preview_render::Outcome::AutoformError(error) => {
                return self.preview_refusal(format_args!("{path}: {error}"), deadline);
            }
        };
        let body = rendered.into_body_checked(|| self.check_before(deadline))?;
        self.check_before(deadline)?;
        Ok(if success {
            PreviewProjection::Rendered(body)
        } else {
            PreviewProjection::Refused(body)
        })
    }
}
fn form_error<E>(error: SelectedAnalysisError<E>) -> SelectedFormError<E> {
    match error {
        SelectedAnalysisError::Native(error) => error.into(),
        SelectedAnalysisError::Selection(mdconf::startup::ManagedSelectionError::Decode(error)) => {
            error.into()
        }
        _ => AnalysisError::Protocol.into(),
    }
}
