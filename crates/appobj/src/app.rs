use crate::host::AppHostImpl;
use crate::model::{AppModel, Uuid16};
use crate::modules::{CompileCache, Modules, internal_error};
use crate::names::AppNamesImpl;
use crate::rights::{Right, RightTarget, RightsMatrix};
use crate::session::{
    CallContext, CallOptions, CallOutcome, HANDLER, ParamSlot, SessionError, SessionHooks,
    SessionStart, SessionState, SessionUser, call_ctx, session_params_value,
};
use crate::types::AppTypes;
use crate::values::uuid1::{SystemClock, Uuid1Gen};
use crate::{AppError, Result};
use bsl_compiler::CompileOptions;
use bsl_prim::CompatMode;
use bsl_syntax::ExecContext;
use bsl_vm::{Arg, Exception, Execution, ModuleInstanceRef, ModuleRef, SharedValue, TypeId, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

pub struct AppOpen {
    pub store: Arc<dyn ibschema::store::Store>,
    pub config: Arc<md_model::Configuration>,
    pub catalog: &'static platform_catalog::Catalog,
    pub event_sink: Arc<dyn evlog::EventSink>,
    pub options: AppOptions,
}

#[derive(Clone)]
pub struct AppOptions {
    pub read_only_scope: Option<crate::ReadOnlyScope>,
    pub lock_timeout: Duration,
    pub compile_cache: Option<Arc<CompileCache>>,
    pub uuid_node: Option<[u8; 6]>,
    pub extensions: Vec<()>,
    pub query_mem_limit: u64,
    pub locale: String,
    pub file_system: Option<Arc<dyn bsl_vm::FileSystem>>,
}

impl Default for AppOptions {
    fn default() -> AppOptions {
        AppOptions {
            read_only_scope: None,
            lock_timeout: Duration::from_secs(20),
            compile_cache: None,
            uuid_node: None,
            extensions: Vec::new(),
            query_mem_limit: 1 << 30,
            locale: "ru_RU".to_string(),
            file_system: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleTarget {
    Common(String),
    HttpService(Uuid16),
    WebService(Uuid16),
    Session,
}

pub struct Application {
    model: Arc<AppModel>,
    store: Arc<dyn ibschema::store::Store>,
    names: Arc<AppNamesImpl>,
    modules: Modules,
    types: OnceLock<AppTypes>,
    pub(crate) rights: RightsMatrix,
    sink: Arc<dyn evlog::EventSink>,
    options: AppOptions,
    catalog: &'static platform_catalog::Catalog,
    uuid1: Uuid1Gen,
    me: Weak<Application>,
    md: Mutex<Weak<crate::meta::MdCtx>>,
    write_epoch: AtomicU64,
    query: OnceLock<std::result::Result<Arc<QueryCtx>, String>>,
    streams: Arc<AtomicUsize>,
    predefined: crate::predefined::Predefined,
}

pub(crate) struct QueryCtx {
    pub schema: Arc<query_ir::QuerySchema>,
    pub binding: query_ir::StorageBinding,
    pub map: Arc<ibschema::storage::StorageMap>,
    pub collation: Arc<dyn bsl_prim::Collation>,
}

impl Application {
    pub fn open(mut o: AppOpen) -> Result<Arc<Application>> {
        if let Some(scope) = &o.options.read_only_scope {
            o.store = scope.bind(o.store)?;
        }
        let mut model = AppModel::build(o.config)?;
        model.bind(o.store.storage())?;
        let compat = compat_of(&model)?;
        let rights = RightsMatrix::build(&model.config);
        let model = Arc::new(model);
        let names = Arc::new(AppNamesImpl::new(model.clone()));
        let cache = o.options.compile_cache.clone().unwrap_or_default();
        let modules = Modules::new(names.clone(), cache, compat);
        let uuid1 = Uuid1Gen::new(o.options.uuid_node, Arc::new(SystemClock));
        Ok(Arc::new_cyclic(|me| Application {
            model,
            store: o.store,
            names,
            modules,
            types: OnceLock::new(),
            rights,
            sink: o.event_sink,
            options: o.options,
            catalog: o.catalog,
            uuid1,
            me: me.clone(),
            md: Mutex::new(Weak::new()),
            write_epoch: AtomicU64::new(0),
            query: OnceLock::new(),
            streams: Arc::new(AtomicUsize::new(0)),
            predefined: crate::predefined::Predefined::default(),
        }))
    }

    pub fn arc(&self) -> Option<Arc<Application>> {
        self.me.upgrade()
    }

    pub fn write_epoch(&self) -> u64 {
        self.write_epoch.load(Ordering::Acquire)
    }

    pub fn bump_write_epoch(&self) {
        self.write_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn query_ctx(&self) -> std::result::Result<&Arc<QueryCtx>, bsl_vm::HostError> {
        self.query_ctx_text().map_err(|e| {
            let t = format!("internal: {e}");
            bsl_vm::HostError::text(&t, &t)
        })
    }

    pub(crate) fn query_ctx_text(&self) -> std::result::Result<&Arc<QueryCtx>, &str> {
        let built = self.query.get_or_init(|| {
            let schema = query_ir::QuerySchema::build(&self.model.config, self.catalog)
                .map_err(|e| format!("query schema: {e}"))?;
            let map = self.store.storage();
            let binding = schema
                .bind_storage(map)
                .map_err(|e| format!("query storage binding: {e}"))?;
            Ok(Arc::new(QueryCtx {
                schema,
                binding,
                map: Arc::new(map.clone()),
                collation: Arc::new(bsl_prim::IcuCollation::new(&self.options.locale)),
            }))
        });
        built.as_ref().map_err(String::as_str)
    }

    pub(crate) fn live_streams(&self) -> &Arc<AtomicUsize> {
        &self.streams
    }

    pub(crate) fn touch_table(
        &self,
        t: ibschema::storage::TableId,
    ) -> std::result::Result<(), query_ir::HostError> {
        self.predefined
            .touch(self, t)
            .map_err(|e| crate::predefined::engine_error(&e))
    }

    #[doc(hidden)]
    pub fn predefined_item_names(&self, obj: crate::model::ObjId) -> Vec<([u8; 16], String)> {
        self.predefined.names(self, obj)
    }

    pub(crate) fn predefined(&self) -> &crate::predefined::Predefined {
        &self.predefined
    }

    pub fn uuid1(&self) -> &Uuid1Gen {
        &self.uuid1
    }

    pub(crate) fn set_md_ctx(&self, cx: &Arc<crate::meta::MdCtx>) {
        *self.md.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(cx);
    }

    pub(crate) fn md_ctx(&self) -> Option<Arc<crate::meta::MdCtx>> {
        self.md.lock().unwrap_or_else(|e| e.into_inner()).upgrade()
    }

    pub fn model(&self) -> &Arc<AppModel> {
        &self.model
    }

    pub fn store(&self) -> &Arc<dyn ibschema::store::Store> {
        &self.store
    }

    pub fn names(&self) -> &Arc<AppNamesImpl> {
        &self.names
    }

    pub fn modules(&self) -> &Modules {
        &self.modules
    }

    pub fn types(&self) -> Option<&AppTypes> {
        self.types.get()
    }

    pub fn event_sink(&self) -> &Arc<dyn evlog::EventSink> {
        &self.sink
    }

    pub fn options(&self) -> &AppOptions {
        &self.options
    }

    pub fn register(self: &Arc<Self>, reg: &mut bsl_vm::Registry) -> Result<()> {
        let query = Arc::new(crate::CallQueryProvider::new(self.clone())?);
        let types = AppTypes::register_with(&self.model, reg, Some(Arc::downgrade(self)))?;
        if let Err(second) = self.types.set(types) {
            debug_assert!(
                self.types.get().is_some_and(|t| t.same_ids(&second)),
                "a second registry of one Application got different type ids"
            );
        }
        let ty = self
            .catalog
            .find_type("ПараметрыСеанса")
            .map(TypeId::of_catalog)
            .ok_or_else(|| {
                AppError::Unsupported("the catalog has no type ПараметрыСеанса".into())
            })?;
        let app = self.clone();
        reg.global_prop(
            "ПараметрыСеанса",
            "SessionParameters",
            Arc::new(move |_| Ok(session_params_value(&app, ty))),
        );
        crate::meta::register(self, reg)?;
        crate::managers::register(self, reg);
        crate::rights::register(self, reg);
        crate::session_info::register(reg);
        crate::predefined::register(self, reg);
        query::register(reg, query);
        Ok(())
    }

    pub fn has_right(&self, user: &SessionUser, target: RightTarget<'_>, right: Right) -> bool {
        if user.no_users {
            return true;
        }
        match target {
            RightTarget::Configuration => self.rights.any_role(&user.roles, None, right),
            RightTarget::Object(id) => self.rights.any_role(&user.roles, Some(&id), right),
            RightTarget::HttpMethod {
                service,
                template,
                method,
            } => self
                .http_method_id(service, template, method)
                .is_some_and(|id| self.rights.any_role(&user.roles, Some(&id), right)),
        }
    }

    fn http_method_id(&self, service: Uuid16, template: &str, method: &str) -> Option<Uuid16> {
        use md_model::ChildTag;
        let svc = self.model.config.object(service)?;
        let t = svc
            .children
            .iter()
            .find(|c| c.tag == ChildTag::URLTemplate && c.name == template)?;
        t.children
            .iter()
            .find(|c| c.tag == ChildTag::Method && c.name == method)
            .map(|c| c.id)
    }

    pub fn host(self: &Arc<Self>) -> Arc<dyn bsl_vm::AppHost> {
        Arc::new(AppHostImpl::new(self.clone()))
    }

    pub fn start_session(
        &self,
        ex: &mut Execution,
        start: SessionStart,
        hooks: &mut dyn SessionHooks,
    ) -> std::result::Result<SessionState, SessionError> {
        let state = SessionState::new(start, self.model.session_params.len());
        hooks.zones_set(&state)?;
        self.modules
            .initialize_globals(ex.lang())
            .map_err(SessionError::GlobalModule)?;
        self.begin_call(ex, state, CallOptions::default());
        let r = self.run_session_module(ex);
        let (state, _) = self.end_call(ex);
        r.map(|()| state).map_err(SessionError::SessionModule)
    }

    pub fn bare_session(&self, start: SessionStart) -> SessionState {
        SessionState::new(start, self.model.session_params.len())
    }

    fn run_session_module(&self, ex: &mut Execution) -> std::result::Result<(), Exception> {
        let Some(m) = self.modules.session_module() else {
            return Ok(());
        };
        let inst = self.instance_in_call(ex, m)?;
        if inst.module().method(HANDLER).is_none() {
            return Ok(());
        }
        ex.call_privileged(&inst, HANDLER, &mut [Arg::Val(Value::Undefined)])
            .map(drop)
    }

    pub fn begin_call(&self, ex: &mut Execution, mut sess: SessionState, opts: CallOptions) {
        let shared = std::mem::take(&mut sess.params);
        let reuse = std::mem::take(&mut sess.reuse);
        let mut params: Vec<ParamSlot> = shared
            .iter()
            .map(|s| match s {
                Some(sv) => sv.to_value(ex).map_or(ParamSlot::Unset, ParamSlot::Set),
                None => ParamSlot::Unset,
            })
            .collect();
        params.resize(self.model.session_params.len(), ParamSlot::Unset);
        debug_assert!(
            call_ctx(ex).is_none(),
            "begin_call over an existing call context"
        );
        ex.attach_reuse(reuse);
        ex.set_host_data(Box::new(CallContext {
            query: crate::queryhost::QueryCallState::new(self.me.clone()),
            sess,
            params,
            common: HashMap::new(),
            md_cache: HashMap::new(),
            mgr_cache: HashMap::new(),
            read_cache: HashMap::new(),
            selections: Vec::new(),
            opts,
        }));
    }

    pub fn end_call(&self, ex: &mut Execution) -> (SessionState, CallOutcome) {
        let reuse = ex.detach_reuse();
        let ctx = ex
            .take_host_data()
            .and_then(|b| b.downcast::<CallContext>().ok());
        let Some(ctx) = ctx else {
            return (SessionState::detached(), CallOutcome::default());
        };
        let mut ctx = *ctx;
        ctx.query.end();
        for s in ctx.selections.drain(..) {
            if let Some(s) = s.upgrade() {
                s.release();
            }
        }
        let CallContext {
            mut sess, params, ..
        } = ctx;
        sess.params = params
            .iter()
            .map(|s| match s {
                ParamSlot::Set(v) => SharedValue::from_value(v),
                ParamSlot::Unset | ParamSlot::Running => None,
            })
            .collect();
        sess.reuse = reuse;
        (sess, CallOutcome { rolled_back: 0 })
    }

    pub(crate) fn instance_in_call(
        &self,
        ex: &mut Execution,
        m: ModuleRef,
    ) -> std::result::Result<ModuleInstanceRef, Exception> {
        if let Some(i) = call_ctx(ex).and_then(|c| c.common.get(&m).cloned()) {
            return Ok(i);
        }
        let inst = self.modules.instance(ex, m)?;
        Ok(match call_ctx(ex) {
            Some(c) => c.common.entry(m).or_insert(inst).clone(),
            None => inst,
        })
    }

    pub fn module_instance(
        &self,
        ex: &mut Execution,
        m: ModuleTarget,
    ) -> std::result::Result<ModuleInstanceRef, Exception> {
        let name = match &m {
            ModuleTarget::Common(n) => format!("ОбщийМодуль.{n}.Модуль"),
            ModuleTarget::HttpService(id) => {
                self.service_module("HTTPСервис", md_model::MdKind::HTTPService, id)
            }
            ModuleTarget::WebService(id) => {
                self.service_module("WebСервис", md_model::MdKind::WebService, id)
            }
            ModuleTarget::Session => {
                let r = self.modules.session_module().ok_or_else(|| {
                    internal_error("internal: the configuration has no session module".into())
                })?;
                return self.instance_in_call(ex, r);
            }
        };
        let r = self
            .modules
            .module_ref(&name)
            .ok_or_else(|| internal_error(format!("internal: no module {name}")))?;
        self.instance_in_call(ex, r)
    }

    fn service_module(&self, prefix: &str, kind: md_model::MdKind, id: &Uuid16) -> String {
        let name = self
            .model
            .config
            .objects
            .iter()
            .find(|o| o.kind == kind && o.id == *id)
            .map_or_else(String::new, |o| o.name.clone());
        format!("{prefix}.{name}.Модуль")
    }

    pub fn compile_options<'a>(
        &'a self,
        name: &'a str,
        kind: bsl_compiler::ModuleKind,
    ) -> CompileOptions<'a> {
        CompileOptions {
            module_name: name,
            kind,
            context: ExecContext::Server,
            compat: self.modules.compat(),
            catalog: self.catalog,
            app: &*self.names,
            extension: None,
        }
    }
}

fn compat_of(model: &AppModel) -> Result<CompatMode> {
    let name = model.config.compatibility_mode().xml_name();
    CompatMode::from_xml_name(&name)
        .map_err(|e| AppError::Unsupported(format!("compatibility mode {}", e.0)))
}
