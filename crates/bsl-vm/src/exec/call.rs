use super::internal;
use crate::error::{Exception, HostError};
use crate::host::{ExecFlags, ModuleInstanceRef};
use crate::object::{CallError, CallResult, HostObject};
use crate::place::{Arg, Place};
use crate::runtime::Execution;
use crate::types::{MemberId, TypeId};
use crate::value::Value;
use std::any::Any;
use std::rc::Rc;

pub(super) struct OmittedArg;

pub(super) struct ArgRef {
    pub place: Place,
    pub init: Value,
}

fn stray() -> CallError {
    CallError::Host(HostError::text("Пропущенный аргумент", "Omitted argument"))
}

macro_rules! marker_object {
    ($t:ty) => {
        impl HostObject for $t {
            fn type_id(&self) -> TypeId {
                TypeId::UNDEFINED
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
                Err(stray())
            }
            fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
                Err(stray())
            }
            fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
                Err(stray())
            }
        }
    };
}
marker_object!(OmittedArg);
marker_object!(ArgRef);

pub(super) fn omitted() -> Value {
    Value::Object(Rc::new(OmittedArg))
}

pub(crate) fn is_omitted(v: &Value) -> bool {
    matches!(v, Value::Object(o) if o.as_any().is::<OmittedArg>())
}

pub(super) fn arg_ref(v: &Value) -> Option<&ArgRef> {
    match v {
        Value::Object(o) => o.as_any().downcast_ref::<ArgRef>(),
        _ => None,
    }
}

pub(super) fn new_ref(place: Place, init: Value) -> Value {
    Value::Object(Rc::new(ArgRef { place, init }))
}

pub(super) fn load_slot(ex: &mut Execution, abs: usize) -> Result<Value, Exception> {
    let v = ex
        .exec
        .stack
        .get(abs)
        .ok_or_else(|| internal("slot out of range"))?
        .clone();
    match arg_ref(&v) {
        Some(r) => {
            let place = r.place.clone();
            place.get(ex)
        }
        None => Ok(v),
    }
}

pub(super) fn store_slot(ex: &mut Execution, abs: usize, v: Value) -> Result<(), Exception> {
    let slot = ex
        .exec
        .stack
        .get_mut(abs)
        .ok_or_else(|| internal("slot out of range"))?;
    if let Some(r) = arg_ref(slot) {
        let place = r.place.clone();
        return place.set(ex, v);
    }
    let old = std::mem::replace(slot, v);
    drop(old);
    Ok(())
}

pub(super) fn ref_slot(ex: &mut Execution, abs: usize) -> Result<Value, Exception> {
    let v = ex
        .exec
        .stack
        .get(abs)
        .ok_or_else(|| internal("slot out of range"))?
        .clone();
    match arg_ref(&v) {
        Some(r) => {
            let place = r.place.clone();
            let init = place.get(ex)?;
            Ok(new_ref(place, init))
        }
        None => Ok(new_ref(Place::local(abs), v)),
    }
}

pub(super) fn pop_args(
    ex: &mut Execution,
    sp_base: usize,
    argc: usize,
) -> Result<Vec<Arg>, Exception> {
    let len = ex.exec.stack.len();
    let from = len
        .checked_sub(argc)
        .filter(|&f| f >= sp_base)
        .ok_or_else(|| internal("operand stack underflow"))?;
    Ok(ex
        .exec
        .stack
        .drain(from..)
        .map(|v| match arg_ref(&v) {
            Some(r) => Arg::Place(r.place.clone()),
            None if is_omitted(&v) => Arg::Omitted,
            None => Arg::Val(v),
        })
        .collect())
}

pub(super) fn call_app(
    ex: &mut Execution,
    module: &bsl_compiler::CompiledModule,
    sp_base: usize,
    method: u32,
    argc: u32,
    flags: crate::host::ExecFlags,
) -> Result<bool, Exception> {
    let argc = argc as usize;
    if ex.exec.stack.len() < sp_base + argc {
        return Err(internal("operand stack underflow"));
    }
    let sig = module
        .app_methods
        .get(method as usize)
        .ok_or_else(|| internal("application method out of range"))?;
    let app = ex.runtime().app().clone();
    let mref = app
        .module_ref(&sig.module)
        .ok_or_else(|| internal(format!("unknown module {}", sig.module)))?;
    let inst = app.module(ex, mref)?;
    let (idx, _) = inst
        .module()
        .method(&sig.name)
        .ok_or_else(|| internal(format!("no method {} in {}", sig.name, sig.module)))?;
    let value = push_external_frame(
        ex,
        inst.module().clone(),
        Some(inst),
        super::frame::MethodRef::Method(idx),
        argc,
        flags,
    )?;
    if let Some(value) = value {
        ex.exec.stack.push(value);
        Ok(false)
    } else {
        Ok(true)
    }
}

pub(crate) fn arg_initial(value: &Value) -> &Value {
    arg_ref(value).map_or(value, |arg| &arg.init)
}

fn push_external_frame(
    ex: &mut Execution,
    module: std::sync::Arc<bsl_compiler::CompiledModule>,
    inst: Option<ModuleInstanceRef>,
    method: super::frame::MethodRef,
    argc: usize,
    flags: ExecFlags,
) -> Result<Option<Value>, Exception> {
    let lookup = match method {
        super::frame::MethodRef::Method(index) => crate::reuse::lookup(ex, &module, index, argc),
        _ => Ok(crate::reuse::Lookup::Bypass),
    };
    let pending = match lookup {
        Ok(crate::reuse::Lookup::Hit(value)) => {
            ex.exec
                .stack
                .truncate(ex.exec.stack.len().saturating_sub(argc));
            ex.exec.entry_weight = 0;
            ex.exec.frame_weight = None;
            return Ok(Some(value));
        }
        Ok(crate::reuse::Lookup::Miss(pending)) => Some(pending),
        Ok(crate::reuse::Lookup::Bypass) => None,
        Err(error) => {
            ex.exec.entry_weight = 0;
            ex.exec.frame_weight = None;
            return Err(error);
        }
    };
    super::run::push_frame(ex, module, inst, method, argc, flags)?;
    if let Some(frame) = ex.exec.frames.last_mut() {
        frame.reuse = pending;
    }
    Ok(None)
}

pub(crate) fn push_args(ex: &mut Execution, args: &[Arg]) -> Result<(), Exception> {
    let start = ex.exec.stack.len();
    for a in args {
        let v = match a {
            Arg::Val(v) => v.clone(),
            Arg::Omitted => omitted(),
            Arg::Place(p) => match p.get(ex) {
                Ok(init) => new_ref(p.clone(), init),
                Err(e) => {
                    ex.exec.stack.truncate(start);
                    return Err(e);
                }
            },
        };
        ex.exec.stack.push(v);
    }
    Ok(())
}

pub(crate) fn check_call(
    ex: &mut Execution,
    code: &bsl_compiler::MethodCode,
    name: &str,
    argc: usize,
    want: bool,
) -> Result<(), Exception> {
    if want && code.kind == bsl_syntax::ast::MethodKind::Procedure {
        return Err(ex.vm_error(PROC_AS_FUNC, Some(name)));
    }
    if argc > code.params.len() {
        return Err(ex.vm_error(TOO_MANY, None));
    }
    if argc < crate::module::required_args(code) {
        return Err(ex.vm_error(NOT_ENOUGH, None));
    }
    Ok(())
}

pub(crate) const METHOD_NOT_FOUND: &str = "bsl:IDS_RTE_VALMETHOD_NOTFOUND";
const PROC_AS_FUNC: &str = "bsl:IDS_RTE_PROP_PROCASFUNC";
const TOO_MANY: &str = "bsl:IDS_RTE_PARAMS_TOOMANY";
const NOT_ENOUGH: &str = "bsl:IDS_RTE_PARAMS_NOTENOUGH";

#[derive(Clone, Copy)]
pub(crate) enum Reentry {
    OnTopOfFrame(u32),
    FullLevel(u32),
}

pub(crate) fn call_index(
    ex: &mut Execution,
    inst: &ModuleInstanceRef,
    idx: u32,
    args: &[Arg],
    flags: ExecFlags,
    reentry: Reentry,
) -> Result<Value, Exception> {
    let module = inst.module().clone();
    let code = module
        .methods
        .get(idx as usize)
        .ok_or_else(|| internal("method out of range"))?;
    let name = code.name.name.to_string();
    check_call(ex, code, &name, args.len(), false)?;
    let floor = ex.exec.frames.len();
    let start = ex.exec.stack.len();
    push_args(ex, args)?;
    match reentry {
        Reentry::OnTopOfFrame(w) => ex.enter_weight(w),
        Reentry::FullLevel(w) => ex.enter_full_weight(w),
    }
    match push_external_frame(
        ex,
        module,
        Some(inst.clone()),
        super::frame::MethodRef::Method(idx),
        args.len(),
        flags,
    ) {
        Ok(Some(value)) => return Ok(value),
        Ok(None) => (),
        Err(error) => {
            ex.exec.stack.truncate(start);
            return Err(error);
        }
    }
    super::run::run_until_return(ex, floor)
}

pub(super) fn enter_module_value(
    ex: &mut Execution,
    sp_base: usize,
    name: &str,
    argc: usize,
    want: bool,
    flags: ExecFlags,
) -> Result<Option<bool>, Exception> {
    let Some(pos) = ex
        .exec
        .stack
        .len()
        .checked_sub(argc + 1)
        .filter(|&p| p >= sp_base)
    else {
        return Ok(None);
    };
    let inst = match ex.exec.stack.get(pos) {
        Some(Value::Object(o)) => match o.as_any().downcast_ref::<crate::module::ModuleValue>() {
            Some(mv) => mv.instance().clone(),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    let module = inst.module().clone();
    let Some((idx, code)) = crate::module::exported(&module, name) else {
        return Err(ex.vm_error(METHOD_NOT_FOUND, Some(name)));
    };
    check_call(ex, code, name, argc, want)?;
    ex.exec.stack.remove(pos);
    let value = push_external_frame(
        ex,
        module,
        Some(inst),
        super::frame::MethodRef::Method(idx),
        argc,
        flags,
    )?;
    if let Some(value) = value {
        ex.exec.stack.push(value);
        Ok(Some(false))
    } else {
        Ok(Some(true))
    }
}
