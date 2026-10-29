mod fill;

use super::{
    KeyValue, creation_number, method_name, no_field, not_implemented, release, reserve_values,
    type_name, types,
};
use crate::bind::{arg_str, arg_value, msg, type_mismatch, value_mismatch};
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, TypeIdx, Value,
};
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const KEY_BASE: u32 = 0x1000_0000;
const MAX_KEY_ID: u32 = u32::MAX - KEY_BASE;

#[derive(Clone)]
struct Field {
    id: u32,
    key: Str,
    value: Value,
}

#[derive(Clone, Default)]
pub(crate) struct Fields {
    entries: Vec<Field>,
    by_name: HashMap<String, usize>,
    by_id: HashMap<u32, usize>,
    next_id: u32,
}

fn fold_units(key: &[u16]) -> String {
    fold_name(&String::from_utf16_lossy(key))
}

impl Fields {
    fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(&fold_name(name)).copied()
    }

    fn find_units(&self, key: &[u16]) -> Option<usize> {
        self.by_name.get(&fold_units(key)).copied()
    }

    #[must_use = "drop the replaced value after the borrow of the structure"]
    pub(crate) fn insert(&mut self, key: Str, value: Value) -> Option<Value> {
        let folded = fold_units(key.units());
        if let Some(&i) = self.by_name.get(&folded) {
            let f = &mut self.entries[i];
            f.key = key;
            return Some(std::mem::replace(&mut f.value, value));
        }
        if self.next_id >= MAX_KEY_ID {
            self.renumber();
        }
        let id = self.next_id;
        self.next_id += 1;
        let i = self.entries.len();
        self.entries.push(Field { id, key, value });
        self.by_name.insert(folded, i);
        self.by_id.insert(id, i);
        None
    }

    fn remove(&mut self, i: usize) -> Option<Value> {
        if i >= self.entries.len() {
            return None;
        }
        let f = self.entries.remove(i);
        self.reindex();
        Some(f.value)
    }

    fn renumber(&mut self) {
        for (i, f) in self.entries.iter_mut().enumerate() {
            f.id = i as u32;
        }
        self.next_id = self.entries.len() as u32;
        self.reindex();
    }

    fn reindex(&mut self) {
        self.by_name.clear();
        self.by_id.clear();
        for (i, f) in self.entries.iter().enumerate() {
            self.by_name.insert(fold_units(f.key.units()), i);
            self.by_id.insert(f.id, i);
        }
    }

    fn by_member(&self, m: MemberId) -> Option<usize> {
        let id = m.0.checked_sub(KEY_BASE)?;
        self.by_id.get(&id).copied()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub(crate) struct Structure {
    fields: RefCell<Fields>,
    fixed: bool,
    hash: i32,
}

impl Structure {
    pub(super) fn retirement_work(&self) -> u64 {
        let fields = self.fields.borrow();
        (fields.by_name.capacity() as u64).saturating_add(fields.by_id.capacity() as u64)
    }
    pub(super) fn visit_values(
        &self,
        visit: &mut super::retire::Visitor<'_>,
    ) -> Result<(), bsl_vm::Exception> {
        for field in &self.fields.borrow().entries {
            visit(&field.value)?;
        }
        Ok(())
    }
    pub(super) fn retire_into(&self, out: &mut Vec<Value>) {
        let mut fields = self.fields.borrow_mut();
        fields.by_name.clear();
        fields.by_id.clear();
        for field in fields.entries.drain(..) {
            super::retire::push(out, field.value);
        }
    }

    pub(crate) fn new_value(fields: Fields, fixed: bool) -> Value {
        Value::Object(Rc::new(Structure {
            fields: RefCell::new(fields),
            fixed,
            hash: creation_number(),
        }))
    }

    pub(crate) fn of(v: &Value) -> Option<&Structure> {
        v.as_object()?.as_any().downcast_ref::<Structure>()
    }

    pub(crate) fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub(super) fn retained_bytes(&self, ex: &mut Execution) -> CallResult<u64> {
        let fields = self.fields.borrow();
        ex.charge(
            (fields.entries.len() as u64)
                .saturating_add(fields.by_name.capacity() as u64)
                .saturating_add(fields.by_id.capacity() as u64),
        )?;
        let mut bytes = (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u64;
        bytes = bytes
            .saturating_add(
                (fields.entries.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<Field>() as u64),
            )
            .saturating_add(
                (fields.by_name.capacity() as u64)
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<(String, usize)>() as u64 + 1),
            )
            .saturating_add(
                (fields.by_id.capacity() as u64)
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<(u32, usize)>() as u64 + 1),
            );
        bytes = bytes.saturating_add(64);
        for name in fields.by_name.keys() {
            bytes = bytes.saturating_add(name.capacity() as u64);
        }
        for field in &fields.entries {
            let payload = match &field.value {
                Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
                Value::String(s) => (s.len() as u64).saturating_mul(2),
                value => crate::values::binary_data_bytes(value).map_or(0, |b| b.len() as u64),
            };
            bytes = bytes
                .saturating_add((field.key.len() as u64).saturating_mul(2))
                .saturating_add(payload);
        }
        Ok(bytes)
    }

    pub(super) fn guarded_pairs(
        &self,
        ex: &mut Execution,
        live: u64,
    ) -> CallResult<(Vec<(Str, Value)>, u64)> {
        let fields = self.fields.borrow();
        ex.charge(
            (fields.entries.len() as u64)
                .saturating_add(fields.by_name.capacity() as u64)
                .saturating_add(fields.by_id.capacity() as u64),
        )?;
        let slots = (fields.entries.len() as u64)
            .saturating_mul(std::mem::size_of::<(Str, Value)>() as u64);
        let mut bytes = live
            .saturating_add(slots)
            .saturating_add(
                (fields.entries.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<Field>() as u64),
            )
            .saturating_add(
                (fields.by_name.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<(String, usize)>() as u64 + 1),
            )
            .saturating_add(
                (fields.by_id.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<(u32, usize)>() as u64 + 1),
            );
        for key in fields.by_name.keys() {
            bytes = bytes.saturating_add(key.capacity() as u64);
        }
        for field in &fields.entries {
            let scalar = super::value_list::scalar_clone_bytes(&field.value);
            let source = match &field.value {
                Value::String(value) => (value.len() as u64).saturating_mul(2),
                value => crate::values::binary_data_bytes(value)
                    .map_or(scalar, |bytes| bytes.len() as u64),
            };
            bytes = bytes
                .saturating_add((field.key.len() as u64).saturating_mul(2))
                .saturating_add(source)
                .saturating_add(scalar);
            if let Value::Number(n) = &field.value {
                ex.charge(n.size_hint_limbs() / 64)?;
            }
        }
        ex.reserve(bytes)?;
        let mut pairs = super::vec_with_capacity(ex, fields.entries.len() as u64)?;
        pairs.extend(
            fields
                .entries
                .iter()
                .map(|f| (f.key.clone(), f.value.clone())),
        );
        Ok((pairs, bytes))
    }

    pub(crate) fn fields(&self) -> Fields {
        self.fields.borrow().clone()
    }

    pub(crate) fn take_values(&self) -> Vec<Value> {
        let mut f = self.fields.borrow_mut();
        f.by_name.clear();
        f.by_id.clear();
        if f.entries.is_empty() {
            return Vec::new();
        }
        std::mem::take(&mut f.entries)
            .into_iter()
            .map(|e| e.value)
            .collect()
    }

    fn type_idx(&self) -> TypeIdx {
        if self.fixed {
            types().fixed_structure
        } else {
            types().structure
        }
    }

    fn len(&self) -> usize {
        self.fields.borrow().len()
    }

    fn insert(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<()> {
        let key = arg_str(ex, args.first())?;
        if !is_name(key.units()) {
            return Err(HostError::key(msg::BAD_KEY).into());
        }
        let v = arg_value(ex, args.get(1))?;
        reserve_values(ex, self.len() as u64 + 1)?;
        let old = self.fields.borrow_mut().insert(key, v);
        drop(old);
        Ok(())
    }

    fn property(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        let key = arg_str(ex, args.first())?;
        let found = {
            let f = self.fields.borrow();
            f.find_units(key.units())
                .map(|i| f.entries[i].value.clone())
        };
        if let Some(Arg::Place(p)) = args.get(1) {
            p.set(ex, found.clone().unwrap_or_default())?;
        }
        Ok(Value::Bool(found.is_some()))
    }

    fn delete(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<()> {
        let key = arg_str(ex, args.first())?;
        let removed = {
            let mut f = self.fields.borrow_mut();
            match f.find_units(key.units()) {
                Some(i) => f.remove(i),
                None => None,
            }
        };
        drop(removed);
        Ok(())
    }

    fn slot(&self, ex: &mut Execution, key: &Value) -> CallResult<usize> {
        let found = match key {
            Value::String(s) => self.fields.borrow().find_units(s.units()),
            other => {
                let s = ex.present(other)?;
                self.fields.borrow().find_units(s.units())
            }
        };
        match found {
            Some(i) => Ok(i),
            None => Err(no_field(ex, key)?.into()),
        }
    }
}

impl Drop for Structure {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.fields.get_mut().entries);
        release(entries.into_iter().map(|e| e.value).collect());
    }
}

impl HostObject for Structure {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.type_idx())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let f = self.fields.borrow();
        match f.by_member(m) {
            Some(i) => Ok(f.entries[i].value.clone()),
            None => Err(not_implemented(self.type_idx(), m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        if self.fixed {
            return Err(HostError::key(msg::PROP_NOT_WRITABLE).raw().into());
        }
        let old = {
            let mut f = self.fields.borrow_mut();
            match f.by_member(m) {
                Some(i) => std::mem::replace(&mut f.entries[i].value, v),
                None => return Err(not_implemented(self.type_idx(), m).into()),
            }
        };
        drop(old);
        Ok(())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = self.type_idx();
        match method_name(t, m) {
            Some("Count") => Ok(Value::from(self.len() as i64)),
            Some("Property") => self.property(ex, args),
            Some("Insert") if !self.fixed => self.insert(ex, args).map(|()| Value::Undefined),
            Some("Delete") if !self.fixed => self.delete(ex, args).map(|()| Value::Undefined),
            Some("Clear") if !self.fixed => {
                release(self.take_values());
                Ok(Value::Undefined)
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let f = self.fields.borrow();
        let i = f.find(name)?;
        Some(MemberId(KEY_BASE + f.entries[i].id))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.fields.borrow().by_member(m)?;
        Some(MemberKind::Property {
            readable: true,
            writable: !self.fixed,
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let i = self.slot(ex, key)?;
        Ok(self.fields.borrow().entries[i].value.clone())
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        if self.fixed {
            return Err(HostError::key(msg::INDEXED_READONLY).into());
        }
        let i = self.slot(ex, key)?;
        let old = std::mem::replace(&mut self.fields.borrow_mut().entries[i].value, v);
        drop(old);
        Ok(())
    }

    fn iter_keys(&self, ex: &mut Execution) -> CallResult<Option<Vec<Value>>> {
        reserve_values(ex, self.len() as u64)?;
        Ok(Some(
            self.fields
                .borrow()
                .entries
                .iter()
                .map(|f| Value::String(f.key.clone()))
                .collect(),
        ))
    }

    fn iter_value(&self, _ex: &mut Execution, key: &Value) -> CallResult<Option<Value>> {
        let Value::String(key) = key else {
            return Ok(None);
        };
        let f = self.fields.borrow();
        Ok(f.find_units(key.units()).map(|i| {
            let field = &f.entries[i];
            KeyValue::new_value(Value::String(field.key.clone()), field.value.clone())
        }))
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        Some(self.len())
    }

    fn iter_get(&self, _ex: &mut Execution, i: usize) -> CallResult<Value> {
        let f = self.fields.borrow();
        match f.entries.get(i) {
            Some(e) => Ok(KeyValue::new_value(
                Value::String(e.key.clone()),
                e.value.clone(),
            )),
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

pub(crate) fn is_name(units: &[u16]) -> bool {
    let mut chars = char::decode_utf16(units.iter().copied());
    let first_ok = match chars.next() {
        Some(Ok(c)) => c.is_alphabetic() || c == '_',
        _ => false,
    };
    first_ok && chars.all(|c| c.is_ok_and(|c| c.is_alphanumeric() || c == '_'))
}

pub(super) fn from_keys(ex: &mut Execution, keys: &Str, values: &[Arg]) -> CallResult<Fields> {
    let mut fields = Fields::default();
    let mut k = 0;
    for part in keys.units().split(|&u| u == u16::from(b',')) {
        let key = bsl_lib::trim::trim(part);
        if key.is_empty() {
            continue;
        }
        if !is_name(key) {
            return Err(value_mismatch(1).into());
        }
        let v = arg_value(ex, values.get(k))?;
        k += 1;
        reserve_values(ex, fields.len() as u64 + 1)?;
        let old = fields.insert(Str::from_units(key), v);
        drop(old);
    }
    Ok(fields)
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let fields = fields_of_args(ex, args, |s| s.is_fixed())?;
    Ok(Structure::new_value(fields, false))
}

pub(super) fn fields_of_args(
    ex: &mut Execution,
    args: &mut [Arg],
    copyable: fn(&Structure) -> bool,
) -> CallResult<Fields> {
    let first = arg_value(ex, args.first())?;
    match &first {
        Value::Undefined => Ok(Fields::default()),
        Value::String(keys) => from_keys(ex, keys, args.get(1..).unwrap_or_default()),
        _ => match Structure::of(&first) {
            Some(s) if copyable(s) => Ok(s.fields()),
            _ => Err(type_mismatch(1).into()),
        },
    }
}

pub fn new_structure(fields: Vec<(&str, Value)>) -> Value {
    let mut f = Fields::default();
    for (k, v) in fields {
        let old = f.insert(Str::from(k), v);
        drop(old);
    }
    Structure::new_value(f, false)
}

pub fn structure_get(v: &Value, key: &str) -> Option<Value> {
    let f = Structure::of(v)?.fields.borrow();
    f.find(key).map(|i| f.entries[i].value.clone())
}

pub(crate) fn field_pairs(v: &Value) -> Option<Vec<(Str, Value)>> {
    let f = Structure::of(v)?.fields.borrow();
    Some(
        f.entries
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect(),
    )
}

pub fn structure_fields(v: &Value) -> Option<Vec<(String, Value)>> {
    let f = Structure::of(v)?.fields.borrow();
    Some(
        f.entries
            .iter()
            .map(|e| (e.key.to_string_lossy(), e.value.clone()))
            .collect(),
    )
}
