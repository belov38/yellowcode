use super::call::pop_args;
use super::internal;
use super::run::pop;
use crate::error::info::{CallKind, from_host};
use crate::error::{Category, CategorySet, ErrorInfo, Exception, HostError, MsgRef};
use crate::lang::{self, arg_value};
use crate::place::Arg;
use crate::runtime::Execution;
use crate::value::Value;
use bsl_prim::Str;
use std::rc::Rc;

const TYPE_MISMATCH: &str = "core85:IDS_PARAMETER_N_TYPE_MISMATCH";
const VALUE_MISMATCH: &str = "core85:IDS_PARAMETER_N_VALUE_MISMATCH";
const RAISE_CALL: &str = "rt$raise";

pub(super) fn catch(ex: &mut Execution, floor: usize, info: &Rc<ErrorInfo>) -> bool {
    while ex.exec.frames.len() > floor {
        let fi = ex.exec.frames.len() - 1;
        let f = &ex.exec.frames[fi];
        let found = f.code().and_then(|c| {
            c.handlers
                .iter()
                .find(|h| h.start <= f.pc && f.pc < h.end)
                .copied()
        });
        let (sp_base, base) = (f.sp_base, f.base);
        if let Some(h) = found {
            ex.exec.stack.truncate(sp_base);
            let f = &mut ex.exec.frames[fi];
            f.handlers.truncate(h.depth as usize);
            f.handler_exc = Some(info.clone());
            f.pc = h.target;
            return true;
        }
        ex.exec.stack.truncate(base);
        ex.exec.frames.pop();
    }
    false
}

pub(super) fn enter(ex: &mut Execution) -> Result<(), Exception> {
    let f = ex
        .exec
        .frames
        .last_mut()
        .ok_or_else(|| internal("no frame"))?;
    let e = f
        .handler_exc
        .take()
        .ok_or_else(|| internal("EnterHandler without a caught exception"))?;
    f.handlers.push(e);
    Ok(())
}

pub(super) fn leave(ex: &mut Execution, n: u16) {
    if let Some(f) = ex.exec.frames.last_mut() {
        let keep = f.handlers.len().saturating_sub(usize::from(n));
        f.handlers.truncate(keep);
    }
}

pub(super) fn reraise(ex: &mut Execution) -> Exception {
    match ex.current_error() {
        Some(e) => Exception::Catchable(e),
        None => internal("ВызватьИсключение without an exception being handled"),
    }
}

pub(super) fn raise(ex: &mut Execution, sp_base: usize) -> Exception {
    raise_text(ex, sp_base).unwrap_or_else(|e| e)
}

fn raise_text(ex: &mut Execution, sp_base: usize) -> Result<Exception, Exception> {
    let v = pop(&mut ex.exec, sp_base)?;
    let text = ex.present(&v)?;
    Ok(raised(
        ex,
        text,
        CategorySet::default(),
        None,
        Str::new(),
        Value::Undefined,
    ))
}

pub(super) fn raise_ext(ex: &mut Execution, sp_base: usize, n: u32) -> Exception {
    raise_args(ex, sp_base, n).unwrap_or_else(|e| e)
}

fn raise_args(ex: &mut Execution, sp_base: usize, n: u32) -> Result<Exception, Exception> {
    let args = pop_args(ex, sp_base, n as usize)?;
    if args.len() > 5 {
        return Err(call_error(ex, HostError::key("bsl:IDS_RTE_PARAMS_TOOMANY")));
    }
    let given = |ex: &mut Execution, i: usize| match args.get(i) {
        None | Some(Arg::Omitted) => Ok(None),
        Some(a) => arg_value(ex, a).map(Some),
    };
    let text = match given(ex, 0)? {
        Some(v) => ex.present(&v)?,
        None => Str::new(),
    };
    let mut cats = CategorySet::default();
    if let Some(v) = given(ex, 1)?.filter(|v| !matches!(v, Value::Undefined)) {
        match lang::category_of(&v) {
            Some(c) if !FIXED.contains(&c) => cats = cats.with(c),
            Some(_) => return Err(call_error(ex, param_error(VALUE_MISMATCH, 2))),
            None => return Err(call_error(ex, param_error(TYPE_MISMATCH, 2))),
        }
    }
    let code = match given(ex, 2)? {
        Some(v) => ex.present(&v)?,
        None => Str::new(),
    };
    let additional = match given(ex, 3)? {
        None | Some(Value::Undefined) => Value::String(Str::new()),
        Some(v) => Value::String(ex.present(&v)?),
    };
    let cause = match given(ex, 4)? {
        None | Some(Value::Undefined) => None,
        Some(v) => match lang::error_info_of(&v) {
            Some(i) => Some(i),
            None => return Err(call_error(ex, param_error(TYPE_MISMATCH, 5))),
        },
    };
    Ok(raised(ex, text, cats, cause, code, additional))
}

const FIXED: [Category; 5] = [
    Category::AllErrors,
    Category::ExceptionRaisedFromScript,
    Category::ScriptRuntimeError,
    Category::ScriptCompileError,
    Category::OtherError,
];

fn call_error(ex: &Execution, e: HostError) -> Exception {
    Exception::Catchable(from_host(ex, e, CallKind::Method, RAISE_CALL))
}

fn param_error(key: &'static str, n: u32) -> HostError {
    let mut e = HostError::key(key);
    if let MsgRef::Key(k) = &mut e.msg {
        k.args.push(n.to_string());
    }
    e
}

fn raised(
    ex: &Execution,
    description: Str,
    extra: CategorySet,
    cause: Option<Rc<ErrorInfo>>,
    code: Str,
    additional: Value,
) -> Exception {
    let mut categories = extra.union(CategorySet::of(&[
        Category::AllErrors,
        Category::ExceptionRaisedFromScript,
        Category::ScriptRuntimeError,
    ]));
    let mut cur = cause.as_deref();
    while let Some(c) = cur {
        categories = categories.union(c.categories);
        cur = c.cause.as_deref();
    }
    let pos = ex.position();
    Exception::Catchable(Rc::new(ErrorInfo {
        description,
        module_name: pos.module_name,
        line: pos.line,
        source_line: pos.source_line,
        cause,
        code,
        additional,
        categories,
        stack: pos.stack,
        key: None,
        wire: None,
    }))
}
