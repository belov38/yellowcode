use crate::Application;
use crate::errors::{self, keys};
use crate::model::{TypeAtom, TypeSet, Uuid16};
use bsl_vm::{
    Arg, CallError, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    ModuleInstanceRef, ModuleRef, ReuseStore, SharedValue, TypeId, Value,
};
use platform_catalog::messages::Lang;
use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const HANDLER: &str = "УстановкаПараметровСеанса";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionUser {
    pub uuid: Uuid16,
    pub name: String,
    pub full_name: String,
    pub roles: Vec<Uuid16>,
    pub no_users: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    HttpService,
    BackgroundJob,
    Cli,
}

#[derive(Clone, Debug)]
pub struct SessionStart {
    pub user: SessionUser,
    pub number: u32,
    pub app: AppKind,
    pub computer: String,
    pub started_at: bsl_prim::BslDate,
}

pub trait SessionHooks {
    fn zones_set(&mut self, s: &SessionState) -> Result<(), SessionError>;
}

pub struct NoHooks;

impl SessionHooks for NoHooks {
    fn zones_set(&mut self, _: &SessionState) -> Result<(), SessionError> {
        Ok(())
    }
}

#[derive(Debug)]
pub enum SessionError {
    GlobalModule(Exception),
    SessionModule(Exception),
    Hook(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::GlobalModule(e) => write!(f, "global module: {}", exception_text(e)),
            SessionError::SessionModule(e) => write!(f, "session module: {}", exception_text(e)),
            SessionError::Hook(h) => write!(f, "hook: {h}"),
        }
    }
}

impl std::error::Error for SessionError {}

fn exception_text(e: &Exception) -> String {
    match e {
        Exception::Fatal(f) => f.text.to_string(),
        Exception::Catchable(i) => i.description.to_string_lossy().to_string(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobFlags {
    pub safe_mode: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallOptions {
    pub safe_mode_disabled: bool,
    pub job: Option<JobFlags>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallOutcome {
    pub rolled_back: u32,
}

pub struct SessionState {
    user: SessionUser,
    number: u32,
    app: AppKind,
    computer: String,
    started_at: bsl_prim::BslDate,
    pub(crate) params: Vec<Option<SharedValue>>,
    pub(crate) reuse: ReuseStore,
    pub(crate) presentations: crate::present::PresentationCache,
}

impl std::fmt::Debug for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionState")
            .field("number", &self.number)
            .field("user", &self.user.name)
            .finish_non_exhaustive()
    }
}

impl SessionState {
    pub(crate) fn new(s: SessionStart, params: usize) -> SessionState {
        SessionState {
            user: s.user,
            number: s.number,
            app: s.app,
            computer: s.computer,
            started_at: s.started_at,
            params: vec![None; params],
            reuse: ReuseStore::default(),
            presentations: crate::present::PresentationCache::default(),
        }
    }

    pub(crate) fn detached() -> SessionState {
        SessionState::new(
            SessionStart {
                user: SessionUser {
                    uuid: Uuid16::default(),
                    name: String::new(),
                    full_name: String::new(),
                    roles: Vec::new(),
                    no_users: true,
                },
                number: 0,
                app: AppKind::Cli,
                computer: String::new(),
                started_at: bsl_prim::BslDate::EMPTY,
            },
            0,
        )
    }

    pub fn number(&self) -> u32 {
        self.number
    }

    pub fn user(&self) -> &SessionUser {
        &self.user
    }

    pub fn app(&self) -> AppKind {
        self.app
    }

    pub fn computer(&self) -> &str {
        &self.computer
    }

    pub fn started_at(&self) -> bsl_prim::BslDate {
        self.started_at
    }
}

#[derive(Clone)]
pub(crate) enum ParamSlot {
    Unset,
    Running,
    Set(Value),
}

pub(crate) struct CallContext {
    pub sess: SessionState,
    pub params: Vec<ParamSlot>,
    pub common: HashMap<ModuleRef, ModuleInstanceRef>,
    pub md_cache: HashMap<crate::meta::MdKey, bsl_vm::ObjRef>,
    pub mgr_cache: HashMap<crate::managers::MgrKey, bsl_vm::ObjRef>,
    pub read_cache: HashMap<crate::values::refs::RefValue, (u64, Rc<crate::read::ReadRow>)>,
    pub selections: Vec<std::rc::Weak<crate::select::SelectionObject>>,
    #[allow(dead_code)]
    pub opts: CallOptions,
}

pub const READ_CACHE_CAP: usize = 2048;

#[doc(hidden)]
pub fn read_cache_len(ex: &mut Execution) -> usize {
    call_ctx(ex).map_or(0, |c| c.read_cache.len())
}

#[doc(hidden)]
pub fn selections_len(ex: &mut Execution) -> usize {
    call_ctx(ex).map_or(0, |c| c.selections.len())
}

pub(crate) fn call_ctx(ex: &mut Execution) -> Option<&mut CallContext> {
    ex.host_data()?.downcast_mut::<CallContext>()
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn l5(key: &'static str) -> HostError {
    match errors::msg(key) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(key),
    }
}

fn l5_args(key: &'static str, args: &[&str]) -> HostError {
    HostError::text(
        &errors::render(key, args, Lang::Ru),
        &errors::render(key, args, Lang::En),
    )
}

fn type_mismatch() -> HostError {
    let mut e = l5(keys::TYPE_MISMATCH);
    e.categories = Some(bsl_vm::CategorySet::of(&[
        bsl_vm::Category::ScriptUseError,
        bsl_vm::Category::OtherError,
    ]));
    e
}

pub(crate) fn accepts(ts: &TypeSet, v: &Value) -> bool {
    if ts.atoms.is_empty() {
        return true;
    }
    if matches!(v, Value::Undefined) && ts.atoms.len() > 1 {
        return true;
    }
    ts.atoms.iter().any(|a| match (a, v) {
        (TypeAtom::Undefined, Value::Undefined) => true,
        (TypeAtom::Null, Value::Null) => true,
        (TypeAtom::Bool, Value::Bool(_)) => true,
        (TypeAtom::Number, Value::Number(n)) => number_fits(ts, n),
        (TypeAtom::Date, Value::Date(d)) => date_fits(ts, *d),
        (TypeAtom::String, Value::String(s)) => match &ts.string {
            Some(q) if q.length > 0 => {
                let len = s.units().len();
                if q.fixed {
                    len == q.length as usize
                } else {
                    len <= q.length as usize
                }
            }
            _ => true,
        },
        (
            TypeAtom::Uuid
            | TypeAtom::ValueStorage
            | TypeAtom::Binary
            | TypeAtom::Ref(_)
            | TypeAtom::AnyRef(_)
            | TypeAtom::Platform(_),
            Value::Object(_),
        ) => true,
        _ => false,
    })
}

fn number_fits(ts: &TypeSet, n: &bsl_prim::Number) -> bool {
    let Some(q) = &ts.number else {
        return true;
    };
    if q.nonnegative && n.is_negative() && !n.is_zero() {
        return false;
    }
    if q.digits == 0 {
        return true;
    }
    let int = n.int_digits();
    let int_len = if int == "0" { 0 } else { int.len() };
    let frac_len = n.frac_digits().len();
    frac_len <= q.fraction as usize && int_len <= q.digits.saturating_sub(q.fraction) as usize
}

fn date_fits(ts: &TypeSet, d: bsl_prim::BslDate) -> bool {
    let p = d.parts();
    match ts.date {
        Some(md_model::DateFractions::Date) => p.hour == 0 && p.minute == 0 && p.second == 0,
        Some(md_model::DateFractions::Time) => p.year == 1 && p.month == 1 && p.day == 1,
        _ => true,
    }
}

pub(crate) struct SessionParams {
    pub app: Arc<Application>,
    pub ty: TypeId,
}

impl SessionParams {
    fn index_of(&self, name: &str) -> Option<usize> {
        self.app.model().session_param(name).map(|(i, _)| i)
    }

    fn name_of(&self, i: usize) -> String {
        self.app
            .model()
            .session_params
            .get(i)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    fn slot(&self, ex: &mut Execution, i: usize) -> CallResult<Option<Value>> {
        let c = call_ctx(ex).ok_or_else(|| internal("session parameters outside of a call"))?;
        Ok(match c.params.get(i) {
            Some(ParamSlot::Set(v)) => Some(v.clone()),
            _ => None,
        })
    }

    fn running(&self, ex: &mut Execution, i: usize) -> bool {
        call_ctx(ex).is_some_and(|c| matches!(c.params.get(i), Some(ParamSlot::Running)))
    }

    fn mark(&self, ex: &mut Execution, i: usize, running: bool) {
        if let Some(c) = call_ctx(ex)
            && let Some(s) = c.params.get_mut(i)
        {
            match (running, &*s) {
                (true, ParamSlot::Unset) => *s = ParamSlot::Running,
                (false, ParamSlot::Running) => *s = ParamSlot::Unset,
                _ => {}
            }
        }
    }

    fn read(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        if let Some(v) = self.slot(ex, i)? {
            return Ok(v);
        }
        if self.running(ex, i) {
            return Err(l5(keys::SESSION_PARAM_NOT_INITIALIZED).into());
        }
        self.mark(ex, i, true);
        let r = self.run_handler(ex, Some(&self.name_of(i)));
        self.mark(ex, i, false);
        r?;
        match self.slot(ex, i)? {
            Some(v) => Ok(v),
            None => Err(l5(keys::SESSION_PARAM_NOT_INITIALIZED).into()),
        }
    }

    fn run_handler(&self, ex: &mut Execution, name: Option<&str>) -> Result<(), Exception> {
        let Some(m) = self.app.modules().session_module() else {
            return Ok(());
        };
        let inst = self.app.instance_in_call(ex, m)?;
        if inst.module().method(HANDLER).is_none() {
            return Ok(());
        }
        let arg = match name {
            Some(n) => bsl_stdlib::new_array(vec![Value::from(n)]),
            None => Value::Undefined,
        };
        ex.call_privileged(&inst, HANDLER, &mut [Arg::Val(arg)])
            .map(drop)
    }

    fn write(&self, ex: &mut Execution, i: usize, v: Value) -> CallResult<()> {
        let ok = self
            .app
            .model()
            .session_params
            .get(i)
            .is_some_and(|p| accepts(&p.types, &v));
        if !ok {
            return Err(type_mismatch().into());
        }
        let c = call_ctx(ex).ok_or_else(|| internal("session parameters outside of a call"))?;
        if let Some(s) = c.params.get_mut(i) {
            *s = ParamSlot::Set(v);
        }
        Ok(())
    }

    fn key_index(&self, key: &Value) -> CallResult<usize> {
        let Value::String(s) = key else {
            return Err(HostError::key("bsl:IDS_RTE_NOTINDEXED").into());
        };
        self.index_of(&s.to_string_lossy()).ok_or_else(|| {
            HostError::key("bsl:IDS_RTE_VALPROP_NOTFOUND")
                .with_arg(s.to_string_lossy())
                .into()
        })
    }

    fn clear(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let names: Option<Vec<Value>> = match args.first() {
            None | Some(Arg::Omitted) => None,
            Some(a) => {
                let v = match a {
                    Arg::Val(v) => v.clone(),
                    Arg::Place(p) => p.get(ex)?,
                    Arg::Omitted => Value::Undefined,
                };
                match v {
                    Value::Undefined => None,
                    v if is_plain_array(&v) => {
                        Some(bsl_stdlib::array_items(&v).ok_or_else(parameter_mismatch)?)
                    }
                    _ => return Err(parameter_mismatch().into()),
                }
            }
        };
        let mut indexes = Vec::new();
        match names {
            None => indexes.extend(0..self.app.model().session_params.len()),
            Some(items) => {
                for item in &items {
                    let Value::String(s) = item else {
                        return Err(parameter_mismatch().into());
                    };
                    indexes.extend(self.index_of(&s.to_string_lossy()));
                }
            }
        }
        let c = call_ctx(ex).ok_or_else(|| internal("session parameters outside of a call"))?;
        for i in indexes {
            if let Some(s) = c.params.get_mut(i) {
                *s = ParamSlot::Unset;
            }
        }
        Ok(Value::Undefined)
    }
}

fn key_text(key: &Value) -> String {
    match key {
        Value::String(s) => s.to_string_lossy(),
        _ => String::new(),
    }
}

fn is_plain_array(v: &Value) -> bool {
    let catalog = platform_catalog::catalog();
    catalog
        .find_type("Массив")
        .is_some_and(|idx| v.type_id() == TypeId::of_catalog(idx))
}

pub(crate) fn wrap_error(key: &str, name: &str, mut inner: HostError) -> HostError {
    let text = |lang| {
        let mut t = platform_catalog::messages::render_key(key, &[], "", lang).unwrap_or_default();
        t.push_str(" (");
        t.push_str(name);
        t.push(')');
        t
    };
    if inner.categories.is_none() {
        inner.categories = Some(bsl_vm::CategorySet::of(&[bsl_vm::Category::ScriptUseError]));
    }
    let mut top = HostError::text(&text(Lang::Ru), &text(Lang::En))
        .raw()
        .caused_by(bsl_vm::Cause::Host(Box::new(inner)));
    top.categories = Some(bsl_vm::CategorySet::of(&[
        bsl_vm::Category::ScriptRuntimeError,
    ]));
    top
}

pub(crate) fn wrap_method(name: &str, e: HostError) -> HostError {
    if e.raw {
        return e;
    }
    wrap_error("bsl:IDS_RTE_CONTEXTCALL", name, e)
}

fn wrap_attr(get: bool, name: &str, inner: HostError) -> HostError {
    let key = if get {
        "bsl:IDS_RTE_CONTEXTGETPROPVAL"
    } else {
        "bsl:IDS_RTE_CONTEXTSETPROPVAL"
    };
    wrap_error(key, name, inner)
}

fn parameter_mismatch() -> HostError {
    l5_args(keys::PARAMETER_TYPE_MISMATCH, &["1"])
}

impl HostObject for SessionParams {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.read(ex, m.0 as usize)
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        self.write(ex, m.0 as usize, v)
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let clear = ex
            .runtime()
            .types()
            .member(self.ty, "Очистить")
            .map(|(id, _)| id);
        if clear == Some(m) {
            let name = match self.app.model().script {
                md_model::ScriptVariant::Russian => "Очистить",
                md_model::ScriptVariant::English => "Clear",
            };
            return self.clear(ex, args).map_err(|e| match e {
                CallError::Host(h) => wrap_method(name, h).into(),
                e => e,
            });
        }
        Err(CallError::Host(internal(
            "unknown method of session parameters",
        )))
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.index_of(name).map(|i| MemberId(i as u32))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        ((m.0 as usize) < self.app.model().session_params.len()).then_some(MemberKind::Property {
            readable: true,
            writable: true,
        })
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(bsl_prim::Str::from(match self.app.model().script {
            md_model::ScriptVariant::Russian => "ПараметрыСеанса",
            md_model::ScriptVariant::English => "SessionParameters",
        })))
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = self.key_index(key)?;
        self.read(ex, i).map_err(|e| match e {
            CallError::Host(h) => wrap_attr(true, &key_text(key), h).into(),
            e => e,
        })
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        let i = self.key_index(key)?;
        self.write(ex, i, v).map_err(|e| match e {
            CallError::Host(h) => wrap_attr(false, &key_text(key), h).into(),
            e => e,
        })
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.app.model().session_params.len())
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.read(ex, i)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().is::<SessionParams>())
    }
}

pub(crate) fn session_params_value(app: &Arc<Application>, ty: TypeId) -> Value {
    Value::Object(Rc::new(SessionParams {
        app: app.clone(),
        ty,
    }))
}
