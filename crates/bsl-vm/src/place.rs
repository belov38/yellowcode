use crate::error::Exception;
use crate::exec::internal;
use crate::host::ModuleInstance;
use crate::runtime::Execution;
use crate::value::{ObjRef, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub enum Arg {
    Val(Value),
    Place(Place),
    Omitted,
}

#[derive(Clone)]
pub struct Place(Kind);

#[derive(Clone)]
enum Kind {
    Temp(Rc<RefCell<Value>>),
    ModVar { inst: Rc<ModuleInstance>, i: usize },
    Local { abs: usize },
    Member { obj: ObjRef, name: Rc<str> },
    Index { obj: ObjRef, key: Value },
}

impl Place {
    pub fn temp(v: Value) -> Place {
        Place(Kind::Temp(Rc::new(RefCell::new(v))))
    }

    pub(crate) fn mod_var(inst: Rc<ModuleInstance>, i: usize) -> Place {
        Place(Kind::ModVar { inst, i })
    }

    pub(crate) fn local(abs: usize) -> Place {
        Place(Kind::Local { abs })
    }

    pub(crate) fn member(obj: ObjRef, name: &str) -> Place {
        Place(Kind::Member {
            obj,
            name: Rc::from(name),
        })
    }

    pub(crate) fn index(obj: ObjRef, key: Value) -> Place {
        Place(Kind::Index { obj, key })
    }

    pub fn temp_value(&self) -> Option<Value> {
        match &self.0 {
            Kind::Temp(c) => Some(c.borrow().clone()),
            _ => None,
        }
    }

    pub fn numeric_backing_bytes(&self, ex: &Execution) -> u64 {
        let bytes = |value: &Value| match value {
            Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
            _ => 0,
        };
        match &self.0 {
            Kind::Temp(c) => bytes(&c.borrow()),
            Kind::ModVar { inst, i } => inst.vars.borrow().get(*i).map_or(0, bytes),
            Kind::Local { abs } => ex.exec.stack.get(*abs).map_or(0, bytes),
            Kind::Member { .. } | Kind::Index { .. } => 0,
        }
    }

    pub fn get_guarded(&self, ex: &mut Execution, live_bytes: u64) -> Result<Value, Exception> {
        let limbs = |value: &Value| match value {
            Value::Number(n) => n.size_hint_limbs(),
            _ => 0,
        };
        let count = match &self.0 {
            Kind::Temp(c) => limbs(&c.borrow()),
            Kind::ModVar { inst, i } => inst.vars.borrow().get(*i).map_or(0, limbs),
            Kind::Local { abs } => ex.exec.stack.get(*abs).map_or(0, limbs),
            Kind::Member { .. } | Kind::Index { .. } => 0,
        };
        ex.reserve(live_bytes.saturating_add(count.saturating_mul(4)))?;
        ex.charge(count / 64)?;
        self.get(ex)
    }

    pub fn get(&self, ex: &mut Execution) -> Result<Value, Exception> {
        match &self.0 {
            Kind::Temp(c) => Ok(c.borrow().clone()),
            Kind::ModVar { inst, i } => inst
                .vars
                .borrow()
                .get(*i)
                .cloned()
                .ok_or_else(|| internal("module variable out of range")),
            Kind::Local { abs } => ex
                .exec
                .stack
                .get(*abs)
                .cloned()
                .ok_or_else(|| internal("slot out of range")),
            Kind::Member { obj, name } => ex.get_member(&Value::Object(obj.clone()), name),
            Kind::Index { obj, key } => ex.index_get(&Value::Object(obj.clone()), key),
        }
    }

    pub fn set(&self, ex: &mut Execution, v: Value) -> Result<(), Exception> {
        match &self.0 {
            Kind::Temp(c) => {
                let old = c.replace(v);
                drop(old);
                Ok(())
            }
            Kind::ModVar { inst, i } => {
                let old = {
                    let mut vars = inst.vars.borrow_mut();
                    let slot = vars
                        .get_mut(*i)
                        .ok_or_else(|| internal("module variable out of range"))?;
                    std::mem::replace(slot, v)
                };
                drop(old);
                Ok(())
            }
            Kind::Local { abs } => {
                let slot = ex
                    .exec
                    .stack
                    .get_mut(*abs)
                    .ok_or_else(|| internal("slot out of range"))?;
                let old = std::mem::replace(slot, v);
                drop(old);
                Ok(())
            }
            Kind::Member { obj, name } => ex.set_member(&Value::Object(obj.clone()), name, v),
            Kind::Index { obj, key } => ex.index_set(&Value::Object(obj.clone()), key, v),
        }
    }
}
