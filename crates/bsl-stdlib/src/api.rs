pub use crate::collections::{ValueTable, ValueTree};
pub use crate::values::boundary::{BoundaryKind, boundary_parts, new_boundary};
pub use crate::values::moment::{new_point_in_time, point_in_time_parts};
use bsl_vm::{Exception, Execution, Value};
use std::any::Any;

pub fn new_fixed_array(ex: &mut Execution, source: &Value) -> Result<Value, Exception> {
    let source = crate::collections::copy_value(
        ex,
        source,
        crate::collections::accounting::retained_shallow_bytes(source),
    )
    .map_err(|e| ex.call_error(e))?;
    ex.construct(
        crate::typedesc::idx("FixedArray"),
        &mut [bsl_vm::Arg::Val(source)],
    )
}

pub fn new_fixed_structure(ex: &mut Execution, source: &Value) -> Result<Value, Exception> {
    let source = crate::collections::copy_value(
        ex,
        source,
        crate::collections::accounting::retained_shallow_bytes(source),
    )
    .map_err(|e| ex.call_error(e))?;
    ex.construct(
        crate::typedesc::idx("FixedStructure"),
        &mut [bsl_vm::Arg::Val(source)],
    )
}

pub fn system_enum_value(type_name: &str, value_name: &str) -> Option<Value> {
    crate::enums::enum_value(
        platform_catalog::catalog().find_type(type_name)?,
        value_name,
    )
}

pub fn new_value_table(ex: &mut Execution) -> Result<Value, Exception> {
    ex.construct(crate::typedesc::idx("ValueTable"), &mut [])
}

pub fn new_value_tree(ex: &mut Execution) -> Result<Value, Exception> {
    ex.construct(crate::typedesc::idx("ValueTree"), &mut [])
}

pub fn as_value_table(value: &Value) -> Option<&ValueTable> {
    let actual: &dyn Any = value.as_object()?.as_ref();
    actual.downcast_ref()
}

pub fn as_value_tree(value: &Value) -> Option<&ValueTree> {
    let actual: &dyn Any = value.as_object()?.as_ref();
    actual.downcast_ref()
}
