use crate::bind::{arg_value, is_given, type_mismatch};
use crate::enums::as_enum_value;
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};
use platform_catalog::catalog;

pub(crate) mod binary;
pub(crate) mod data;
pub(crate) mod dates;
pub(crate) mod format;
pub(crate) mod message;
mod modes;
pub(crate) mod numbers;
mod regex;
mod reuse;
pub(crate) mod strings;
pub(crate) mod tz;

pub(crate) fn register(reg: &mut Registry) {
    reg.global_fn(
        "ПривилегированныйРежим",
        "PrivilegedMode",
        std::sync::Arc::new(|ex, _| Ok(Value::Bool(ex.effective_flags().privileged))),
    );
    reg.global_fn(
        "БезопасныйРежим",
        "SafeMode",
        std::sync::Arc::new(|ex, _| Ok(Value::Bool(ex.effective_flags().safe_mode))),
    );
    strings::register(reg);
    regex::register(reg);
    numbers::register(reg);
    data::register(reg);
    binary::register(reg);
    dates::register(reg);
    tz::register(reg);
    format::register(reg);
    message::register(reg);
    reuse::register(reg);
    modes::register(reg);
}

pub(crate) fn str_value(units: Vec<u16>) -> Value {
    Value::String(Str::from_vec(units))
}

pub(crate) fn int_value(n: i64) -> Value {
    Value::Number(bsl_prim::Number::from_i64(n))
}

pub(crate) fn reserve_units(ex: &mut Execution, units: usize, factor: usize) -> CallResult<()> {
    let bytes = units.saturating_mul(factor).saturating_mul(2);
    ex.charge(u64::try_from(units).unwrap_or(u64::MAX) / 64)?;
    ex.reserve(u64::try_from(bytes).unwrap_or(u64::MAX))?;
    Ok(())
}

pub(crate) fn enum_arg(
    ex: &mut Execution,
    a: Option<&Arg>,
    ty: &str,
    param: usize,
) -> CallResult<Option<&'static str>> {
    if !is_given(a) {
        return Ok(None);
    }
    let v = arg_value(ex, a)?;
    enum_of(&v, ty, param)
}

pub(crate) fn enum_of(v: &Value, ty: &str, param: usize) -> CallResult<Option<&'static str>> {
    if matches!(v, Value::Undefined) {
        return Ok(None);
    }
    match as_enum_value(v) {
        Some(e) if catalog().find_type(ty) == Some(e.type_idx()) => Ok(Some(e.name_ru())),
        _ => Err(type_mismatch(param).into()),
    }
}
