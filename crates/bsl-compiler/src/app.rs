use bsl_syntax::ExecContext;

pub trait AppNames: Send + Sync {
    fn global_value(&self, name: &str, ctx: ExecContext) -> Option<AppGlobal>;
    fn application_variable(&self, _name: &str, _ctx: ExecContext) -> Option<ApplicationVariable> {
        None
    }
    fn global_method(&self, name: &str, ctx: ExecContext) -> Option<AppMethodSig>;
    fn context_value(&self, module: &str, name: &str) -> Option<ContextValue>;
    fn context_method(&self, module: &str, name: &str) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppGlobal {
    CommonModule { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppMethodSig {
    pub module: String,
    pub name: String,
    pub is_function: bool,
    pub params: Vec<ParamSig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParamSig {
    pub by_val: bool,
    pub has_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextValue {
    ThisObject,
    Member,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoApp;

impl AppNames for NoApp {
    fn global_value(&self, _name: &str, _ctx: ExecContext) -> Option<AppGlobal> {
        None
    }
    fn global_method(&self, _name: &str, _ctx: ExecContext) -> Option<AppMethodSig> {
        None
    }
    fn context_value(&self, _module: &str, _name: &str) -> Option<ContextValue> {
        None
    }
    fn context_method(&self, _module: &str, _name: &str) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApplicationModule {
    Managed,
    Ordinary,
}

impl ApplicationModule {
    pub fn for_context(context: ExecContext) -> Option<Self> {
        match context {
            ExecContext::ThinClient | ExecContext::WebClient | ExecContext::ThickClientManaged => {
                Some(Self::Managed)
            }
            ExecContext::ThickClientOrdinary => Some(Self::Ordinary),
            _ => None,
        }
    }

    pub fn kind(self) -> crate::ModuleKind {
        match self {
            Self::Managed => crate::ModuleKind::ManagedApplication,
            Self::Ordinary => crate::ModuleKind::OrdinaryApplication,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApplicationVariable {
    pub module: ApplicationModule,
    pub name: String,
}
