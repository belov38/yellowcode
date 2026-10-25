use super::{method_name, not_implemented, type_name, value_list::arg_value_cached};
use crate::{
    sort::{SortKey, sort_key},
    typedesc::idx,
};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostObject, MemberId, TypeId, Value};
use std::{any::Any, cmp::Ordering, rc::Rc};

pub(super) struct CompareValues;

fn bytes(value: &Value) -> u64 {
    match value {
        Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(s) => (s.len() as u64).saturating_mul(2),
        _ => crate::values::binary_data_bytes(value).map_or_else(
            || super::value_list::scalar_clone_bytes(value),
            |b| b.len() as u64,
        ),
    }
}
pub(super) fn ctor(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    ex.reserve((std::mem::size_of::<CompareValues>() + 2 * std::mem::size_of::<usize>()) as u64)?;
    Ok(Value::Object(Rc::new(CompareValues)))
}
impl HostObject for CompareValues {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("CompareValues"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(idx("CompareValues"), m).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(idx("CompareValues"), m).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if method_name(idx("CompareValues"), m) != Some("Compare") {
            return Err(not_implemented(idx("CompareValues"), m).into());
        }
        let mut live = (2 * std::mem::size_of::<SortKey>()) as u64;
        for arg in args.iter() {
            if let Arg::Val(value) = arg {
                live = live.saturating_add(bytes(value));
            }
        }
        ex.reserve(live)?;
        let left = arg_value_cached(ex, args.first(), live)?;
        live = live.saturating_add(if matches!(args.first(), Some(Arg::Val(_))) {
            super::value_list::scalar_clone_bytes(&left)
        } else {
            bytes(&left)
        });
        ex.reserve(live)?;
        let right = arg_value_cached(ex, args.get(1), live)?;
        live = live.saturating_add(if matches!(args.get(1), Some(Arg::Val(_))) {
            super::value_list::scalar_clone_bytes(&right)
        } else {
            bytes(&right)
        });
        ex.reserve(live)?;
        let a = sort_key(ex, &left, &mut live)?;
        let b = sort_key(ex, &right, &mut live)?;
        ex.charge(1 + a.payload_work().max(b.payload_work()))?;
        Ok(Value::from(match a.cmp(&b) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, idx("CompareValues"))))
    }
}
