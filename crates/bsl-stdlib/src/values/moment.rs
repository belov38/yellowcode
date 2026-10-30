use crate::bind::{msg, type_mismatch};
use crate::collections::{
    accounting::retained_rc_size, method_name, not_implemented, property_name, property_ru,
};
use bsl_prim::{BslDate, Str};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, Registry,
    RetainedMemory, TypeId, TypeImpl, Value,
};
use std::{any::Any, borrow::Cow, cmp::Ordering, rc::Rc, sync::Arc};

fn ty() -> platform_catalog::TypeIdx {
    crate::typedesc::idx("PointInTime")
}

pub(crate) struct Moment {
    date: BslDate,
    reference: Value,
    key: Option<([u8; 16], [u8; 16])>,
}

fn of(value: &Value) -> Option<&Moment> {
    let actual: &dyn Any = value.as_object()?.as_ref();
    actual.downcast_ref()
}

pub fn new_point_in_time(
    ex: &mut Execution,
    date: BslDate,
    reference: &Value,
) -> Result<Value, Exception> {
    create(ex, date, reference).map_err(|e| ex.call_error(e))
}

fn create(ex: &mut Execution, date: BslDate, reference: &Value) -> CallResult<Value> {
    ex.charge(1)?;
    let key = match reference {
        Value::Undefined => None,
        Value::Object(object) => Some(
            object
                .point_in_time_key(ex)
                .ok_or_else(|| type_mismatch(2))?,
        ),
        _ => return Err(type_mismatch(2).into()),
    };
    let scratch = retained_rc_size::<Moment>();
    let retained = ex.measure_retained(scratch, |visit| visit.value(reference))?;
    ex.reserve(retained.saturating_add(scratch))?;
    Ok(Value::Object(Rc::new(Moment {
        date,
        reference: reference.clone(),
        key,
    })))
}

pub fn point_in_time_parts(
    ex: &mut Execution,
    value: &Value,
) -> Result<Option<(BslDate, Value)>, Exception> {
    let Some(point) = of(value) else {
        return Ok(None);
    };
    ex.charge(1)?;
    ex.measure_retained(0, |visit| visit.value(value))?;
    Ok(Some((point.date, point.reference.clone())))
}

impl HostObject for Moment {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(ty())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.value(&self.reference)?;
        Ok(true)
    }
    fn get(&self, ex: &mut Execution, member: MemberId) -> CallResult<Value> {
        ex.charge(1)?;
        match property_name(ty(), member) {
            Some("Date") => Ok(Value::Date(self.date)),
            Some("Ref") => Ok(self.reference.clone()),
            _ => Err(not_implemented(ty(), member).into()),
        }
    }
    fn set(&self, _: &mut Execution, member: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(property_ru(ty(), member))
            .raw()
            .into())
    }
    fn call(&self, ex: &mut Execution, member: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if method_name(ty(), member) != Some("Compare") {
            return Err(not_implemented(ty(), member).into());
        }
        ex.charge(1)?;
        let value = borrowed_arg(ex, args.first())?;
        let other = of(&value).ok_or_else(|| type_mismatch(1))?;
        let order = self
            .date
            .cmp(&other.date)
            .then_with(|| self.key.cmp(&other.key));
        Ok(Value::from(match order {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let other: &dyn Any = other;
        let other = other.downcast_ref::<Moment>()?;
        Some(self.date == other.date && self.key == other.key)
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0)
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let date = Value::String(ex.present(&Value::Date(self.date))?);
        let prefix = bsl_vm::ops::binary(ex, bsl_vm::ops::BinOp::Add, &date, &Value::from("; "))?;
        let text = bsl_vm::ops::binary(ex, bsl_vm::ops::BinOp::Add, &prefix, &self.reference)?;
        match text {
            Value::String(text) => Ok(Some(text)),
            _ => unreachable!(),
        }
    }
}

fn borrowed_arg<'a>(ex: &mut Execution, arg: Option<&'a Arg>) -> CallResult<Cow<'a, Value>> {
    match arg {
        Some(Arg::Val(value)) => Ok(Cow::Borrowed(value)),
        Some(Arg::Place(place)) => {
            let live = place.numeric_backing_bytes(ex);
            Ok(Cow::Owned(place.get_guarded(ex, live)?))
        }
        _ => Ok(Cow::Owned(Value::Undefined)),
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    ex.charge(1)?;
    let date = borrowed_arg(ex, args.first())?;
    let Value::Date(date) = *date else {
        return Err(type_mismatch(1).into());
    };
    let reference = borrowed_arg(ex, args.get(1))?;
    create(ex, date, &reference)
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        ty(),
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
}
