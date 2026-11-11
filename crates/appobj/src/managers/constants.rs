use crate::Application;
use crate::model::{FieldDef, ObjId, TypeAtom};
use crate::read::{admit_data_read, internal, read_view, store_call_error};
use crate::read_budget as budget;
use crate::txn::CallTxn;
use crate::values::field::from_field;
use crate::values::refs::empty_ref;
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{CallResult, Execution, HostError, Value};
use ibschema::fields::FieldValue;
use std::sync::Arc;

fn refusal(what: &str) -> HostError {
    HostError::not_implemented("КонстантаМенеджер", &format!("Получить: {what}"))
}

fn default_value(ex: &mut Execution, app: &Application, field: &FieldDef) -> CallResult<Value> {
    let types = &field.types;
    Ok(match types.single() {
        Some(TypeAtom::Bool) => Value::Bool(false),
        Some(TypeAtom::Number) => Value::Number(Number::zero()),
        Some(TypeAtom::String) => {
            let pad = types
                .string
                .filter(|q| q.fixed)
                .map_or(0, |q| q.length as usize);
            budget::reserve(ex, (pad as u64).saturating_mul(4))?;
            Value::String(Str::from(" ".repeat(pad).as_str()))
        }
        Some(TypeAtom::Date) => Value::Date(BslDate::EMPTY),
        Some(TypeAtom::Uuid) => bsl_stdlib::values::new_uuid([0; 16]),
        Some(TypeAtom::Ref(obj)) => empty_ref(app, *obj)?,
        Some(TypeAtom::ValueStorage) => bsl_stdlib::new_value_storage(ex, &Value::Undefined, None)?,
        _ => return Err(refusal("составной тип без строки").into()),
    })
}

pub(crate) fn get(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    admit_data_read(ex)?;
    let model = app.model();
    let (Some(st), Some(field)) = (model.storage_of(obj), model.obj(obj).value.as_ref()) else {
        return Err(internal("the constant is not bound to a table").into());
    };
    let Some(value) = st.value else {
        return Err(internal("the table of the constant has no field of the value").into());
    };
    let table = app
        .store()
        .storage()
        .try_table(st.main)
        .ok_or_else(|| internal("no table"))?;
    if !table.separators.is_empty() && app.options().area().is_none() {
        return Err(refusal("Разделение данных").into());
    }

    let call_txn = CallTxn::of_call(ex);
    let mut view = read_view(ex, call_txn.as_deref(), app)?;
    let remaining = ex
        .options()
        .memory_limit
        .unwrap_or(u64::MAX)
        .saturating_sub(ex.retained_workspace_bytes());
    let memory = view
        .read_memory(remaining)
        .map_err(|e| store_call_error(ex, e))?;
    let txn = view.txn();
    let mut rows = txn.scan(st.main).map_err(|e| store_call_error(ex, e))?;
    let Some(first) = rows.next() else {
        return if table.separators.is_empty() {
            Ok(Value::Undefined)
        } else {
            default_value(ex, app, field)
        };
    };
    let (_, row) = first.map_err(|e| store_call_error(ex, e))?;
    match rows.next() {
        None => {}
        Some(Ok(_)) => {
            return Err(internal("the table of the constant holds more than one row").into());
        }
        Some(Err(e)) => return Err(store_call_error(ex, e)),
    }

    let cell = row
        .get(usize::from(value.0))
        .ok_or_else(|| internal("the row is shorter than its table"))?;
    if let FieldValue::Storage(bytes) = cell {
        budget::admit(
            ex,
            &memory,
            budget::stored_row(&row).saturating_add(budget::storage_copy(bytes)),
        )?;
        return Ok(bsl_stdlib::value_storage_from_bytes(Arc::from(
            bytes.as_slice(),
        )));
    }
    budget::admit(
        ex,
        &memory,
        budget::stored_row(&row).saturating_add(budget::conversion_peak(cell)),
    )?;
    Ok(from_field(app, field, cell)?)
}
