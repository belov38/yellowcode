use crate::bind::{arg_value, global, type_mismatch};
use bsl_vm::{Registry, Value};

pub(super) fn register(reg: &mut Registry) {
    global(
        reg,
        "УстановитьБезопасныйРежим",
        "SetSafeMode",
        |ex, args| {
            let enabled = match arg_value(ex, args.first())? {
                Value::Bool(value) => value,
                Value::String(_) => true,
                _ => return Err(type_mismatch(1).into()),
            };
            ex.set_safe_mode(enabled)?;
            Ok(Value::Undefined)
        },
    );
    global(
        reg,
        "УстановитьПривилегированныйРежим",
        "SetPrivilegedMode",
        |ex, args| {
            let enabled = match arg_value(ex, args.first())? {
                Value::Bool(value) => value,
                Value::Number(value) => !value.is_zero(),
                Value::String(value) => {
                    bsl_vm::ops::string_to_bool(value.units()).ok_or_else(|| type_mismatch(1))?
                }
                _ => return Err(type_mismatch(1).into()),
            };
            ex.set_privileged_mode(enabled)?;
            Ok(Value::Undefined)
        },
    );
}
