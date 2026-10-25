use super::call::{self, is_omitted};
use super::frame::{Frame, MethodRef, code_of};
use super::{ExecState, fatal, handler, internal, iter, member};
use crate::error::info::CallKind;
use crate::error::{Exception, FatalKind, HostError};
use crate::host::{ExecFlags, ModuleInstanceRef};
use crate::object::CallError;
use crate::ops::{self, BinOp, CmpOp};
use crate::runtime::Execution;
use crate::value::Value;
use bsl_compiler::AppGlobal;
use bsl_compiler::ir::{Const, GlobalRef};
use bsl_compiler::{CompiledModule, Op};
use bsl_prim::Str;
use std::sync::Arc;

const BUDGET_TEXT: &str = "<instruction budget exhausted>";

impl Execution {
    pub fn charge(&mut self, units: u64) -> Result<(), Exception> {
        let total = self.exec.steps.saturating_add(units);
        if self.opts.budget.is_some_and(|b| total > b) {
            return Err(fatal(FatalKind::Budget, BUDGET_TEXT.into()));
        }
        self.exec.steps = total;
        Ok(())
    }
}

fn const_value(c: &Const) -> Value {
    match c {
        Const::Number(n) => Value::Number(n.clone()),
        Const::String(s) => Value::String(Str::from_units(s)),
        Const::Date(d) => Value::Date(*d),
        Const::Bool(b) => Value::Bool(*b),
        Const::Undefined => Value::Undefined,
        Const::Null => Value::Null,
    }
}

pub(crate) fn pop(st: &mut ExecState, sp_base: usize) -> Result<Value, Exception> {
    if st.stack.len() > sp_base
        && let Some(v) = st.stack.pop()
    {
        return Ok(v);
    }
    Err(internal("operand stack underflow"))
}

fn unsupported(op: Op) -> Exception {
    internal(format!("unsupported op {op:?}"))
}

fn store_global(
    ex: &mut Execution,
    module: &CompiledModule,
    sp_base: usize,
    i: u32,
) -> Result<(), Exception> {
    pop(&mut ex.exec, sp_base)?;
    let g = module
        .globals
        .get(i as usize)
        .ok_or_else(|| internal("global out of range"))?;
    let catalog = ex.runtime().registry().catalog;
    let name: String = match g {
        GlobalRef::PlatformProperty(p) => catalog
            .global_properties()
            .get(usize::from(*p))
            .map(|p| p.name_ru.to_string()),
        GlobalRef::SystemEnum(t) => catalog
            .types()
            .get(usize::from(t.0))
            .map(|t| t.name_ru.to_string()),
        GlobalRef::App(AppGlobal::CommonModule { name }) => Some(name.clone()),
    }
    .ok_or_else(|| internal("global out of range"))?;
    Err(ex.vm_error("bsl:IDS_RTE_PROP_NONWRITABLE", Some(&name)))
}

fn handler_op(ex: &mut Execution, name: &str) -> Exception {
    let found = ex.runtime().registry().catalog.find_global_method(name);
    let builtin = found.is_some_and(|m| m.builtin);
    let shown = found
        .map(|m| match ex.script_variant() {
            platform_catalog::messages::Lang::Ru => m.info.name_ru.clone(),
            platform_catalog::messages::Lang::En => m.info.name_en.clone(),
        })
        .unwrap_or_else(|| name.to_owned());
    let r = Err(CallError::Host(HostError::not_implemented(
        "Глобальный контекст",
        name,
    )));
    match ex.finish(r, CallKind::Global { builtin }, &shown) {
        Err(e) => e,
        Ok(_) => internal("handler op"),
    }
}

pub(crate) fn push_frame(
    ex: &mut Execution,
    module: Arc<CompiledModule>,
    inst: Option<ModuleInstanceRef>,
    method: MethodRef,
    argc: usize,
    flags: ExecFlags,
) -> Result<(), Exception> {
    let weight = ex.opts.weights.call;
    push_frame_weighted(ex, module, inst, method, argc, flags, weight)
}

fn push_frame_weighted(
    ex: &mut Execution,
    module: Arc<CompiledModule>,
    inst: Option<ModuleInstanceRef>,
    method: MethodRef,
    argc: usize,
    flags: ExecFlags,
    weight: u32,
) -> Result<(), Exception> {
    let entry = std::mem::take(&mut ex.exec.entry_weight);
    let own = ex.exec.frame_weight.take().unwrap_or(u64::from(weight));
    let used = ex.exec.frames.last().map_or(0, |f| f.used) + entry + own;
    if used > ex.opts.stack_limit || ex.exec.frames.len() >= ex.opts.max_depth as usize {
        return Err(ex.stack_overflow());
    }
    let code = code_of(&module, method).ok_or_else(|| internal("no such method"))?;
    if code.code.is_empty() {
        return Err(internal("method has no code"));
    }
    let nslots = code.slot_names.len();
    let nparams = code.params.len();
    if argc > nparams || nparams > nslots {
        return Err(internal("wrong argument count"));
    }
    let base = ex
        .exec
        .stack
        .len()
        .checked_sub(argc)
        .ok_or_else(|| internal("operand stack underflow"))?;
    let sp_base = base + nslots;
    ex.exec.stack.resize(sp_base, Value::Undefined);
    for (i, p) in code.params.iter().enumerate() {
        let slot = &mut ex.exec.stack[base + i];
        if i >= argc || is_omitted(slot) {
            *slot = p
                .default
                .and_then(|d| module.consts.get(d as usize))
                .map_or(Value::Undefined, const_value);
        } else if p.by_val
            && let Some(r) = call::arg_ref(slot)
        {
            *slot = r.init.clone();
        }
    }
    ex.exec.frames.push(Frame {
        method,
        home: module.clone(),
        module,
        inst,
        base,
        sp_base,
        pc: 0,
        handler_exc: None,
        handlers: Vec::new(),
        flags,
        outer: None,
        used,
    });
    Ok(())
}

pub(crate) fn push_fragment(
    ex: &mut Execution,
    module: Arc<CompiledModule>,
    inst: Option<ModuleInstanceRef>,
    home: Arc<CompiledModule>,
    flags: ExecFlags,
    caller: Option<usize>,
    expression: bool,
) -> Result<(), Exception> {
    let w = &ex.opts.weights;
    let weight = if expression { w.eval } else { w.execute };
    push_frame_weighted(ex, module, inst, MethodRef::Fragment, 0, flags, weight)?;
    if let Some(f) = ex.exec.frames.last_mut() {
        f.outer = caller;
        f.home = home;
    }
    Ok(())
}

pub(crate) fn run_until_return(ex: &mut Execution, floor: usize) -> Result<Value, Exception> {
    run_frames(ex, floor, false).map(|(v, _)| v)
}

pub(crate) fn run_frames(
    ex: &mut Execution,
    floor: usize,
    snapshot: bool,
) -> Result<(Value, Vec<Value>), Exception> {
    let r = if ex.exec.nesting >= ex.opts.max_native_depth {
        Err(ex.stack_overflow())
    } else {
        ex.exec.nesting += 1;
        let r = run_loop(ex, floor, snapshot);
        ex.exec.nesting -= 1;
        r
    };
    if r.is_err() {
        if let Some(f) = ex.exec.frames.get(floor) {
            let base = f.base;
            ex.exec.stack.truncate(base);
        }
        ex.exec.frames.truncate(floor);
    }
    r
}

fn run_loop(
    ex: &mut Execution,
    floor: usize,
    snapshot: bool,
) -> Result<(Value, Vec<Value>), Exception> {
    loop {
        match run_ops(ex, floor, snapshot) {
            Err(Exception::Catchable(info)) => {
                if !handler::catch(ex, floor, &info) {
                    return Err(Exception::Catchable(info));
                }
            }
            r => return r,
        }
    }
}

fn run_ops(
    ex: &mut Execution,
    floor: usize,
    snapshot: bool,
) -> Result<(Value, Vec<Value>), Exception> {
    'frame: loop {
        if ex.exec.frames.len() <= floor {
            return Err(internal("no frame to run"));
        }
        let fi = ex.exec.frames.len() - 1;
        let (module, inst, method, base, sp_base, mut pc, flags) = {
            let f = &ex.exec.frames[fi];
            (
                f.module.clone(),
                f.inst.clone(),
                f.method,
                f.base,
                f.sp_base,
                f.pc,
                f.flags,
            )
        };
        let code = code_of(&module, method).ok_or_else(|| internal("no such method"))?;
        loop {
            let Some(&op) = code.code.get(pc as usize) else {
                return Err(internal("jump or flow out of the code"));
            };
            ex.charge(1)?;
            ex.exec.frames[fi].pc = pc;
            pc += 1;
            macro_rules! pop {
                () => {
                    pop(&mut ex.exec, sp_base)?
                };
            }
            macro_rules! push {
                ($v:expr) => {{
                    let v = $v;
                    ex.exec.stack.push(v);
                }};
            }
            macro_rules! slot {
                ($i:expr) => {{
                    let abs = base + $i as usize;
                    if abs >= sp_base {
                        return Err(internal("slot out of range"));
                    }
                    abs
                }};
            }
            macro_rules! binary {
                ($op:expr) => {{
                    let b = pop!();
                    let a = pop!();
                    let r = ops::binary(ex, $op, &a, &b)?;
                    push!(r);
                }};
            }
            macro_rules! compare {
                ($op:expr) => {{
                    let b = pop!();
                    let a = pop!();
                    let r = ops::compare(ex, $op, &a, &b)?;
                    push!(Value::Bool(r));
                }};
            }
            match op {
                Op::Const(i) => {
                    let c = module
                        .consts
                        .get(i as usize)
                        .ok_or_else(|| internal("constant out of range"))?;
                    push!(const_value(c));
                }
                Op::Undef => push!(Value::Undefined),
                Op::Null => push!(Value::Null),
                Op::True => push!(Value::Bool(true)),
                Op::False => push!(Value::Bool(false)),
                Op::Omitted => push!(call::omitted()),
                Op::Pop => {
                    pop!();
                }
                Op::Dup => {
                    let v = match ex.exec.stack.last() {
                        Some(v) if ex.exec.stack.len() > sp_base => v.clone(),
                        _ => return Err(internal("operand stack underflow")),
                    };
                    push!(v);
                }
                Op::LoadLocal(i) => {
                    let abs = slot!(i);
                    let v = call::load_slot(ex, abs)?;
                    push!(v);
                }
                Op::RefLocal(i) => {
                    let abs = slot!(i);
                    let v = call::ref_slot(ex, abs)?;
                    push!(v);
                }
                Op::StoreLocal(i) => {
                    let v = pop!();
                    let abs = slot!(i);
                    call::store_slot(ex, abs, v)?;
                }
                Op::RefModVar(i) => member::ref_mod_var(ex, inst.as_ref(), i)?,
                Op::LoadModVar(i) => {
                    let inst = inst
                        .as_ref()
                        .ok_or_else(|| internal("no module instance"))?;
                    let v = inst
                        .0
                        .vars
                        .borrow()
                        .get(i as usize)
                        .cloned()
                        .ok_or_else(|| internal("module variable out of range"))?;
                    push!(v);
                }
                Op::StoreModVar(i) => {
                    let v = pop!();
                    let inst = inst
                        .as_ref()
                        .ok_or_else(|| internal("no module instance"))?;
                    let old = {
                        let mut vars = inst.0.vars.borrow_mut();
                        let slot = vars
                            .get_mut(i as usize)
                            .ok_or_else(|| internal("module variable out of range"))?;
                        std::mem::replace(slot, v)
                    };
                    drop(old);
                }
                Op::LoadGlobal(i) => {
                    let g = module
                        .globals
                        .get(i as usize)
                        .ok_or_else(|| internal("global out of range"))?;
                    let v = match g {
                        GlobalRef::PlatformProperty(p) => {
                            let name = ex
                                .runtime()
                                .registry()
                                .catalog
                                .global_properties()
                                .get(usize::from(*p))
                                .map(|p| p.name_ru.clone())
                                .ok_or_else(|| internal("global property out of range"))?;
                            ex.global_property(&name)?
                        }
                        GlobalRef::SystemEnum(t) => ex.system_enum(*t)?,
                        GlobalRef::App(g) => ex.app_global(g)?,
                    };
                    push!(v);
                }
                Op::Neg => {
                    let a = pop!();
                    let r = ops::neg(ex, &a)?;
                    push!(r);
                }
                Op::Pos => {
                    let a = pop!();
                    let n = ops::to_number(ex, &a)?;
                    push!(Value::Number(n));
                }
                Op::Not => {
                    let a = pop!();
                    let b = ops::to_bool(ex, &a)?;
                    push!(Value::Bool(!b));
                }
                Op::ToBool => {
                    let a = pop!();
                    let b = ops::to_bool(ex, &a)?;
                    push!(Value::Bool(b));
                }
                Op::Add => binary!(BinOp::Add),
                Op::Sub => binary!(BinOp::Sub),
                Op::Mul => binary!(BinOp::Mul),
                Op::Div => binary!(BinOp::Div),
                Op::Mod => binary!(BinOp::Mod),
                Op::Eq | Op::Ne => {
                    let b = pop!();
                    let a = pop!();
                    ops::charge_equal(ex, &a, &b)?;
                    let eq = ops::equal(&a, &b);
                    push!(Value::Bool(eq == matches!(op, Op::Eq)));
                }
                Op::Lt => compare!(CmpOp::Lt),
                Op::Le => compare!(CmpOp::Le),
                Op::Gt => compare!(CmpOp::Gt),
                Op::Ge => compare!(CmpOp::Ge),
                Op::Jump(t) => pc = t,
                Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                    let v = pop!();
                    if ops::to_bool(ex, &v)? == matches!(op, Op::JumpIfTrue(_)) {
                        pc = t;
                    }
                }
                Op::CallLocal { method, argc, .. } => {
                    let argc = argc as usize;
                    if ex.exec.stack.len() < sp_base + argc {
                        return Err(internal("operand stack underflow"));
                    }
                    let target = ex.exec.frames[fi].home.clone();
                    push_frame(
                        ex,
                        target,
                        inst.clone(),
                        MethodRef::Method(method),
                        argc,
                        flags,
                    )?;
                    continue 'frame;
                }
                Op::Return | Op::ReturnValue => {
                    let v = if matches!(op, Op::ReturnValue) {
                        pop!()
                    } else {
                        Value::Undefined
                    };
                    let slots = if snapshot && fi == floor {
                        ex.exec.stack[base..sp_base].to_vec()
                    } else {
                        Vec::new()
                    };
                    ex.exec.stack.truncate(base);
                    ex.exec.frames.pop();
                    if fi == floor {
                        return Ok((v, slots));
                    }
                    let executed = matches!(method, MethodRef::Fragment) && {
                        let c = &ex.exec.frames[fi - 1];
                        code_of(&c.module, c.method)
                            .and_then(|k| k.code.get(c.pc as usize))
                            .is_some_and(|o| matches!(o, Op::Execute))
                    };
                    if !executed {
                        push!(v);
                    }
                    ex.exec.frames[fi - 1].pc += 1;
                    continue 'frame;
                }
                Op::CallApp { method, argc, .. } => {
                    call::call_app(ex, &module, sp_base, method, argc, flags)?;
                    continue 'frame;
                }
                Op::CallPlatform { method, argc, .. } => {
                    member::call_platform(ex, sp_base, method, argc)?;
                }
                Op::CallContext { name, argc, want } => {
                    member::call_context(ex, &module, inst.as_ref(), sp_base, name, argc, want)?;
                }
                Op::CallMember { name, argc, want } => {
                    if member::call_member(ex, &module, sp_base, name, argc, want, flags)? {
                        continue 'frame;
                    }
                }
                Op::New { ty, argc } => member::new_object(ex, sp_base, ty, argc)?,
                Op::NewNoArgs { ty } => member::new_object(ex, sp_base, ty, 0)?,
                Op::NewDynamic { argc } => member::new_dynamic(ex, sp_base, argc)?,
                Op::GetMember(i) => member::get_member(ex, &module, sp_base, i)?,
                Op::SetMember(i) => member::set_member(ex, &module, sp_base, i)?,
                Op::GetIndex => member::get_index(ex, sp_base)?,
                Op::SetIndex => member::set_index(ex, sp_base)?,
                Op::RefMember(i) => member::ref_member(ex, &module, sp_base, i)?,
                Op::RefIndex => member::ref_index(ex, sp_base)?,
                Op::LoadContext(i) => member::load_context(ex, &module, inst.as_ref(), i)?,
                Op::StoreContext(i) => {
                    member::store_context(ex, &module, inst.as_ref(), sp_base, i)?;
                }
                Op::StoreGlobal(i) => store_global(ex, &module, sp_base, i)?,
                Op::IterStart(slot) => iter::start(ex, base, sp_base, slot)?,
                Op::IterNext { slot, end } => {
                    if !iter::next(ex, base, sp_base, slot)? {
                        pc = end;
                    }
                }
                Op::Raise => return Err(handler::raise(ex, sp_base)),
                Op::RaiseExt(n) => return Err(handler::raise_ext(ex, sp_base, n)),
                Op::Reraise => return Err(handler::reraise(ex)),
                Op::EnterHandler => handler::enter(ex)?,
                Op::LeaveHandler(n) => handler::leave(ex, n),
                Op::Execute | Op::Evaluate => {
                    crate::fragment::start(ex, sp_base, matches!(op, Op::Evaluate))?;
                    continue 'frame;
                }
                Op::LoadOuter(i) => {
                    let abs = crate::fragment::outer_slot(ex, fi, i as usize)?;
                    let v = call::load_slot(ex, abs)?;
                    push!(v);
                }
                Op::StoreOuter(i) => {
                    let v = pop!();
                    let abs = crate::fragment::outer_slot(ex, fi, i as usize)?;
                    call::store_slot(ex, abs, v)?;
                }
                Op::RefOuter(i) => {
                    let abs = crate::fragment::outer_slot(ex, fi, i as usize)?;
                    let v = call::ref_slot(ex, abs)?;
                    push!(v);
                }
                Op::AddHandler => return Err(handler_op(ex, "ДобавитьОбработчик")),
                Op::RemoveHandler => return Err(handler_op(ex, "УдалитьОбработчик")),
                Op::Await => return Err(unsupported(op)),
            }
        }
    }
}
