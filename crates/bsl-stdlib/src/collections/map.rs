use super::{
    KeyValue, creation_number, method_name, not_implemented, release, reserve_values, same_value,
    type_name, types,
};
use crate::bind::{arg_value, msg, type_mismatch};
use bsl_prim::{BoostKey, BoostMap, Str, boost_hash};
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, TypeIdx, Value};
use platform_catalog::catalog;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct Key {
    value: Value,
    hash: i32,
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        same_value(&self.value, &other.value)
    }
}

impl Eq for Key {}

impl BoostKey for Key {
    fn boost_hash(&self) -> i32 {
        self.hash
    }
}

fn own_hash(ex: &Execution, v: &Value) -> Option<i32> {
    Some(match v {
        Value::Undefined => boost_hash::UNDEFINED,
        Value::Null => boost_hash::NULL,
        Value::Bool(true) => boost_hash::TRUE,
        Value::Bool(false) => boost_hash::FALSE,
        Value::Number(n) => n.boost_hash(),
        Value::String(s) => s.boost_hash(),
        Value::Date(d) => d.boost_hash(),
        Value::Type(t) => type_hash(ex, *t),
        Value::Object(o) => return o.boost_hash(),
    })
}

fn type_hash(ex: &Execution, t: TypeId) -> i32 {
    let types = ex.runtime().types();
    let guid = match types.app_type(t) {
        Some(info) => Some(info.guid),
        None => types
            .catalog_type(t)
            .and_then(|idx| catalog().types().get(usize::from(idx.0))?.guid),
    };
    guid.map_or(t.0 as i32, |g| boost_hash::uuid(&g))
}

fn identity(v: &Value) -> Option<usize> {
    v.as_object().map(|o| Rc::as_ptr(o) as *const () as usize)
}

#[derive(Default)]
pub(crate) struct Table {
    pairs: BoostMap<Key, Value>,
    numbers: HashMap<usize, i32>,
    next: i32,
}

impl Table {
    fn key(&self, ex: &Execution, v: Value) -> Option<Key> {
        let hash = match own_hash(ex, &v) {
            Some(h) => h,
            None => *self.numbers.get(&identity(&v)?)?,
        };
        Some(Key { value: v, hash })
    }

    fn get(&self, ex: &Execution, v: Value) -> Value {
        self.key(ex, v)
            .and_then(|k| self.pairs.get(&k).cloned())
            .unwrap_or_default()
    }

    fn insert(&mut self, ex: &Execution, k: Value, v: Value) -> Option<Value> {
        let hash = match own_hash(ex, &k) {
            Some(h) => h,
            None => {
                let id = identity(&k).unwrap_or_default();
                let next = &mut self.next;
                *self.numbers.entry(id).or_insert_with(|| {
                    let n = *next;
                    *next = next.wrapping_add(1);
                    n
                })
            }
        };
        self.pairs.insert(Key { value: k, hash }, v)
    }

    fn remove(&mut self, ex: &Execution, k: Value) -> Option<Value> {
        let numbered = own_hash(ex, &k).is_none();
        let id = identity(&k);
        let key = self.key(ex, k)?;
        let old = self.pairs.remove(&key);
        if numbered && old.is_some() {
            self.numbers.remove(&id.unwrap_or_default());
        }
        old
    }

    fn copy(&self) -> Table {
        let mut pairs = BoostMap::new();
        for (k, v) in self.pairs.iter() {
            pairs.insert(k.clone(), v.clone());
        }
        Table {
            pairs,
            numbers: self.numbers.clone(),
            next: self.next,
        }
    }

    fn take_all(&mut self) -> Vec<Value> {
        let all = self
            .pairs
            .iter()
            .flat_map(|(k, v)| [k.value.clone(), v.clone()])
            .collect();
        self.pairs.clear();
        self.numbers.clear();
        all
    }
}

type Order = Rc<Vec<(Value, Value)>>;

pub(crate) struct Map {
    table: RefCell<Table>,
    order: RefCell<Option<Order>>,
    fixed: bool,
    hash: i32,
}

impl Map {
    pub(super) fn retirement_work(&self) -> u64 {
        let table = self.table.borrow();
        (table.pairs.storage_slots() as u64).saturating_add(table.numbers.capacity() as u64)
    }
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for (key, value) in self.table.borrow().pairs.iter() {
            visit(&key.value)?;
            visit(value)?;
        }
        if let Some(order) = self.order.borrow().as_ref() {
            for (key, value) in order.iter() {
                visit(key)?;
                visit(value)?;
            }
        }
        Ok(())
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        if let Some(order) = self.order.borrow_mut().take()
            && let Ok(order) = Rc::try_unwrap(order)
        {
            for (key, value) in order {
                super::retire::push(out, key);
                super::retire::push(out, value);
            }
        }
        let mut table = self.table.borrow_mut();
        table.numbers.clear();
        for (key, value) in table.pairs.drain() {
            super::retire::push(out, key.value);
            super::retire::push(out, value);
        }
    }

    pub(crate) fn new_value(table: Table, fixed: bool) -> Value {
        Value::Object(Rc::new(Map {
            table: RefCell::new(table),
            order: RefCell::new(None),
            fixed,
            hash: creation_number(),
        }))
    }

    pub(crate) fn of(v: &Value) -> Option<&Map> {
        v.as_object()?.as_any().downcast_ref::<Map>()
    }

    pub(crate) fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub(crate) fn take_all(&self) -> Vec<Value> {
        drop(self.order.borrow_mut().take());
        self.table.borrow_mut().take_all()
    }

    fn type_idx(&self) -> TypeIdx {
        if self.fixed {
            types().fixed_map
        } else {
            types().map
        }
    }

    fn len(&self) -> usize {
        self.table.borrow().pairs.len()
    }

    fn changed(&self) {
        drop(self.order.borrow_mut().take());
    }

    fn put(&self, ex: &mut Execution, key: Value, v: Value) -> CallResult<()> {
        let ignored = ex.env().compat().flags().map_ignores_undefined_key;
        if ignored && matches!(key, Value::Undefined) {
            return Ok(());
        }
        reserve_values(ex, (self.len() as u64 + 1).saturating_mul(2))?;
        self.changed();
        let old = self.table.borrow_mut().insert(ex, key, v);
        drop(old);
        Ok(())
    }

    fn ordered(&self) -> Order {
        if let Some(o) = self.order.borrow().as_ref() {
            return o.clone();
        }
        let pairs: Vec<(Value, Value)> = self
            .table
            .borrow()
            .pairs
            .iter()
            .map(|(k, v)| (k.value.clone(), v.clone()))
            .collect();
        let pairs = Rc::new(pairs);
        *self.order.borrow_mut() = Some(pairs.clone());
        pairs
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        release(self.take_all());
    }
}

impl HostObject for Map {
    fn to_shared(&self) -> Option<bsl_vm::SharedValue> {
        bsl_vm::SharedValue::from_object(self)
    }

    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.table
            .borrow()
            .pairs
            .iter()
            .all(|(key, value)| visit.value(&key.value) && visit.value(value))
            && self
                .order
                .borrow()
                .as_ref()
                .is_none_or(|order| visit.backing(Rc::new(SessionOrder(order.clone()))))
    }
    fn session_clear(&self) {
        drop(self.take_all());
        self.order.borrow_mut().take();
    }
    fn session_transfer_supported(&self) -> bool {
        true
    }
    fn session_transfer(
        &self,
        out: &mut dyn bsl_vm::SessionExport,
    ) -> Result<Option<Box<dyn bsl_vm::SessionObject>>, bsl_vm::Exception> {
        let table = self.table.borrow();
        out.reserve(
            (std::mem::size_of::<SessionMap>() + table.pairs.len() * std::mem::size_of::<i32>())
                as u64,
        )?;
        let mut hashes = Vec::new();
        hashes
            .try_reserve_exact(table.pairs.len())
            .map_err(|_| out.memory_error())?;
        for (key, value) in table.pairs.iter() {
            hashes.push(key.hash);
            out.value(&key.value)?;
            out.value(value)?;
        }
        Ok(Some(Box::new(SessionMap {
            hashes,
            buckets: table.pairs.bucket_count(),
            fixed: self.fixed,
            hash: self.hash,
            next: table.next,
        })))
    }
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        visit.mutable();
        let table = self.table.borrow();
        visit.bytes(
            table
                .pairs
                .backing_bytes()
                .saturating_add(
                    (table.numbers.capacity() as u64)
                        .saturating_mul(2 * (std::mem::size_of::<(usize, i32)>() as u64 + 1)),
                )
                .saturating_add(32),
        )?;
        if let Some(order) = self.order.borrow().as_ref()
            && visit.allocation(
                Rc::as_ptr(order) as usize,
                (std::mem::size_of::<Vec<(Value, Value)>>() + 2 * std::mem::size_of::<usize>())
                    as u64
                    + (order.capacity() as u64)
                        .saturating_mul(std::mem::size_of::<(Value, Value)>() as u64),
            )?
        {
            for (key, value) in order.iter() {
                visit.value(key)?;
                visit.value(value)?;
            }
        }
        for (key, value) in table.pairs.iter() {
            visit.value(&key.value)?;
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
                Some("Count") => Ok(Value::from(self.len() as i64)),
                Some("Get") => {
                    let k = arg_value(ex, args.first())?;
                    Ok(self.table.borrow().get(ex, k))
                }
                Some("Insert") if !self.fixed => {
                    let k = arg_value(ex, args.first())?;
                    let v = arg_value(ex, args.get(1))?;
                    self.put(ex, k, v)?;
                    Ok(Value::Undefined)
                }
                Some("Delete") if !self.fixed => {
                    let k = arg_value(ex, args.first())?;
                    self.changed();
                    let old = self.table.borrow_mut().remove(ex, k);
                    drop(old);
                    Ok(Value::Undefined)
                }
                Some("Clear") if !self.fixed => {
                    release(self.take_all());
                    Ok(Value::Undefined)
                }
                _ => Err(not_implemented(t, m).into()),
            }
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        Ok(self.table.borrow().get(ex, key.clone()))
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        ex.with_reuse_check(|ex| {
            if self.fixed {
                return Err(HostError::key(msg::INDEXED_READONLY).into());
            }
            self.put(ex, key.clone(), v)
        })
    }

    fn iter_keys(&self, ex: &mut Execution) -> CallResult<Option<Vec<Value>>> {
        ex.with_reuse_check(|ex| {
            reserve_values(ex, self.len() as u64)?;
            Ok(Some(
                self.table
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(k, _)| k.value.clone())
                    .collect(),
            ))
        })
    }

    fn iter_value(&self, ex: &mut Execution, key: &Value) -> CallResult<Option<Value>> {
        ex.with_reuse_check(|ex| {
            let table = self.table.borrow();
            let Some(k) = table.key(ex, key.clone()) else {
                return Ok(None);
            };
            Ok(table
                .pairs
                .get(&k)
                .map(|v| KeyValue::new_value(key.clone(), v.clone())))
        })
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.len())
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        ex.with_reuse_check(|_ex| match self.ordered().get(i) {
            Some((k, v)) => Ok(KeyValue::new_value(k.clone(), v.clone())),
            None => Err(HostError::key(msg::INDEX_OFFBOUND).into()),
        })
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, self.type_idx())))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }
}

struct SessionMap {
    hashes: Vec<i32>,
    buckets: usize,
    fixed: bool,
    hash: i32,
    next: i32,
}
struct SessionOrder(Order);
impl bsl_vm::SessionBacking for SessionOrder {
    fn session_owner(&self) -> Option<(usize, usize)> {
        Some((Rc::as_ptr(&self.0) as usize, Rc::strong_count(&self.0)))
    }
    fn session_edges(&self, visit: &mut dyn bsl_vm::SessionEdges) -> bool {
        self.0
            .iter()
            .all(|(key, value)| visit.value(key) && visit.value(value))
    }
}
impl bsl_vm::SessionObject for SessionMap {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<(), bsl_vm::Exception> {
        visit.bytes((self.hashes.capacity() * std::mem::size_of::<i32>()) as u64)
    }
    fn create(
        &self,
        _: &mut Execution,
        _: &mut bsl_vm::SessionImports,
    ) -> Result<Value, bsl_vm::Exception> {
        Ok(Value::Object(Rc::new(Map {
            table: RefCell::new(Table::default()),
            order: RefCell::new(None),
            fixed: self.fixed,
            hash: self.hash,
        })))
    }
    fn fill(
        &self,
        ex: &mut Execution,
        shell: &Value,
        children: Vec<Value>,
    ) -> Result<(), bsl_vm::Exception> {
        if let Some(map) = Map::of(shell) {
            let mut table = map.table.borrow_mut();
            table.next = self.next;
            table.pairs = BoostMap::try_with_buckets(self.buckets, self.hashes.len())
                .ok_or_else(|| ex.out_of_memory())?;
            table
                .numbers
                .try_reserve(self.hashes.len())
                .map_err(|_| ex.out_of_memory())?;
            let mut children = children.into_iter().rev();
            for &hash in self.hashes.iter().rev() {
                let (Some(value), Some(key)) = (children.next(), children.next()) else {
                    return Err(ex.out_of_memory());
                };
                if own_hash(ex, &key).is_none()
                    && let Some(id) = identity(&key)
                {
                    table.numbers.insert(id, hash);
                }
                drop(table.pairs.insert(Key { value: key, hash }, value));
            }
        }
        Ok(())
    }
    fn clear(&self, shell: &Value) {
        if let Some(map) = Map::of(shell) {
            drop(map.take_all());
        }
    }
    fn restore_bytes(&self, children: usize) -> u64 {
        (children as u64)
            .saturating_mul(256)
            .saturating_add(256)
            .saturating_add((self.buckets * std::mem::size_of::<u32>()) as u64)
    }
}

pub fn map_pairs(v: &Value) -> Option<Vec<(Value, Value)>> {
    Some(
        Map::of(v)?
            .table
            .borrow()
            .pairs
            .iter()
            .map(|(key, value)| (key.value.clone(), value.clone()))
            .collect(),
    )
}

pub fn new_map() -> Value {
    Map::new_value(Table::default(), false)
}

pub fn map_put(ex: &mut Execution, m: &Value, k: Value, v: Value) -> CallResult<()> {
    ex.with_reuse_check(|ex| match Map::of(m) {
        Some(map) if !map.fixed => map.put(ex, k, v),
        _ => Err(type_mismatch(1).into()),
    })
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let table = copy_of_arg(ex, args, |m| m.is_fixed())?;
    Ok(Map::new_value(table, false))
}

pub(super) fn copy_of_arg(
    ex: &mut Execution,
    args: &mut [Arg],
    copyable: fn(&Map) -> bool,
) -> CallResult<Table> {
    let src = arg_value(ex, args.first())?;
    if matches!(src, Value::Undefined) {
        return Ok(Table::default());
    }
    match Map::of(&src) {
        Some(m) if copyable(m) => {
            reserve_values(ex, (m.len() as u64).saturating_mul(2))?;
            Ok(m.table.borrow().copy())
        }
        _ => Err(type_mismatch(1).into()),
    }
}

pub fn new_fixed_map(ex: &Execution, pairs: Vec<(Value, Value)>) -> Value {
    let mut table = Table::default();
    for (k, v) in pairs {
        drop(table.insert(ex, k, v));
    }
    Map::new_value(table, true)
}
