use super::{
    creation_number, method_name, no_field, not_implemented, position, release, reserve_values,
    same_value, type_name, types, vec_with_capacity,
};
use crate::bind::{arg_number, arg_value, msg, to_number, type_mismatch, value_mismatch};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value,
};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) struct Array {
    items: RefCell<Vec<Value>>,
    fixed: bool,
    hash: i32,
}

impl Array {
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for value in self.items.borrow().iter() {
            visit(value)?;
        }
        Ok(())
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        for value in self.items.borrow_mut().drain(..) {
            super::retire::push(out, value);
        }
    }

    pub(crate) fn new(items: Vec<Value>, fixed: bool) -> Rc<Array> {
        Rc::new(Array {
            items: RefCell::new(items),
            fixed,
            hash: creation_number(),
        })
    }

    pub(crate) fn new_value(items: Vec<Value>, fixed: bool) -> Value {
        Value::Object(Array::new(items, fixed))
    }

    pub(crate) fn of(v: &Value) -> Option<&Array> {
        v.as_object()?.as_any().downcast_ref::<Array>()
    }

    pub(crate) fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub(crate) fn items(&self) -> Vec<Value> {
        self.items.borrow().clone()
    }

    pub(super) fn retained_bytes(&self, ex: &mut Execution) -> CallResult<u64> {
        let items = self.items.borrow();
        ex.charge(items.len() as u64)?;
        let mut bytes = (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64
            + (items.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64);
        for value in items.iter() {
            bytes = bytes.saturating_add(match value {
                Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
                Value::String(s) => (s.len() as u64).saturating_mul(2),
                value => crate::values::binary_data_bytes(value).map_or(0, |b| b.len() as u64),
            });
        }
        Ok(bytes)
    }

    pub(super) fn guarded_items(
        &self,
        ex: &mut Execution,
        live: u64,
    ) -> CallResult<(Vec<Value>, u64)> {
        let items = self.items.borrow();
        ex.charge(items.len() as u64)?;
        let mut bytes = live
            .saturating_add((std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64)
            .saturating_add(
                (items.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            )
            .saturating_add(
                (items.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            );
        for value in items.iter() {
            let scalar = super::value_list::scalar_clone_bytes(value);
            let source = match value {
                Value::String(s) => (s.len() as u64).saturating_mul(2),
                value => crate::values::binary_data_bytes(value).map_or(scalar, |b| b.len() as u64),
            };
            bytes = bytes.saturating_add(source).saturating_add(scalar);
            if let Value::Number(n) = value {
                ex.charge(n.size_hint_limbs() / 64)?;
            }
        }
        ex.reserve(bytes)?;
        let mut snapshot = vec_with_capacity(ex, items.len() as u64)?;
        snapshot.extend(items.iter().cloned());
        Ok((snapshot, bytes))
    }

    pub(crate) fn items_clone_bytes(&self) -> u64 {
        let items = self.items.borrow();
        items.iter().fold(
            (items.len() as u64).saturating_mul(std::mem::size_of::<Value>() as u64),
            |bytes, value| {
                if let Value::Number(n) = value {
                    bytes.saturating_add(n.size_hint_limbs().saturating_mul(4))
                } else {
                    bytes
                }
            },
        )
    }

    pub(crate) fn set_items(&self, items: Vec<Value>) {
        *self.items.borrow_mut() = items;
    }

    pub(crate) fn take_items(&self) -> Vec<Value> {
        std::mem::take(&mut *self.items.borrow_mut())
    }

    fn type_idx(&self) -> TypeIdx {
        if self.fixed {
            types().fixed_array
        } else {
            types().array
        }
    }

    fn len(&self) -> usize {
        self.items.borrow().len()
    }

    fn checked(&self, ex: &mut Execution, a: Option<&Arg>) -> CallResult<usize> {
        let v = arg_value(ex, a)?;
        let Ok(n) = to_number(ex, &v) else {
            return Err(type_mismatch(1).into());
        };
        match position(&n) {
            Some(i) if i < self.len() => Ok(i),
            _ => Err(HostError::key(msg::RANGE).into()),
        }
    }

    fn slot(&self, ex: &mut Execution, key: &Value) -> CallResult<usize> {
        let Value::Number(n) = key else {
            return Err(no_field(ex, key)?.into());
        };
        match position(n) {
            Some(i) if i < self.len() => Ok(i),
            _ => Err(HostError::key(msg::INDEX_OFFBOUND).into()),
        }
    }

    fn push(&self, ex: &mut Execution, v: Value) -> Result<(), Exception> {
        let len = self.len();
        reserve_values(ex, len as u64 + 1)?;
        let mut items = self.items.borrow_mut();
        items.try_reserve(1).map_err(|_| ex.out_of_memory())?;
        items.push(v);
        Ok(())
    }

    fn insert(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<()> {
        let n = arg_number(ex, args.first(), 1)?;
        let Some(i) = position(&n) else {
            return Err(value_mismatch(1).into());
        };
        let v = arg_value(ex, args.get(1))?;
        let len = self.len();
        let new_len = (i.max(len) as u64).saturating_add(1);
        reserve_values(ex, new_len)?;
        let mut items = self.items.borrow_mut();
        let extra = usize::try_from(new_len).map_err(|_| ex.out_of_memory())? - len;
        items.try_reserve(extra).map_err(|_| ex.out_of_memory())?;
        if i > len {
            items.resize(i, Value::Undefined);
        }
        items.insert(i, v);
        Ok(())
    }
}

impl Drop for Array {
    fn drop(&mut self) {
        release(std::mem::take(self.items.get_mut()));
    }
}

impl HostObject for Array {
    fn to_shared(&self) -> Option<bsl_vm::SharedValue> {
        bsl_vm::SharedValue::from_object(self)
    }

    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.items.borrow().iter().all(|value| visit.value(value))
    }
    fn session_clear(&self) {
        drop(self.take_items());
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, Exception> {
        out.reserve(std::mem::size_of::<SessionArray>() as u64)?;
        for value in self.items.borrow().iter() {
            out.value(value)?;
        }
        Ok(Some(Box::new(SessionArray {
            fixed: self.fixed,
            hash: self.hash,
        })))
    }
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        if !self.fixed {
            visit.mutable();
        }
        let items = self.items.borrow();
        visit
            .bytes((items.capacity() as u64).saturating_mul(std::mem::size_of::<Value>() as u64))?;
        for value in items.iter() {
            visit.value(value)?;
        }
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.type_idx())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(self.type_idx(), m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(self.type_idx(), m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        ex.with_reuse_check(|ex| {
            let t = self.type_idx();
            match method_name(t, m) {
                Some("UBound") => Ok(Value::from(self.len() as i64 - 1)),
                Some("Count") => Ok(Value::from(self.len() as i64)),
                Some("Find") => {
                    let v = arg_value(ex, args.first())?;
                    let found = self.items.borrow().iter().position(|x| same_value(x, &v));
                    Ok(found.map_or(Value::Undefined, |i| Value::from(i as i64)))
                }
                Some("Get") => {
                    let i = self.checked(ex, args.first())?;
                    Ok(self.items.borrow()[i].clone())
                }
                Some("Add") if !self.fixed => {
                    let v = arg_value(ex, args.first())?;
                    self.push(ex, v)?;
                    Ok(Value::Undefined)
                }
                Some("Insert") if !self.fixed => {
                    self.insert(ex, args)?;
                    Ok(Value::Undefined)
                }
                Some("Set") if !self.fixed => {
                    let i = self.checked(ex, args.first())?;
                    let v = arg_value(ex, args.get(1))?;
                    let old = match self.items.borrow_mut().get_mut(i) {
                        Some(slot) => std::mem::replace(slot, v),
                        None => return Err(HostError::key(msg::RANGE).into()),
                    };
                    drop(old);
                    Ok(Value::Undefined)
                }
                Some("Delete") if !self.fixed => {
                    let i = self.checked(ex, args.first())?;
                    let removed = self.items.borrow_mut().remove(i);
                    drop(removed);
                    Ok(Value::Undefined)
                }
                Some("Clear") if !self.fixed => {
                    let old = self.take_items();
                    release(old);
                    Ok(Value::Undefined)
                }
                _ => Err(not_implemented(t, m).into()),
            }
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = self.slot(ex, key)?;
        Ok(self.items.borrow()[i].clone())
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        ex.with_reuse_check(|ex| {
            if self.fixed {
                return Err(HostError::key(msg::INDEXED_READONLY).into());
            }
            let i = self.slot(ex, key)?;
            let old = std::mem::replace(&mut self.items.borrow_mut()[i], v);
            drop(old);
            Ok(())
        })
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        match self.items.borrow().get(i) {
            Some(v) => Ok(v.clone()),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).into()),
        }
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, self.type_idx())))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

struct SessionArray {
    fixed: bool,
    hash: i32,
}
impl bsl_vm::SessionObject for SessionArray {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<(), Exception> {
        Ok(())
    }
    fn create(
        &self,
        _: &mut Execution,
        _: &mut bsl_vm::SessionImports,
    ) -> Result<Value, Exception> {
        Ok(Value::Object(Rc::new(Array {
            items: RefCell::new(Vec::new()),
            fixed: self.fixed,
            hash: self.hash,
        })))
    }
    fn fill(
        &self,
        _: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), Exception> {
        if let Some(array) = Array::of(shell) {
            array.set_items(children);
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(array) = Array::of(shell) {
            drop(array.take_items());
        }
    }
    fn restore_bytes(&self, children: usize) -> u64 {
        (std::mem::size_of::<Array>()
            + 2 * std::mem::size_of::<usize>()
            + children * std::mem::size_of::<Value>()) as u64
    }
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() {
        return Ok(Array::new_value(Vec::new(), false));
    }
    let mut dims = Vec::new();
    for (k, a) in args.iter().enumerate() {
        let v = arg_value(ex, Some(a))?;
        if args.len() == 1
            && let Some(src) = Array::of(&v).filter(|s| s.fixed)
        {
            return Ok(Array::new_value(src.items(), false));
        }
        if matches!(v, Value::Object(_)) {
            return Err(type_mismatch(k + 1).into());
        }
        let n = to_number(ex, &v)?;
        match position(&n) {
            Some(d) if d > 0 => dims.push(d as u64),
            _ => return Err(value_mismatch(k + 1).into()),
        }
    }
    Ok(build(ex, &dims)?)
}

const NESTED_WEIGHT: u64 = 4;

fn build(ex: &mut Execution, dims: &[u64]) -> Result<Value, Exception> {
    let mut total: u64 = 0;
    let mut prod: u64 = 1;
    for (i, d) in dims.iter().enumerate() {
        prod = prod.saturating_mul(*d);
        let weight = if i + 1 < dims.len() { NESTED_WEIGHT } else { 1 };
        total = total.saturating_add(prod.saturating_mul(weight));
    }
    reserve_values(ex, total)?;
    let Some((&inner, outer)) = dims.split_last() else {
        return Ok(Array::new_value(Vec::new(), false));
    };
    let count = outer.iter().fold(1u64, |a, d| a.saturating_mul(*d));
    let mut level: Vec<Value> = vec_with_capacity(ex, count)?;
    for _ in 0..count {
        let mut items = vec_with_capacity(ex, inner)?;
        items.resize(inner as usize, Value::Undefined);
        level.push(Array::new_value(items, false));
    }
    for &d in outer.iter().rev() {
        let mut next = vec_with_capacity(ex, level.len() as u64 / d)?;
        let mut rest = level.into_iter();
        loop {
            let chunk: Vec<Value> = rest.by_ref().take(d as usize).collect();
            if chunk.is_empty() {
                break;
            }
            next.push(Array::new_value(chunk, false));
        }
        level = next;
    }
    Ok(level.pop().unwrap_or_default())
}

pub fn new_array(items: Vec<Value>) -> Value {
    Array::new_value(items, false)
}

pub(crate) fn new_fixed_array(items: Vec<Value>) -> Value {
    Array::new_value(items, true)
}

pub fn array_items(v: &Value) -> Option<Vec<Value>> {
    Array::of(v).map(Array::items)
}

impl Array {
    pub(super) fn retained(
        &self,
        a: &mut impl super::accounting::Charge,
    ) -> super::accounting::Result<()> {
        let values = self
            .items
            .try_borrow()
            .map_err(|_| super::RetainedChargeError::Borrowed)?;
        a.vector(&values)?;
        for value in values.iter() {
            a.value(value)?;
        }
        Ok(())
    }
}
