use crate::app::AppNames;
use bsl_prim::CompatMode;
use bsl_syntax::ExecContext;
use platform_catalog::Catalog;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    Common(CommonFlags),
    Object,
    Manager,
    RecordSet,
    ValueManager,
    Command,
    Form,
    Session,
    ManagedApplication,
    OrdinaryApplication,
    ExternalConnection,
    HttpService,
    WebService,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CommonFlags {
    pub server: bool,
    pub client_managed: bool,
    pub client_ordinary: bool,
    pub external_connection: bool,
    pub server_call: bool,
    pub global: bool,
    pub privileged: bool,
    pub reuse: Reuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Reuse {
    #[default]
    DontUse,
    DuringRequest,
    DuringSession,
}

pub struct CompileOptions<'a> {
    pub module_name: &'a str,
    pub kind: ModuleKind,
    pub context: ExecContext,
    pub compat: CompatMode,
    pub catalog: &'a Catalog,
    pub app: &'a dyn AppNames,
    pub extension: Option<&'a str>,
}

impl ModuleKind {
    pub fn methods_only(self) -> bool {
        matches!(
            self,
            ModuleKind::Common(_)
                | ModuleKind::Manager
                | ModuleKind::Session
                | ModuleKind::HttpService
                | ModuleKind::WebService
        )
    }
}
