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
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.pair.borrow().iter().all(|value| visit.value(value))
    }
    fn session_clear(&self) {
        drop(self.take());
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        out.reserve(4)?;
        for value in self.pair.borrow().iter() {
            out.value(value)?;
        }
        Ok(Some(Box::new(SessionPair(self.hash))))
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        for value in self.pair.borrow().iter() {
            visit.value(value)?;
        }
        Ok(true)
    }
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

struct SessionPair(i32);
impl bsl_vm::SessionObject for SessionPair {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<(), bsl_vm::Exception> {
        Ok(())
    }
    fn create(
        &self,
        _: &mut Execution,
        _: &mut bsl_vm::SessionImports,
    ) -> Result<Value, bsl_vm::Exception> {
        Ok(Value::Object(Rc::new(KeyValue {
            pair: RefCell::new([Value::Undefined, Value::Undefined]),
            hash: self.0,
        })))
    }
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), bsl_vm::Exception> {
        if let Some(object) = shell
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<KeyValue>())
        {
            *object.pair.borrow_mut() = children.try_into().map_err(|_| ex.out_of_memory())?;
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(object) = shell
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<KeyValue>())
        {
            drop(object.take());
        }
    }
    fn restore_bytes(&self, _: usize) -> u64 {
        (std::mem::size_of::<KeyValue>() + 16) as u64
    }
}
