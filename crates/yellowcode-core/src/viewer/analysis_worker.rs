use super::{Lang, check, definition, literals, module};
use mdconf::{Module, ModuleKind};
pub use memory_budget::sealed_heap::SealedHeap as Heap;
use memory_budget::sealed_heap::{
    ANALYSIS_HEAP_BYTES, ANALYSIS_OWNER_STACK_BYTES, RESOURCE_EXIT_CODE,
};
use serde::Deserialize;
use std::io::{Read, Write};
pub const INTERNAL_SWITCH: &str = "--internal-viewer-analysis";
pub const PROTOCOL_EXIT_CODE: i32 = 74;
const OUTPUT_IO_EXIT_CODE: i32 = 75;
pub const INPUT_IO_EXIT_CODE: i32 = 76;
const MAX_METADATA_BYTES: usize = 64 << 10;

#[derive(Deserialize)]
enum WireKind {
    Module,
    ObjectModule,
    ManagerModule,
    RecordSetModule,
    ValueManagerModule,
    CommandModule,
    FormModule,
    ManagedApplicationModule,
    OrdinaryApplicationModule,
    SessionModule,
    ExternalConnectionModule,
}
impl WireKind {
    fn native(self) -> ModuleKind {
        match self {
            Self::Module => ModuleKind::Module,
            Self::ObjectModule => ModuleKind::ObjectModule,
            Self::ManagerModule => ModuleKind::ManagerModule,
            Self::RecordSetModule => ModuleKind::RecordSetModule,
            Self::ValueManagerModule => ModuleKind::ValueManagerModule,
            Self::CommandModule => ModuleKind::CommandModule,
            Self::FormModule => ModuleKind::FormModule,
            Self::ManagedApplicationModule => ModuleKind::ManagedApplicationModule,
            Self::OrdinaryApplicationModule => ModuleKind::OrdinaryApplicationModule,
            Self::SessionModule => ModuleKind::SessionModule,
            Self::ExternalConnectionModule => ModuleKind::ExternalConnectionModule,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireLang {
    En,
    Ru,
}
impl WireLang {
    fn native(self) -> Lang {
        match self {
            Self::En => Lang::En,
            Self::Ru => Lang::Ru,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireCtx {
    Server,
    ThinClient,
    WebClient,
    All,
}
impl WireCtx {
    fn native(self) -> check::Ctx {
        match self {
            Self::Server => check::Ctx::Server,
            Self::ThinClient => check::Ctx::ThinClient,
            Self::WebClient => check::Ctx::WebClient,
            Self::All => check::Ctx::All,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Request {
    #[serde(rename = "render_preview")]
    RenderPreview {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "form_ordinal")]
        _form_ordinal: u64,
        kind: Option<super::form::worker::Kind>,
        lang: WireLang,
        name: String,
        viewport: [u32; 2],
        form_path: Option<String>,
        millis: u64,
        format: super::form::worker::RenderFormat,
    },
    #[serde(rename = "picture_resource")]
    PictureResource {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        ext: String,
    },
    #[serde(rename = "layout_preview")]
    LayoutPreview {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "form_ordinal")]
        _form_ordinal: u64,
        kind: Option<super::form::worker::Kind>,
        lang: WireLang,
        name: String,
        viewport: [u32; 2],
    },
    #[serde(rename = "prepare_preview")]
    PreparePreview {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "form_ordinal")]
        _form_ordinal: u64,
        kind: Option<super::form::worker::Kind>,
        lang: WireLang,
    },
    #[serde(rename = "generated_roles")]
    GeneratedRoles {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "object_ordinal")]
        _object_ordinal: u64,
        lang: WireLang,
    },
    #[serde(rename = "form_xml")]
    FormXml {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "form_ordinal")]
        _form_ordinal: u64,
        owner: Option<String>,
        shared: bool,
    },
    #[serde(rename = "check")]
    Check {
        platform: String,
        snapshot: u64,
        module_ordinal: u64,
        compat: u32,
        kind: WireKind,
        name: String,
        rel: String,
        names_dir: std::path::PathBuf,
        lang: WireLang,
        contexts: WireCtx,
        own_contexts: bool,
    },
    #[serde(rename = "literals")]
    Literals {
        platform: String,
        snapshot: u64,
        module_ordinal: u64,
        compat: u32,
        kind: WireKind,
    },
    #[serde(rename = "module_view")]
    ModuleView {
        platform: String,
        #[serde(rename = "snapshot")]
        _snapshot: u64,
        #[serde(rename = "module_ordinal")]
        _module_ordinal: u64,
        #[serde(rename = "compat")]
        _compat: u32,
        kind: WireKind,
        lang: WireLang,
        plain: bool,
        modules: Vec<String>,
    },
    #[serde(rename = "definition_probe")]
    DefinitionProbe {
        platform: String,
        snapshot: u64,
        module_ordinal: u64,
        compat: u32,
        kind: WireKind,
        path: String,
        module: String,
        line: u32,
        col: u32,
    },
    #[serde(rename = "definition_method")]
    DefinitionMethod {
        platform: String,
        snapshot: u64,
        module_ordinal: u64,
        compat: u32,
        kind: WireKind,
        path: String,
        name: String,
        exported_only: bool,
    },
}
impl Request {
    fn platform(&self) -> &str {
        match self {
            Self::RenderPreview { platform, .. }
            | Self::PictureResource { platform, .. }
            | Self::LayoutPreview { platform, .. }
            | Self::PreparePreview { platform, .. }
            | Self::GeneratedRoles { platform, .. }
            | Self::FormXml { platform, .. }
            | Self::Check { platform, .. }
            | Self::Literals { platform, .. }
            | Self::ModuleView { platform, .. }
            | Self::DefinitionProbe { platform, .. }
            | Self::DefinitionMethod { platform, .. } => platform,
        }
    }
}
#[allow(
    unsafe_code,
    reason = "audited executable startup is the sole activation boundary for the installed heap"
)]
pub unsafe fn run_at_startup(heap: &'static Heap) -> i32 {
    if unsafe { heap.seal(ANALYSIS_HEAP_BYTES) }.is_err() {
        return RESOURCE_EXIT_CODE;
    }
    let Ok(worker) = std::thread::Builder::new()
        .name("viewer-analysis".into())
        .stack_size(ANALYSIS_OWNER_STACK_BYTES)
        .spawn(move || run(heap))
    else {
        return RESOURCE_EXIT_CODE;
    };
    worker.join().unwrap_or(PROTOCOL_EXIT_CODE)
}
fn read_length(input: &mut impl Read, heap: &Heap, maximum: usize) -> Result<usize, i32> {
    let mut header = [0u8; 8];
    input
        .read_exact(&mut header)
        .map_err(|_| PROTOCOL_EXIT_CODE)?;
    let length = usize::try_from(u64::from_le_bytes(header)).map_err(|_| RESOURCE_EXIT_CODE)?;
    if length > maximum || length > heap.capacity().saturating_sub(heap.used()) {
        return Err(RESOURCE_EXIT_CODE);
    }
    Ok(length)
}
fn run(heap: &Heap) -> i32 {
    let input = std::io::stdin();
    let mut input = input.lock();
    let length = match read_length(&mut input, heap, MAX_METADATA_BYTES) {
        Ok(length) => length,
        Err(code) => return code,
    };
    let mut bytes = vec![0u8; length];
    if input.read_exact(&mut bytes).is_err() {
        return PROTOCOL_EXIT_CODE;
    }
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return PROTOCOL_EXIT_CODE;
    };
    if platform_catalog::select(&platform_catalog::PlatformVersion::new(request.platform()))
        .is_err()
    {
        return PROTOCOL_EXIT_CODE;
    }
    let source_len = match read_length(&mut input, heap, ANALYSIS_HEAP_BYTES) {
        Ok(length) => length,
        Err(code) => return code,
    };
    let mut source = vec![0u8; source_len];
    if input.read_exact(&mut source).is_err() {
        return PROTOCOL_EXIT_CODE;
    }
    let facts = if matches!(
        &request,
        Request::FormXml { .. } | Request::RenderPreview { .. }
    ) {
        let length = match read_length(&mut input, heap, ANALYSIS_HEAP_BYTES) {
            Ok(length) => length,
            Err(code) => return code,
        };
        let mut facts = vec![0u8; length];
        if input.read_exact(&mut facts).is_err() {
            return PROTOCOL_EXIT_CODE;
        }
        Some(facts)
    } else {
        None
    };
    let mut tail = [0u8; 1];
    if !matches!(input.read(&mut tail), Ok(0)) {
        return PROTOCOL_EXIT_CODE;
    }
    let value = match request {
        Request::RenderPreview {
            kind,
            lang,
            name,
            viewport,
            form_path,
            millis,
            format,
            ..
        } => {
            let Some(assets) = facts else {
                return PROTOCOL_EXIT_CODE;
            };
            let Ok(output) = super::form::worker::render(
                &source,
                &assets,
                super::form::worker::RenderOptions {
                    kind,
                    lang: lang.native(),
                    name: &name,
                    viewport,
                    form_path: form_path.as_deref(),
                    millis,
                    format,
                },
            ) else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }
        Request::PictureResource { ext, .. } => {
            let Ok(output) = super::form::worker::picture(&source, &ext) else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }

        Request::LayoutPreview {
            kind,
            lang,
            name,
            viewport,
            ..
        } => {
            let Ok(output) =
                super::form::worker::layout(&source, kind, lang.native(), &name, viewport)
            else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }

        Request::PreparePreview { kind, lang, .. } => {
            let Ok(output) = super::form::worker::prepare(&source, kind, lang.native()) else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }
        Request::GeneratedRoles { lang, .. } => {
            let Ok(output) = super::form::worker::roles(&source, lang.native()) else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }
        Request::FormXml { owner, shared, .. } => {
            let Some(facts) = facts else {
                return PROTOCOL_EXIT_CODE;
            };
            let Ok(facts) = mdconf::forms::wire::decode_facts(&facts) else {
                return PROTOCOL_EXIT_CODE;
            };
            let Ok(output) = mdconf::forms::wire::render(source, facts, owner.as_deref(), shared)
            else {
                return PROTOCOL_EXIT_CODE;
            };
            return publish(&output);
        }
        Request::Check {
            snapshot,
            module_ordinal,
            compat,
            kind,
            name,
            rel,
            names_dir,
            lang,
            contexts,
            own_contexts,
            ..
        } => {
            let _identity = (snapshot, module_ordinal);
            let m = Module {
                kind: kind.native(),
                source,
            };
            let items = [check::Item {
                name,
                rel,
                module: &m,
            }];
            let run = check::Run {
                ctxs: contexts.native(),
                own_contexts,
                lang: lang.native(),
            };
            match check::check_items_strict(check::compat_of(compat), &names_dir, &items, run) {
                Ok(mut report) => {
                    let warnings = std::mem::take(&mut report.warnings);
                    serde_json::json!({"report": report, "warnings": warnings})
                }
                Err(error) => {
                    return match error.kind {
                        bsl_testhost::ScanErrorKind::Io | bsl_testhost::ScanErrorKind::Changed => {
                            INPUT_IO_EXIT_CODE
                        }
                        bsl_testhost::ScanErrorKind::Poisoned
                        | bsl_testhost::ScanErrorKind::InvalidData => PROTOCOL_EXIT_CODE,
                    };
                }
            }
        }
        Request::Literals {
            kind,
            snapshot,
            module_ordinal,
            compat,
            ..
        } => {
            let _identity = (snapshot, module_ordinal, compat);
            let m = Module {
                kind: kind.native(),
                source,
            };
            literals::literals(&m.text())
        }
        Request::ModuleView {
            kind,
            lang,
            plain,
            modules,
            ..
        } => module::view(
            &Module {
                source,
                kind: kind.native(),
            },
            lang.native(),
            plain,
            modules,
        ),
        Request::DefinitionProbe {
            kind,
            path,
            line,
            col,
            snapshot,
            module_ordinal,
            compat,
            module,
            ..
        } => {
            let _identity = (snapshot, module_ordinal, compat, module);
            let m = Module {
                kind: kind.native(),
                source,
            };
            serde_json::json!({"attempts": definition::probe(&m.text(), &path, m.kind, line, col)})
        }
        Request::DefinitionMethod {
            kind,
            path,
            name,
            exported_only,
            snapshot,
            module_ordinal,
            compat,
            ..
        } => {
            let _identity = (snapshot, module_ordinal, compat);
            let m = Module {
                kind: kind.native(),
                source,
            };
            let Ok(value) = serde_json::to_value(definition::method_location(
                &m.text(),
                &path,
                m.kind,
                &name,
                exported_only,
            )) else {
                return PROTOCOL_EXIT_CODE;
            };
            value
        }
    };
    let Ok(output) = serde_json::to_vec(&value) else {
        return PROTOCOL_EXIT_CODE;
    };
    publish(&output)
}
fn publish(output: &[u8]) -> i32 {
    let Ok(mut out) = output_file() else {
        return OUTPUT_IO_EXIT_CODE;
    };
    if out
        .write_all(&(output.len() as u64).to_le_bytes())
        .and_then(|()| out.write_all(output))
        .and_then(|()| out.flush())
        .is_err()
    {
        return OUTPUT_IO_EXIT_CODE;
    }
    0
}

#[cfg(unix)]
fn output_file() -> std::io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
}

#[cfg(windows)]
fn output_file() -> std::io::Result<std::fs::File> {
    use std::os::windows::io::AsHandle;
    std::io::stdout()
        .as_handle()
        .try_clone_to_owned()
        .map(std::fs::File::from)
}
