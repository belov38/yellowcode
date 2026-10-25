use super::call::{new_ref, pop_args};
use super::internal;
use crate::error::{Exception, HostError};
use crate::host::{ModuleContext, ModuleInstanceRef};
use crate::place::{Arg, Place};
use crate::runtime::Execution;
use crate::value::Value;
use bsl_compiler::CompiledModule;
use platform_catalog::TypeIdx;

const NOT_ENUM: &str = "bsl:IDS_RTE_NOTENUM";
const TYPE_NOT_DEFINED: &str = "bsl:IDS_RTE_TYPENOTDEFINED";

fn pop(ex: &mut Execution, sp_base: usize) -> Result<Value, Exception> {
    super::run::pop(&mut ex.exec, sp_base)
}

fn name(module: &CompiledModule, i: u32) -> Result<std::sync::Arc<str>, Exception> {
    module
        .syms
        .get(i as usize)
        .map(|s| s.name.clone())
        .ok_or_else(|| internal("name out of range"))
}

pub(super) fn get_member(
    ex: &mut Execution,
    module: &CompiledModule,
    sp_base: usize,
    i: u32,
) -> Result<(), Exception> {
    let obj = pop(ex, sp_base)?;
    let v = ex.get_member(&obj, &name(module, i)?)?;
    ex.exec.stack.push(v);
    Ok(())
}

pub(super) fn set_member(
    ex: &mut Execution,
    module: &CompiledModule,
    sp_base: usize,
    i: u32,
) -> Result<(), Exception> {
    let v = pop(ex, sp_base)?;
    let obj = pop(ex, sp_base)?;
    ex.set_member(&obj, &name(module, i)?, v)
}

pub(super) fn get_index(ex: &mut Execution, sp_base: usize) -> Result<(), Exception> {
    let key = pop(ex, sp_base)?;
    let obj = pop(ex, sp_base)?;
    let v = ex.index_get(&obj, &key)?;
    ex.exec.stack.push(v);
    Ok(())
}

pub(super) fn set_index(ex: &mut Execution, sp_base: usize) -> Result<(), Exception> {
    let v = pop(ex, sp_base)?;
    let key = pop(ex, sp_base)?;
    let obj = pop(ex, sp_base)?;
    ex.index_set(&obj, &key, v)
}

pub(super) fn ref_member(
    ex: &mut Execution,
    module: &CompiledModule,
    sp_base: usize,
    i: u32,
) -> Result<(), Exception> {
    let obj = pop(ex, sp_base)?;
    let name = name(module, i)?;
    let init = ex.get_member(&obj, &name)?;
    let Value::Object(o) = obj else {
        return Err(internal("member of a value that is not an object"));
    };
    ex.exec.stack.push(new_ref(Place::member(o, &name), init));
    Ok(())
}

pub(super) fn ref_index(ex: &mut Execution, sp_base: usize) -> Result<(), Exception> {
    let key = pop(ex, sp_base)?;
    let obj = pop(ex, sp_base)?;
    let init = ex.index_get(&obj, &key)?;
    let Value::Object(o) = obj else {
        return Err(internal("index of a value that is not an object"));
    };
    ex.exec.stack.push(new_ref(Place::index(o, key), init));
    Ok(())
}

pub(super) fn ref_mod_var(
    ex: &mut Execution,
    inst: Option<&ModuleInstanceRef>,
    i: u32,
) -> Result<(), Exception> {
    let inst = inst.ok_or_else(|| internal("no module instance"))?;
    let i = i as usize;
    let init = inst
        .0
        .vars
        .borrow()
        .get(i)
        .cloned()
        .ok_or_else(|| internal("module variable out of range"))?;
    ex.exec
        .stack
        .push(new_ref(Place::mod_var(inst.0.clone(), i), init));
    Ok(())
}

pub(super) fn call_member(
    ex: &mut Execution,
    module: &CompiledModule,
    sp_base: usize,
    i: u32,
    argc: u32,
    want: bool,
    flags: crate::host::ExecFlags,
) -> Result<bool, Exception> {
    let n = name(module, i)?;
    if super::call::enter_module_value(ex, sp_base, &n, argc as usize, want, flags)? {
        return Ok(true);
    }
    let mut args = pop_args(ex, sp_base, argc as usize)?;
    let obj = pop(ex, sp_base)?;
    let v = ex.call_method(&obj, &n, &mut args, want)?;
    ex.exec.stack.push(v);
    Ok(false)
}

pub(super) fn call_platform(
    ex: &mut Execution,
    sp_base: usize,
    method: u16,
    argc: u32,
) -> Result<(), Exception> {
    let catalog = ex.runtime().registry().catalog;
    let m = catalog
        .global_methods()
        .get(usize::from(method))
        .ok_or_else(|| internal("global method out of range"))?;
    let mut args = pop_args(ex, sp_base, argc as usize)?;
    let v = ex.call_global(&m.info.name_ru, &mut args)?;
    ex.exec.stack.push(if m.info.returns_value {
        v
    } else {
        Value::Undefined
    });
    Ok(())
}

pub(super) fn new_object(
    ex: &mut Execution,
    sp_base: usize,
    ty: u16,
    argc: u32,
) -> Result<(), Exception> {
    let mut args = pop_args(ex, sp_base, argc as usize)?;
    let v = ex.construct(TypeIdx(ty), &mut args)?;
    ex.exec.stack.push(v);
    Ok(())
}

pub(super) fn new_dynamic(ex: &mut Execution, sp_base: usize, argc: u32) -> Result<(), Exception> {
    let args = pop_args(ex, sp_base, argc as usize)?;
    let mut it = args.into_iter();
    let value_of = |ex: &mut Execution, a: Option<Arg>| match a {
        None | Some(Arg::Omitted) => Ok(Value::Undefined),
        Some(Arg::Val(v)) => Ok(v),
        Some(Arg::Place(p)) => p.get(ex),
    };
    let first = it.next();
    let ty = value_of(ex, first)?;
    let params = value_of(ex, it.next())?;
    let type_name = match &ty {
        Value::String(s) => s.to_string_lossy(),
        Value::Type(t) => ex
            .runtime()
            .types()
            .name(*t, platform_catalog::messages::Lang::Ru)
            .to_owned(),
        other => ex.present(other)?.to_string_lossy(),
    };
    let mut ctor_args = Vec::new();
    match &params {
        Value::Undefined => {}
        Value::Object(o) => {
            let n = o
                .iter_len(ex)
                .ok_or_else(|| ex.error(HostError::key(NOT_ENUM)))?;
            for i in 0..n {
                let v = match o.iter_get(ex, i) {
                    Ok(v) => v,
                    Err(crate::object::CallError::Exc(e)) => return Err(e),
                    Err(crate::object::CallError::Host(h)) => return Err(ex.error(h)),
                };
                ctor_args.push(Arg::Val(v));
            }
        }
        _ => return Err(ex.error(HostError::key(NOT_ENUM))),
    }
    if type_name.is_empty() {
        return Err(ex.error(HostError::key(TYPE_NOT_DEFINED).with_arg("")));
    }
    let v = ex.construct_by_name(&type_name, &mut ctor_args)?;
    ex.exec.stack.push(v);
    Ok(())
}

fn context_object(inst: Option<&ModuleInstanceRef>) -> Result<Value, Exception> {
    let inst = inst.ok_or_else(|| internal("no module instance"))?;
    match inst.context() {
        ModuleContext::Object(o) | ModuleContext::Form(o) => Ok(Value::Object(o.clone())),
        ModuleContext::None => Err(internal("the module has no context object")),
    }
}

fn is_this(module: &CompiledModule, i: u32) -> bool {
    module.syms.get(i as usize).is_some_and(|s| {
        matches!(
            &*s.folded,
            "этотобъект" | "этаформа" | "thisobject" | "thisform"
        )
    })
}

pub(super) fn load_context(
    ex: &mut Execution,
    module: &CompiledModule,
    inst: Option<&ModuleInstanceRef>,
    i: u32,
) -> Result<(), Exception> {
    let ctx = context_object(inst)?;
    let v = if is_this(module, i) {
        ctx
    } else {
        ex.get_member(&ctx, &name(module, i)?)?
    };
    ex.exec.stack.push(v);
    Ok(())
}

pub(super) fn store_context(
    ex: &mut Execution,
    module: &CompiledModule,
    inst: Option<&ModuleInstanceRef>,
    sp_base: usize,
    i: u32,
) -> Result<(), Exception> {
    let v = pop(ex, sp_base)?;
    if is_this(module, i) {
        return Err(ex.vm_error("bsl:IDS_RTE_PROP_NONWRITABLE", Some("ЭтотОбъект")));
    }
    let ctx = context_object(inst)?;
    ex.set_member(&ctx, &name(module, i)?, v)
}

pub(super) fn call_context(
    ex: &mut Execution,
    module: &CompiledModule,
    inst: Option<&ModuleInstanceRef>,
    sp_base: usize,
    i: u32,
    argc: u32,
    want: bool,
) -> Result<(), Exception> {
    let ctx = context_object(inst)?;
    let mut args = pop_args(ex, sp_base, argc as usize)?;
    let v = ex.call_method(&ctx, &name(module, i)?, &mut args, want)?;
    ex.exec.stack.push(v);
    Ok(())
}
