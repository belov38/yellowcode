use super::{
    extension_info::{Uuid, quoted},
    extension_intercepts,
    extension_objects::Fragments,
    metadata::Format,
};
use crate::Result;
use brace::Guid;
use mdconf::{
    ManagedCfFile, ModuleKind,
    extension_objects::{ExtensionObjects, ModuleReadError, ModuleReadFailure, ObjectReadError},
    extension_reader::{ExtensionReadError, ExtensionRoot},
};
use memory_budget::{BudgetError, BudgetedWriter};
use serde::Serialize;
use std::{io::Write, path::Path};

const OUTPUT_LIMIT: u64 = 8 << 20;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("extension source: {0}")]
    Source(#[source] mdconf::Error),
    #[error(transparent)]
    Root(#[from] ExtensionReadError),
    #[error("configinfo manifest is absent; extension modules are unavailable")]
    NoManifest,
    #[error(transparent)]
    Object(#[from] ObjectReadError),
    #[error(transparent)]
    Module(#[from] ModuleReadError),
    #[error("extension module {module_kind} for object entry {entry_id}: {source}")]
    Intercepts {
        module_kind: &'static str,
        entry_id: Guid,
        #[source]
        source: crate::Error,
    },
    #[error("--entry-id must be a UUID")]
    EntryId,
    #[error("module {kind:?} for extension object entry {id} was not found")]
    NotFound { id: Guid, kind: String },
    #[error("extension output: {0}")]
    Output(#[source] BudgetError),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Row<'a> {
    entry_id: Uuid,
    header_id: Uuid,
    object_kind: Option<&'static str>,
    name: &'a str,
    module_kind: &'static str,
    source_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InterceptRow<'a> {
    entry_id: Uuid,
    object_kind: Option<&'static str>,
    name: &'a str,
    module_kind: &'static str,
    method: &'a str,
    intercept_kind: &'static str,
    target_method: &'a str,
}

enum Request {
    Show(Guid, String),
    Modules(Format),
    Intercepts(Format),
}

pub(super) fn run(
    path: &Path,
    format: Format,
    selection: Option<(String, String)>,
    out: &mut dyn Write,
) -> Result<()> {
    let selection = selection
        .map(|(id, kind)| Ok::<_, Error>((Guid::parse(&id).ok_or(Error::EntryId)?, kind)))
        .transpose()?;
    let request = match selection {
        Some((id, kind)) => Request::Show(id, kind),
        None => Request::Modules(format),
    };
    inspect(path, request, out)
}

pub(super) fn run_intercepts(path: &Path, format: Format, out: &mut dyn Write) -> Result<()> {
    inspect(path, Request::Intercepts(format), out)
}

fn inspect(path: &Path, request: Request, out: &mut dyn Write) -> Result<()> {
    let (source_budget, budget) = crate::service::memory::extension_info_budgets()?;
    if matches!(request, Request::Intercepts(_)) {
        budget.set_limits(extension_intercepts::command_limits());
    }
    let source = ManagedCfFile::open(path, &source_budget).map_err(Error::Source)?;
    let root = ExtensionRoot::read(&source, &budget)
        .map_err(Error::Root)?
        .ok_or(Error::NoManifest)?;
    let mut objects = root.objects(&source).map_err(Error::Object)?;
    let (format, include_intercepts) = match request {
        Request::Show(id, kind) => return show(&mut objects, id, kind, out),
        Request::Modules(format) => (format, false),
        Request::Intercepts(format) => (format, true),
    };

    let mut buffer = BudgetedWriter::new(&budget, OUTPUT_LIMIT).map_err(Error::Output)?;
    let mut fragments = Fragments::new(&mut buffer, OUTPUT_LIMIT);
    let analysis = include_intercepts.then(|| extension_intercepts::Analysis::new(&budget));
    let listed = list(&mut objects, format, analysis.as_ref(), &mut fragments);
    if let Some(error) = fragments.error() {
        return Err(Error::Output(error).into());
    }
    listed?;
    fragments.finish().map_err(Error::Output)?;
    out.write_all(buffer.as_slice())?;
    Ok(())
}

fn show(
    objects: &mut ExtensionObjects<'_>,
    id: Guid,
    kind: String,
    out: &mut dyn Write,
) -> Result<()> {
    if let Some(object) = objects.find(id).map_err(Error::Object)? {
        for module in object.modules().map_err(Error::Object)? {
            let module = module.map_err(Error::Module)?;
            if module.name() == kind {
                let text = module.read().map_err(Error::Module)?;
                out.write_all(text.as_ref())?;
                return Ok(());
            }
        }
    }
    Err(Error::NotFound { id, kind }.into())
}

fn list(
    objects: &mut ExtensionObjects<'_>,
    format: Format,
    analysis: Option<&extension_intercepts::Analysis<'_>>,
    out: &mut Fragments<'_>,
) -> Result<()> {
    if let Format::Json = format {
        let key = if analysis.is_some() {
            "intercepts"
        } else {
            "modules"
        };
        write!(out, "{{\"schemaVersion\":1,\"{key}\":[")?;
    }
    let mut first = true;
    while let Some(object) = objects.next().map_err(Error::Object)? {
        let header = object.header().map_err(Error::Object)?;
        for module in object.modules().map_err(Error::Object)? {
            let module = module.map_err(Error::Module)?;
            let text = match module.read() {
                Ok(text) => text,
                Err(error) if matches!(error.source, ModuleReadFailure::NoSource) => continue,
                Err(error) => return Err(Error::Module(error).into()),
            };
            let row = Row {
                entry_id: Uuid(object.reference.entry_id),
                header_id: Uuid(header.id),
                object_kind: object.reference.kind,
                name: header.name,
                module_kind: module.name(),
                source_bytes: text.as_ref().len(),
            };
            if let Some(analysis) = analysis {
                write_intercepts(
                    &row,
                    module.kind(),
                    text.as_ref(),
                    format,
                    analysis,
                    &mut first,
                    out,
                )?;
            } else {
                match format {
                    Format::Json => write_json_row(out, &mut first, &row)?,
                    Format::Text => {
                        row.write_text_prefix(out)?;
                        writeln!(out, " ({} bytes)", row.source_bytes)?;
                    }
                }
            }
        }
    }
    if let Format::Json = format {
        out.write_all(b"\n]}\n")?;
    }
    Ok(())
}

impl Row<'_> {
    fn write_text_prefix(&self, out: &mut dyn Write) -> Result<()> {
        write!(
            out,
            "{} {} ",
            self.entry_id.0,
            self.object_kind.unwrap_or("Unknown")
        )?;
        quoted(out, self.name)?;
        write!(out, " {}", self.module_kind)?;
        Ok(())
    }
}

fn write_intercepts(
    module: &Row<'_>,
    kind: ModuleKind,
    bytes: &[u8],
    format: Format,
    analysis: &extension_intercepts::Analysis<'_>,
    first: &mut bool,
    out: &mut Fragments<'_>,
) -> Result<()> {
    let module_error = |source| Error::Intercepts {
        module_kind: module.module_kind,
        entry_id: module.entry_id.0,
        source,
    };
    let source = std::str::from_utf8(bytes).map_err(|error| module_error(error.into()))?;
    let kind = compiler_kind(kind, module.object_kind);
    let intercepts =
        extension_intercepts::intercepts_with_budget(source, kind, analysis, out.remaining())
            .map_err(|error| module_error(error.into()))?;
    for intercept in &intercepts.rows {
        let kind = intercept.kind.annotation();
        match format {
            Format::Json => write_json_row(
                out,
                first,
                &InterceptRow {
                    entry_id: Uuid(module.entry_id.0),
                    object_kind: module.object_kind,
                    name: module.name,
                    module_kind: module.module_kind,
                    method: &intercept.method,
                    intercept_kind: kind,
                    target_method: &intercept.target_method,
                },
            )?,
            Format::Text => {
                module.write_text_prefix(out)?;
                write!(out, " {} {kind}(", intercept.method)?;
                quoted(out, &intercept.target_method)?;
                writeln!(out, ")")?;
            }
        }
    }
    Ok(())
}

fn compiler_kind(kind: ModuleKind, object_kind: Option<&str>) -> bsl_compiler::ModuleKind {
    use bsl_compiler::ModuleKind as Bsl;
    match (kind, object_kind) {
        (ModuleKind::Module, Some("CommonModule")) => Bsl::Common(Default::default()),
        (ModuleKind::Module, Some("HTTPService" | "IntegrationService")) => Bsl::HttpService,
        (ModuleKind::Module, Some("WebService")) => Bsl::WebService,
        (ModuleKind::ManagerModule, _) => Bsl::Manager,
        (ModuleKind::SessionModule, _) => Bsl::Session,
        _ => Bsl::Object,
    }
}

fn write_json_row(out: &mut dyn Write, first: &mut bool, row: &impl Serialize) -> Result<()> {
    if !*first {
        out.write_all(b",")?;
    }
    out.write_all(b"\n")?;
    serde_json::to_writer(&mut *out, row)?;
    *first = false;
    Ok(())
}

pub(super) fn parse_entry_id(value: &str) -> std::result::Result<String, String> {
    Guid::parse(value)
        .map(|_| value.to_owned())
        .ok_or_else(|| "expected a UUID such as 01234567-89ab-cdef-0123-456789abcdef".to_owned())
}
