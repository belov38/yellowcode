use bsl_vm::{Registry, Value};
use std::sync::Arc;

pub(super) fn register(registry: &mut Registry) {
    registry.global_fn(
        "ОбновитьПовторноИспользуемыеЗначения",
        "RefreshReusableValues",
        Arc::new(|ex, _| {
            ex.refresh_reusable_values();
            Ok(Value::Undefined)
        }),
    );
}
