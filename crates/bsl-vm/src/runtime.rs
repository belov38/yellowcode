use crate::error::info::{self, CallKind, Position};
use crate::error::{ErrorInfo, Exception, FatalError, FatalKind, HostError, catalog_text};
use crate::host::{AppHost, ExecFlags, ReuseStore, SessionEnv};
use crate::object::HostObject;
use crate::registry::Registry;
use crate::types::{TypeId, TypeRegistry};
use crate::value::Value;
use bsl_prim::Str;
use platform_catalog::{Catalog, TypeIdx, messages::Lang};
use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Clone)]
pub struct Runtime(Arc<Inner>);

struct Inner {
    types: TypeRegistry,
    registry: Registry,
    app: Arc<dyn AppHost>,
    module_type: TypeId,
}

impl Runtime {
    pub fn new(
        catalog: &'static Catalog,
        mut registry: Registry,
        app: Arc<dyn AppHost>,
    ) -> Runtime {
        debug_assert!(
            std::ptr::eq(catalog, registry.catalog),
            "the registry was built for another catalog"
        );
        crate::lang::register_language(&mut registry);
        let types = TypeRegistry::new(catalog, std::mem::take(&mut registry.app_types));
        let module_type = catalog.find_type("ОбщийМодуль");
        debug_assert!(module_type.is_some(), "the catalog has no type ОбщийМодуль");
        let module_type = module_type.map_or(TypeId::UNDEFINED, TypeId::of_catalog);
        Runtime(Arc::new(Inner {
            types,
            registry,
            app,
            module_type,
        }))
    }

    pub(crate) fn global_fn_by_name(&self, name: &str) -> Option<crate::registry::GlobalFn> {
        self.0.registry.find_global_fn(name)
    }

    pub(crate) fn module_type(&self) -> TypeId {
        self.0.module_type
    }

    pub fn app(&self) -> &Arc<dyn AppHost> {
        &self.0.app
    }

    pub fn types(&self) -> &TypeRegistry {
        &self.0.types
    }

    pub fn has_type_impl(&self, t: TypeIdx) -> bool {
        self.0.registry.has_type_impl(t)
    }

    pub(crate) fn registry(&self) -> &Registry {
        &self.0.registry
    }

    pub fn execution(&self, env: Arc<dyn SessionEnv>, opts: ExecOptions) -> Execution {
        crate::object::restart_creation_numbers();
        Execution {
            runtime: self.clone(),
            session_tz: env.time_zone().to_owned(),
            messages: Messages::default(),
            env,
            opts,
            host_data: None,
            reuse: None,
            system_enums: HashMap::new(),
            exec: crate::exec::ExecState::default(),
        }
    }
}

pub const CALL_UNIT: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackWeights {
    pub call: u32,
    pub execute: u32,
    pub eval: u32,
    pub reentry: u32,
}

impl StackWeights {
    pub const MIN_REENTRY: u32 = 2 * CALL_UNIT;
}

impl Default for StackWeights {
    fn default() -> StackWeights {
        StackWeights {
            call: CALL_UNIT,
            execute: 140,
            eval: 142,
            reentry: 4 * CALL_UNIT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub privileged: bool,
    pub safe_mode_disabled: bool,
    pub budget: Option<u64>,
    pub max_depth: u32,
    pub memory_limit: Option<u64>,
    pub stack_limit: u64,
    pub weights: StackWeights,
    pub max_native_depth: u32,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for ExecOptions {
    fn default() -> ExecOptions {
        ExecOptions {
            privileged: false,
            safe_mode_disabled: false,
            budget: None,
            max_depth: 10_000,
            memory_limit: None,
            stack_limit: 46_000,
            weights: StackWeights::default(),
            max_native_depth: 64,
            cancel: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
    pub status: Option<String>,
}

pub const MESSAGE_CAP_COUNT: usize = 10_000;
pub const MESSAGE_CAP_BYTES: usize = 8 << 20;

#[derive(Default)]
struct Messages {
    kept: Vec<UserMessage>,
    bytes: usize,
    dropped: usize,
    closed: bool,
}

pub struct Execution {
    runtime: Runtime,
    env: Arc<dyn SessionEnv>,
    pub(crate) opts: ExecOptions,
    host_data: Option<Box<dyn Any>>,
    reuse: Option<ReuseStore>,
    session_tz: String,
    messages: Messages,
    pub(crate) system_enums: HashMap<TypeIdx, Value>,
    pub(crate) exec: crate::exec::ExecState,
}

impl Execution {
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn env(&self) -> &dyn SessionEnv {
        &*self.env
    }

    pub fn lang(&self) -> Lang {
        self.env.interface_language()
    }

    pub fn script_variant(&self) -> Lang {
        self.runtime.app().script_variant()
    }

    pub fn set_host_data(&mut self, d: Box<dyn Any>) {
        self.host_data = Some(d);
    }

    pub fn options(&self) -> &ExecOptions {
        &self.opts
    }

    pub fn host_data_ref(&self) -> Option<&dyn Any> {
        self.host_data.as_deref()
    }

    pub fn host_data(&mut self) -> Option<&mut dyn Any> {
        self.host_data.as_deref_mut()
    }

    pub fn take_host_data(&mut self) -> Option<Box<dyn Any>> {
        self.host_data.take()
    }

    pub fn attach_reuse(&mut self, s: ReuseStore) {
        self.reuse = Some(s);
    }

    pub fn detach_reuse(&mut self) -> ReuseStore {
        self.reuse.take().unwrap_or_default()
    }

    pub fn effective_flags(&self) -> ExecFlags {
        self.frame_flags()
    }

    pub fn session_time_zone(&self) -> &str {
        &self.session_tz
    }

    pub fn set_session_time_zone(&mut self, tz: String) {
        self.session_tz = tz;
    }

    pub fn push_message(&mut self, m: UserMessage) {
        let ms = &mut self.messages;
        let size = m.text.len() + m.status.as_ref().map_or(0, String::len);
        if !ms.closed && ms.kept.len() < MESSAGE_CAP_COUNT && size <= MESSAGE_CAP_BYTES - ms.bytes {
            ms.bytes += size;
            ms.kept.push(m);
        } else {
            ms.closed = true;
            ms.dropped += 1;
        }
    }

    pub fn take_messages(&mut self) -> (Vec<UserMessage>, usize) {
        let ms = std::mem::take(&mut self.messages);
        (ms.kept, ms.dropped)
    }

    pub fn reserve(&mut self, bytes: u64) -> Result<(), Exception> {
        match self.opts.memory_limit {
            Some(limit) if bytes > limit => Err(self.out_of_memory()),
            _ => Ok(()),
        }
    }

    pub fn out_of_memory(&self) -> Exception {
        Exception::Fatal(Rc::new(FatalError {
            kind: FatalKind::Memory,
            text: catalog_text("1cv8:IDS_ERR_BAD_ALLOC", &[], "", self.lang()),
            cause: None,
        }))
    }

    pub fn call_error(&mut self, e: crate::object::CallError) -> Exception {
        match e {
            crate::object::CallError::Exc(e) => e,
            crate::object::CallError::Host(h) => self.error(h),
        }
    }

    pub fn present(&mut self, v: &Value) -> Result<Str, Exception> {
        if let Some(p) = self.runtime.0.registry.presenter.clone() {
            return p(self, v);
        }
        let lang = self.lang();
        Ok(match v {
            Value::Undefined | Value::Null => Str::new(),
            Value::Bool(b) => {
                let ru = self.env.config_language().eq_ignore_ascii_case("ru");
                Str::from(match (ru, b) {
                    (true, true) => "Да",
                    (true, false) => "Нет",
                    (false, true) => "Yes",
                    (false, false) => "No",
                })
            }
            Value::Number(n) => Str::from(n.to_xml_string()),
            Value::String(s) => s.clone(),
            Value::Date(d) => Str::from(d.to_xml_string()),
            Value::Type(t) => Str::from(self.runtime.types().presentation(*t, lang)),
            Value::Object(o) => match o.presentation(self).map_err(|e| self.call_error(e))? {
                Some(s) => s,
                None => Str::from(
                    self.runtime
                        .types()
                        .presentation(HostObject::type_id(&**o), self.script_variant()),
                ),
            },
        })
    }

    pub fn error(&mut self, e: HostError) -> Exception {
        Exception::Catchable(info::from_host(self, e, CallKind::Vm, ""))
    }

    pub(crate) fn position(&self) -> Position {
        self.frames_position()
    }

    pub fn current_error(&self) -> Option<Rc<ErrorInfo>> {
        self.exec.frames.last()?.handlers.last().cloned()
    }
}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<Runtime>();
    send_sync::<Registry>();
    send::<ReuseStore>();
};
