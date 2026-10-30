use crate::error::{Exception, HostError};
use crate::fs::FileSystem;
use crate::place::Place;
pub use crate::reuse::ReuseStore;
use crate::runtime::Execution;
use crate::types::TypeId;
use crate::value::{ObjRef, Value};
use bsl_compiler::{AppGlobal, AppNames, CompiledModule};
use bsl_prim::{BslDate, Collation, CompatMode, fold_name};
use platform_catalog::{XdtoName, messages::Lang};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub trait SessionEnv: Send + Sync {
    fn reuse_clock(&self) -> std::time::Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
    fn locale(&self) -> &str;
    fn ib_locale(&self) -> &str;
    fn collation(&self) -> &dyn Collation;
    fn interface_language(&self) -> Lang;
    fn presentation_language(&self) -> Lang {
        self.interface_language()
    }
    fn config_language(&self) -> &str;
    fn ib_regional_settings(&self) -> Option<Arc<IbRegionalSettings>> {
        None
    }
    fn time_zone(&self) -> &str;
    fn now_utc(&self) -> BslDate;
    fn compat(&self) -> CompatMode;
    fn code_pages(&self) -> [&str; 3];
}

#[derive(Default)]
pub struct IbRegionalSettings {
    pub values: std::collections::BTreeMap<String, crate::SharedValue>,
}

pub trait AppHost: Send + Sync {
    fn names(&self) -> &dyn AppNames;
    fn global_value(&self, ex: &mut Execution, g: &AppGlobal) -> Result<Value, Exception>;
    fn module(&self, ex: &mut Execution, m: ModuleRef) -> Result<ModuleInstanceRef, Exception>;
    fn module_ref(&self, name: &str) -> Option<ModuleRef>;
    fn serializer(&self) -> Option<&dyn AppSerializer> {
        None
    }
    fn ib_regional_settings(&self) -> Option<Arc<IbRegionalSettings>> {
        None
    }
    fn set_ib_regional_settings(
        &self,
        _settings: Arc<IbRegionalSettings>,
    ) -> Result<(), HostError> {
        Err(HostError::not_implemented(
            "Глобальный контекст",
            "УстановитьРегиональныеНастройкиИнформационнойБазы",
        ))
    }
    fn file_system(&self) -> Option<&dyn FileSystem> {
        None
    }
    fn script_variant(&self) -> Lang {
        Lang::Ru
    }
    fn check(&self, _ex: &Execution, _op: GuardedOp) -> Result<(), HostError> {
        Ok(())
    }
}

pub trait AppSerializer: Send + Sync {
    fn xdto_type(&self, t: TypeId) -> Option<XdtoName>;
    fn internal_write(
        &self,
        _ex: &mut Execution,
        _v: &ObjRef,
    ) -> Result<Option<Vec<brace::Node>>, Exception> {
        Ok(None)
    }
    fn internal_read(
        &self,
        _ex: &mut Execution,
        _type_guid: &[u8; 16],
        _items: &[brace::Node],
    ) -> Result<Option<Value>, Exception> {
        Ok(None)
    }
    fn value_guid(&self, _t: TypeId) -> Option<[u8; 16]> {
        None
    }
    fn type_by_value_guid(&self, _g: &[u8; 16]) -> Option<TypeId> {
        None
    }
    fn xdto_write(&self, _ex: &mut Execution, _v: &ObjRef) -> Result<Option<AppXdto>, Exception> {
        Ok(None)
    }
    fn xdto_read(
        &self,
        ex: &mut Execution,
        _t: TypeId,
        _data: AppXdto,
    ) -> Result<Value, Exception> {
        Err(ex.error(HostError::not_implemented(
            "СериализаторXDTO",
            "ПрочитатьXML",
        )))
    }
}

#[derive(Clone, Debug)]
pub enum AppXdto {
    Text(String),
    Object(Vec<AppXdtoProp>),
}

#[derive(Clone, Debug)]
pub struct AppXdtoProp {
    pub name: String,
    pub ns: Option<String>,
    pub value: AppXdtoValue,
}

#[derive(Clone, Debug)]
pub enum AppXdtoValue {
    Value { value: Value, typed: bool },
    Text(String),
    Object(Vec<AppXdtoProp>),
    Nil,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardedOp {
    File,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecFlags {
    pub safe_mode: bool,
    pub privileged: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ModuleRef(pub u32);

pub enum ModuleContext {
    None,
    Object(ObjRef),
    Form(ObjRef),
}

#[derive(Clone)]
pub struct ModuleInstanceRef(pub(crate) Rc<ModuleInstance>);

pub(crate) struct ModuleInstance {
    module: Arc<CompiledModule>,
    pub(crate) vars: RefCell<Vec<Value>>,
    ctx: ModuleContext,
}

impl ModuleInstanceRef {
    pub(crate) fn new(module: Arc<CompiledModule>, ctx: ModuleContext) -> ModuleInstanceRef {
        let vars = vec![Value::Undefined; module.vars.len()];
        ModuleInstanceRef(Rc::new(ModuleInstance {
            module,
            vars: RefCell::new(vars),
            ctx,
        }))
    }

    pub fn module(&self) -> &Arc<CompiledModule> {
        &self.0.module
    }

    pub(crate) fn context(&self) -> &ModuleContext {
        &self.0.ctx
    }

    pub fn export_var(&self, name: &str) -> Option<Place> {
        let folded = fold_name(name);
        let i = self
            .0
            .module
            .vars
            .iter()
            .position(|v| v.export && *v.name.folded == *folded)?;
        Some(Place::mod_var(self.0.clone(), i))
    }
}
