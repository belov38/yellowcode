use crate::Application;
use crate::errors::{self, keys};
use crate::modules::internal_error;
use crate::serial::AppSerializerImpl;
use bsl_compiler::{AppGlobal, AppNames};
use bsl_vm::{
    AppHost, Exception, Execution, GuardedOp, HostError, ModuleInstanceRef, ModuleRef, Value,
};
use platform_catalog::messages::Lang;
use std::sync::Arc;

pub(crate) struct AppHostImpl {
    pub app: Arc<Application>,
    pub serializer: AppSerializerImpl,
}

impl AppHostImpl {
    pub fn new(app: Arc<Application>) -> AppHostImpl {
        AppHostImpl {
            serializer: AppSerializerImpl::new(app.clone()),
            app,
        }
    }
}

impl AppHost for AppHostImpl {
    fn names(&self) -> &dyn AppNames {
        &**self.app.names()
    }

    fn global_value(&self, ex: &mut Execution, g: &AppGlobal) -> Result<Value, Exception> {
        let AppGlobal::CommonModule { name } = g;
        match self
            .app
            .modules()
            .module_ref(&format!("ОбщийМодуль.{name}.Модуль"))
        {
            Some(r) => ex.module_value(r),
            None => Err(internal_error(format!("internal: no common module {name}"))),
        }
    }

    fn module(&self, ex: &mut Execution, m: ModuleRef) -> Result<ModuleInstanceRef, Exception> {
        self.app.instance_in_call(ex, m)
    }

    fn module_ref(&self, name: &str) -> Option<ModuleRef> {
        if name.contains('.') {
            self.app.modules().module_ref(name)
        } else {
            self.app
                .modules()
                .module_ref(&format!("ОбщийМодуль.{name}.Модуль"))
        }
    }

    fn script_variant(&self) -> Lang {
        match self.app.model().script {
            md_model::ScriptVariant::Russian => Lang::Ru,
            md_model::ScriptVariant::English => Lang::En,
        }
    }

    fn serializer(&self) -> Option<&dyn bsl_vm::AppSerializer> {
        Some(&self.serializer)
    }

    fn file_system(&self) -> Option<&dyn bsl_vm::FileSystem> {
        self.app.options().file_system.as_deref()
    }

    fn check(&self, ex: &Execution, _op: GuardedOp) -> Result<(), HostError> {
        if ex.effective_flags().safe_mode {
            return Err(match errors::msg(keys::SAFE_MODE) {
                Some(m) => HostError::text(m.ru, m.en),
                None => HostError::text(keys::SAFE_MODE, keys::SAFE_MODE),
            });
        }
        Ok(())
    }
}
