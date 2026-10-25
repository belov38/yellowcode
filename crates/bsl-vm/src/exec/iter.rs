use super::internal;
use super::run::pop;
use crate::error::info::CallKind;
use crate::error::{Exception, HostError};
use crate::object::{CallResult, HostObject};
use crate::place::Arg;
use crate::runtime::Execution;
use crate::types::{MemberId, TypeId};
use crate::value::{ObjRef, Value};
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

const NOT_ENUM: &str = "bsl:IDS_RTE_NOTENUM";

struct Iter {
    obj: ObjRef,
    pos: Cell<usize>,
    keys: Option<Vec<Value>>,
}

fn stray() -> crate::object::CallError {
    HostError::text("Итератор цикла", "Loop iterator").into()
}

impl HostObject for Iter {
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

fn slot_index(base: usize, sp_base: usize, slot: u32) -> Result<usize, Exception> {
    let abs = base + slot as usize;
    if abs < sp_base {
        Ok(abs)
    } else {
        Err(internal("iterator slot out of range"))
    }
}

pub(super) fn start(
    ex: &mut Execution,
    base: usize,
    sp_base: usize,
    slot: u32,
) -> Result<(), Exception> {
    let coll = pop(&mut ex.exec, sp_base)?;
    let abs = slot_index(base, sp_base, slot)?;
    let walkable = match &coll {
        Value::Object(o) => o.clone().iter_len(ex).map(|_| o.clone()),
        _ => None,
    };
    let Some(obj) = walkable else {
        return Err(ex.error(HostError::key(NOT_ENUM)));
    };
    let r = obj.iter_keys(ex);
    let keys = r.map_err(|e| ex.call_error(e))?;
    let it = Value::Object(Rc::new(Iter {
        obj,
        keys,
        pos: Cell::new(0),
    }));
    let old = std::mem::replace(&mut ex.exec.stack[abs], it);
    drop(old);
    Ok(())
}

pub(super) fn next(
    ex: &mut Execution,
    base: usize,
    sp_base: usize,
    slot: u32,
) -> Result<bool, Exception> {
    let abs = slot_index(base, sp_base, slot)?;
    let it = match &ex.exec.stack[abs] {
        Value::Object(o) if o.as_any().is::<Iter>() => o.clone(),
        _ => return Err(internal("IterNext without an iterator")),
    };
    let Some(it) = it.as_any().downcast_ref::<Iter>() else {
        return Err(internal("IterNext without an iterator"));
    };
    let obj = it.obj.clone();
    if let Some(keys) = &it.keys {
        while let Some(key) = keys.get(it.pos.get()) {
            it.pos.set(it.pos.get() + 1);
            let r = obj.iter_value(ex, key);
            if let Some(v) = r.map_err(|e| ex.call_error(e))? {
                ex.exec.stack.push(v);
                return Ok(true);
            }
        }
        let old = std::mem::replace(&mut ex.exec.stack[abs], Value::Undefined);
        drop(old);
        return Ok(false);
    }
    let i = it.pos.get();
    if i >= obj.iter_len(ex).unwrap_or(0) {
        let old = std::mem::replace(&mut ex.exec.stack[abs], Value::Undefined);
        drop(old);
        return Ok(false);
    }
    it.pos.set(i + 1);
    let r = obj.iter_get(ex, i);
    let v = ex.finish(r, CallKind::Vm, "")?;
    ex.exec.stack.push(v);
    Ok(true)
}
