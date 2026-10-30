mod array;
mod codec;
mod columns;
mod compare;
mod fill;
mod fixed;
mod map;
mod pair;
mod retained;
mod retire;
mod structure;
mod value_list;
mod value_table;
mod value_tree;
pub use value_table::ValueTable;
pub use value_tree::ValueTree;
mod xdto;
pub use value_list::{new_value_list, value_list_items};

pub(crate) use array::new_fixed_array;
pub use array::{array_items, new_array};
pub(crate) use codec::codecs;
pub use structure::{new_structure, structure_fields, structure_get};
pub(crate) use xdto::xdto_codecs;

pub use map::{map_pairs, map_put, new_fixed_map, new_map};
pub(crate) use pair::KeyValue;
pub(crate) use structure::is_name;
pub(crate) use structure::{copy_number, copy_value};

use crate::bind::msg;
use crate::enums::type_by_id;
use bsl_vm::{Exception, Execution, HostError, MemberId, Registry, TypeIdx, TypeImpl, Value};
use platform_catalog::catalog;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

pub(crate) struct Types {
    pub array: TypeIdx,
    pub fixed_array: TypeIdx,
    pub structure: TypeIdx,
    pub fixed_structure: TypeIdx,
    pub map: TypeIdx,
    pub fixed_map: TypeIdx,
    pub pair: TypeIdx,
    pub binary: TypeIdx,
    pub binary_buffer: TypeIdx,
    pub byte_order: TypeIdx,
    pub uuid: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            array: t("Array"),
            fixed_array: t("FixedArray"),
            structure: t("Structure"),
            fixed_structure: t("FixedStructure"),
            map: t("Map"),
            fixed_map: t("FixedMap"),
            pair: t("KeyAndValue"),
            binary: t("BinaryData"),
            binary_buffer: t("BinaryDataBuffer"),
            byte_order: t("ByteOrder"),
            uuid: t("UUID"),
        }
    })
}

pub(crate) fn register(reg: &mut Registry) {
    reg.global_fn(
        "ЗаполнитьЗначенияСвойств",
        "FillPropertyValues",
        Arc::new(fill::fill),
    );
    let t = types();
    let ctor = |f: fn(&mut Execution, &mut [bsl_vm::Arg]) -> bsl_vm::CallResult<Value>| TypeImpl {
        ctor: Some(Arc::new(f)),
    };
    reg.type_impl(crate::typedesc::idx("ValueList"), ctor(value_list::ctor));
    reg.type_impl(crate::typedesc::idx("ValueTable"), ctor(value_table::ctor));
    reg.type_impl(crate::typedesc::idx("ValueTree"), ctor(value_tree::ctor));
    reg.type_impl(crate::typedesc::idx("CompareValues"), ctor(compare::ctor));
    reg.type_impl(t.array, ctor(array::ctor));
    reg.type_impl(t.fixed_array, ctor(fixed::array_ctor));
    reg.type_impl(t.structure, ctor(structure::ctor));
    reg.type_impl(t.fixed_structure, ctor(fixed::structure_ctor));
    reg.type_impl(t.map, ctor(map::ctor));
    reg.type_impl(t.fixed_map, ctor(fixed::map_ctor));
}

pub(crate) fn creation_number() -> i32 {
    bsl_vm::creation_number() as i32
}

pub(crate) fn method_name(t: TypeIdx, m: MemberId) -> Option<&'static str> {
    let info = catalog().types().get(usize::from(t.0))?;
    let j = usize::try_from(m.0)
        .ok()?
        .checked_sub(info.properties.len())?;
    info.methods.get(j).map(|x| x.name_en.as_str())
}

pub(crate) fn property_name(t: TypeIdx, m: MemberId) -> Option<&'static str> {
    let info = catalog().types().get(usize::from(t.0))?;
    info.properties
        .get(usize::try_from(m.0).ok()?)
        .map(|x| x.name_en.as_str())
}

pub(crate) fn property_ru(t: TypeIdx, m: MemberId) -> &'static str {
    let info = catalog().types().get(usize::from(t.0));
    info.and_then(|i| i.properties.get(usize::try_from(m.0).ok()?))
        .map_or("", |p| p.name_ru.as_str())
}

pub(crate) fn type_name_ru(t: TypeIdx) -> &'static str {
    catalog()
        .types()
        .get(usize::from(t.0))
        .map_or("", |i| i.name_ru.as_str())
}

pub(crate) fn ctor_not_implemented(t: TypeIdx) -> HostError {
    HostError::not_implemented(type_name_ru(t), "Новый")
}

pub(crate) fn not_implemented(t: TypeIdx, m: MemberId) -> HostError {
    let info = catalog().types().get(usize::from(t.0));
    let type_name = info.map_or("", |i| i.name_ru.as_str());
    let member = info.and_then(|i| {
        let k = usize::try_from(m.0).ok()?;
        match k.checked_sub(i.properties.len()) {
            None => i.properties.get(k).map(|p| p.name_ru.clone()),
            Some(j) => i.methods.get(j).map(|x| x.name_ru.clone()),
        }
    });
    HostError::not_implemented(type_name, &member.unwrap_or_else(|| m.0.to_string()))
}

pub(crate) fn type_name(ex: &Execution, t: TypeIdx) -> bsl_prim::Str {
    let id = bsl_vm::TypeId::of_catalog(t);
    bsl_prim::Str::from(ex.runtime().types().name(id, ex.script_variant()))
}

pub(crate) fn release(values: Vec<Value>) {
    let mut stack = values;
    while let Some(v) = stack.pop() {
        let Value::Object(o) = v else {
            continue;
        };
        if Rc::strong_count(&o) == 1 {
            let any = o.as_any();
            if value_tree::drain(&*o, &mut stack, false) {
            } else if let Some(a) = any.downcast_ref::<array::Array>() {
                stack.extend(a.take_items());
            } else if let Some(s) = any.downcast_ref::<structure::Structure>() {
                stack.extend(s.take_values());
            } else if let Some(m) = any.downcast_ref::<map::Map>() {
                stack.extend(m.take_all());
            } else if let Some(list) = any.downcast_ref::<value_list::ValueList>() {
                stack.extend(list.take_values());
            } else if let Some(item) = any.downcast_ref::<value_list::ValueListItem>() {
                stack.extend(item.take_values());
            } else if let Some(table) = any.downcast_ref::<value_table::ValueTable>() {
                stack.extend(table.take_values());
            } else if let Some(row) = any.downcast_ref::<value_table::row::TableRow>() {
                stack.extend(row.take_values());
            } else if let Some(kv) = any.downcast_ref::<KeyValue>() {
                stack.extend(kv.take());
            } else if let Some(boundary) = any.downcast_ref::<crate::values::boundary::Boundary>() {
                stack.push(boundary.take());
            }
        }
        drop(o);
    }
}

pub(crate) fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(_), Value::Number(_)) | (Value::Number(_), Value::Bool(_)) => false,
        _ => bsl_vm::ops::equal(a, b),
    }
}

const VALUE_BYTES: u64 = std::mem::size_of::<Value>() as u64;

const MAX_VALUES: u64 = 1 << 27;

pub(crate) fn reserve_values(ex: &mut Execution, n: u64) -> Result<(), Exception> {
    if n > MAX_VALUES {
        return Err(ex.out_of_memory());
    }
    ex.reserve(n.saturating_mul(VALUE_BYTES))
}

pub(crate) fn vec_with_capacity<T>(ex: &mut Execution, n: u64) -> Result<Vec<T>, Exception> {
    reserve_values(ex, n)?;
    let mut v = Vec::new();
    let n = usize::try_from(n).map_err(|_| ex.out_of_memory())?;
    v.try_reserve_exact(n).map_err(|_| ex.out_of_memory())?;
    Ok(v)
}

pub(crate) fn position(n: &bsl_prim::Number) -> Option<usize> {
    usize::try_from(n.trunc_to_i64_saturating()).ok()
}

pub(crate) fn no_field(ex: &mut Execution, key: &Value) -> bsl_vm::CallResult<HostError> {
    let name = match key {
        Value::String(s) => s.to_string_lossy(),
        other => ex.present(other)?.to_string_lossy(),
    };
    Ok(HostError::key(msg::PROP_NOT_FOUND).with_arg(name))
}

pub(crate) enum Snapshot {
    Array(Vec<Value>),
    Structure(Vec<(bsl_prim::Str, Value)>),
    Map(Vec<(Value, Value)>),
}

pub(crate) fn snapshot(v: &Value) -> Option<Snapshot> {
    if let Some(items) = array_items(v) {
        return Some(Snapshot::Array(items));
    }
    if let Some(f) = structure::field_pairs(v) {
        return Some(Snapshot::Structure(f));
    }
    map::map_pairs(v).map(Snapshot::Map)
}

pub(crate) mod accounting;
pub use accounting::{
    RetainedChargeError, retained_charge, retained_rc_size, retained_shallow_bytes,
};
