use super::{creation_number, not_implemented, property_name, release, type_name, types};
use crate::bind::msg;
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct KeyValue {
    pair: RefCell<[Value; 2]>,
    hash: i32,
}

impl KeyValue {
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for value in self.pair.borrow().iter() {
            visit(value)?;
        }
        Ok(())
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        for value in self.take() {
            super::retire::push(out, value);
        }
    }

    pub(crate) fn new_value(key: Value, value: Value) -> Value {
        Value::Object(Rc::new(KeyValue {
            pair: RefCell::new([key, value]),
            hash: creation_number(),
        }))
    }

    pub(crate) fn pair(&self) -> [Value; 2] {
        self.pair.borrow().clone()
    }

    pub(crate) fn take(&self) -> [Value; 2] {
        std::mem::take(&mut *self.pair.borrow_mut())
    }
}

impl Drop for KeyValue {
    fn drop(&mut self) {
        if self
            .pair
            .get_mut()
            .iter()
            .any(|v| !matches!(v, Value::Undefined))
        {
            release(std::mem::take(self.pair.get_mut()).into());
        }
    }
}

impl HostObject for KeyValue {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().pair)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let pair = self.pair.borrow();
        match property_name(types().pair, m) {
            Some("Key") => Ok(pair[0].clone()),
            Some("Value") => Ok(pair[1].clone()),
            _ => Err(not_implemented(types().pair, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, _m: MemberId, _v: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().pair, m).into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, types().pair)))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}
