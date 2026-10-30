use crate::bind::{msg, type_mismatch};
use crate::collections::{
    accounting::retained_rc_size, copy_value, not_implemented, property_name, property_ru,
    type_name,
};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, Registry,
    RetainedMemory, TypeId, TypeImpl, Value,
};
use std::{any::Any, cell::RefCell, rc::Rc, sync::Arc};

fn ty() -> platform_catalog::TypeIdx {
    crate::typedesc::idx("Boundary")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryKind {
    Including,
    Excluding,
}

pub(crate) struct Boundary {
    value: RefCell<Value>,
    kind: BoundaryKind,
}

fn of(value: &Value) -> Option<&Boundary> {
    let actual: &dyn Any = value.as_object()?.as_ref();
    actual.downcast_ref()
}

pub fn new_boundary(
    ex: &mut Execution,
    value: &Value,
    kind: BoundaryKind,
) -> Result<Value, Exception> {
    ex.charge(1)?;
    let scratch = retained_rc_size::<Boundary>();
    let retained = ex.measure_retained(scratch, |visit| visit.value(value))?;
    let live = retained.saturating_add(scratch);
    ex.reserve(live)?;
    let value = copy_value(ex, value, live).map_err(|e| ex.call_error(e))?;
    Ok(Value::Object(Rc::new(Boundary {
        value: RefCell::new(value),
        kind,
    })))
}

pub fn boundary_parts(
    ex: &mut Execution,
    value: &Value,
) -> Result<Option<(Value, BoundaryKind)>, Exception> {
    let Some(boundary) = of(value) else {
        return Ok(None);
    };
    ex.charge(1)?;
    let retained = ex.measure_retained(0, |visit| visit.value(value))?;
    let value = copy_value(ex, &boundary.value.borrow(), retained).map_err(|e| ex.call_error(e))?;
    Ok(Some((value, boundary.kind)))
}

impl Boundary {
    pub(crate) fn take(&self) -> Value {
        self.value.replace(Value::Undefined)
    }
}

impl Drop for Boundary {
    fn drop(&mut self) {
        let value = std::mem::take(self.value.get_mut());
        crate::collections::release(vec![value]);
    }
}

impl HostObject for Boundary {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(ty())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.value(&self.value.borrow())?;
        Ok(true)
    }
    fn get(&self, ex: &mut Execution, member: MemberId) -> CallResult<Value> {
        let retained = ex.measure_retained(0, |visit| {
            visit.bytes(retained_rc_size::<Self>())?;
            visit.value(&self.value.borrow())
        })?;
        self.get_with_retained(ex, member, retained)
    }
    fn get_with_retained(
        &self,
        ex: &mut Execution,
        member: MemberId,
        retained: u64,
    ) -> CallResult<Value> {
        ex.charge(1)?;
        match property_name(ty(), member) {
            Some("Value") => copy_value(ex, &self.value.borrow(), retained),
            Some("BoundaryType") => {
                ex.reserve(retained.saturating_add(retained_rc_size::<crate::enums::EnumValue>()))?;
                Ok(crate::api::system_enum_value(
                    "BoundaryType",
                    match self.kind {
                        BoundaryKind::Including => "Including",
                        BoundaryKind::Excluding => "Excluding",
                    },
                )
                .unwrap_or(Value::Undefined))
            }
            _ => Err(not_implemented(ty(), member).into()),
        }
    }
    fn set(&self, _: &mut Execution, member: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(property_ru(ty(), member))
            .raw()
            .into())
    }
    fn call(&self, _: &mut Execution, member: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(ty(), member).into())
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(type_name(ex, ty())))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let other: &dyn Any = other;
        let other = other.downcast_ref::<Boundary>()?;
        if self.kind != other.kind {
            return Some(false);
        }
        Some(equal_values(&self.value.borrow(), &other.value.borrow()))
    }
    fn charge_equals(&self, ex: &mut Execution, other: &dyn HostObject) -> Result<bool, Exception> {
        let other: &dyn Any = other;
        let Some(other) = other.downcast_ref::<Boundary>() else {
            return Ok(false);
        };
        ex.charge(1)?;
        if self.kind == other.kind {
            charge_equal_values(ex, &self.value.borrow(), &other.value.borrow())?;
        }
        Ok(true)
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0)
    }
}

fn charge_equal_values(ex: &mut Execution, left: &Value, right: &Value) -> Result<(), Exception> {
    let (Value::Object(left), Value::Object(right)) = (left, right) else {
        return bsl_vm::ops::charge_equal(ex, left, right);
    };
    let (mut left, mut right) = (left.clone(), right.clone());
    loop {
        let (a, b): (&dyn Any, &dyn Any) = (left.as_ref(), right.as_ref());
        let (Some(a), Some(b)) = (a.downcast_ref::<Boundary>(), b.downcast_ref::<Boundary>())
        else {
            return bsl_vm::ops::charge_equal(ex, &Value::Object(left), &Value::Object(right));
        };
        ex.charge(1)?;
        if a.kind != b.kind {
            return Ok(());
        }
        let (av, bv) = (a.value.borrow(), b.value.borrow());
        let (Value::Object(next_a), Value::Object(next_b)) = (&*av, &*bv) else {
            return bsl_vm::ops::charge_equal(ex, &av, &bv);
        };
        let next = (next_a.clone(), next_b.clone());
        drop((av, bv));
        (left, right) = next;
    }
}

fn equal_values(left: &Value, right: &Value) -> bool {
    let (Value::Object(left), Value::Object(right)) = (left, right) else {
        return bsl_vm::ops::equal(left, right);
    };
    let (mut left, mut right) = (left.clone(), right.clone());
    loop {
        let (a, b): (&dyn Any, &dyn Any) = (left.as_ref(), right.as_ref());
        let (Some(a), Some(b)) = (a.downcast_ref::<Boundary>(), b.downcast_ref::<Boundary>())
        else {
            return bsl_vm::ops::equal(&Value::Object(left), &Value::Object(right));
        };
        if a.kind != b.kind {
            return false;
        }
        let (av, bv) = (a.value.borrow(), b.value.borrow());
        let (Value::Object(next_a), Value::Object(next_b)) = (&*av, &*bv) else {
            return bsl_vm::ops::equal(&av, &bv);
        };
        let next = (next_a.clone(), next_b.clone());
        drop((av, bv));
        (left, right) = next;
    }
}

fn ctor_live(ex: &mut Execution, args: &[Arg], copies: &[&Value]) -> CallResult<u64> {
    let places = args.iter().take(2).fold(0u64, |bytes, arg| match arg {
        Arg::Place(place) => bytes.saturating_add(place.numeric_backing_bytes(ex)),
        _ => bytes,
    });
    let scratch = retained_rc_size::<Boundary>().saturating_add(places);
    let retained = ex.measure_retained(scratch, |visit| {
        for arg in args.iter().take(2) {
            if let Arg::Val(value) = arg {
                visit.value(value)?;
            }
        }
        for value in copies {
            visit.value(value)?;
        }
        Ok(())
    })?;
    let live = retained.saturating_add(scratch);
    ex.reserve(live)?;
    Ok(live)
}

fn ctor_arg(ex: &mut Execution, arg: Option<&Arg>, live: u64) -> CallResult<Value> {
    match arg {
        Some(Arg::Val(value)) => copy_value(ex, value, live),
        Some(Arg::Place(place)) => Ok(place.get_guarded(ex, live)?),
        _ => Ok(Value::Undefined),
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    ex.charge(1)?;
    let live = ctor_live(ex, args, &[])?;
    let value = ctor_arg(ex, args.first(), live)?;
    let live = ctor_live(ex, args, &[&value])?;
    let kind_value = ctor_arg(ex, args.get(1), live)?;
    let kind = if args.len() < 2 || matches!(args.get(1), Some(Arg::Omitted)) {
        BoundaryKind::Including
    } else {
        let kind = crate::enums::as_enum_value(&kind_value).ok_or_else(|| type_mismatch(2))?;
        if kind.type_idx() != crate::typedesc::idx("BoundaryType") {
            return Err(type_mismatch(2).into());
        }
        match kind.name_en() {
            "Including" => BoundaryKind::Including,
            "Excluding" => BoundaryKind::Excluding,
            _ => return Err(type_mismatch(2).into()),
        }
    };
    ctor_live(ex, args, &[&value, &kind_value])?;
    Ok(Value::Object(Rc::new(Boundary {
        value: RefCell::new(value),
        kind,
    })))
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        ty(),
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
}
