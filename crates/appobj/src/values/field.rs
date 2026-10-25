use super::refs::{RefValue, as_ref, empty_ref, ref_value};
use crate::Application;
use crate::model::{FieldDef, TypeAtom};
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{HostError, Value};
use ibschema::fields::FieldValue;
use ibschema::values::Uuid16;
use query_ir::convert::{data_version, date_from_q, date_to_q, number_from_q, number_to_q};

fn bad(f: &FieldDef, what: &str) -> HostError {
    HostError::text(
        &format!(
            "Значение не может быть сохранено в реквизите ({}): {what}",
            f.name
        ),
        &format!(
            "The value cannot be stored in the attribute ({}): {what}",
            f.name
        ),
    )
}

pub fn default_of(app: &Application, f: &FieldDef) -> Result<Value, HostError> {
    Ok(match f.types.single() {
        Some(TypeAtom::Bool) => Value::Bool(false),
        Some(TypeAtom::Number) => Value::Number(Number::zero()),
        Some(TypeAtom::String) => Value::String(Str::from("")),
        Some(TypeAtom::Date) => Value::Date(BslDate::EMPTY),
        Some(TypeAtom::Uuid) => bsl_stdlib::values::new_uuid([0; 16]),
        Some(TypeAtom::Ref(obj)) => empty_ref(app, *obj)?,
        _ => Value::Undefined,
    })
}

pub fn from_field(app: &Application, f: &FieldDef, v: &FieldValue) -> Result<Value, HostError> {
    Ok(match v {
        FieldValue::Null => default_of(app, f)?,
        FieldValue::Undefined => Value::Undefined,
        FieldValue::Bool(b) => Value::Bool(*b),
        FieldValue::Number(n) => Value::Number(number_to_q(n)),
        FieldValue::Date(d) => Value::Date(date_to_q(d)),
        FieldValue::String(s) => Value::String(Str::from(s.as_str())),
        FieldValue::Uuid(u) => bsl_stdlib::values::new_uuid(*u.as_bytes()),
        FieldValue::Ref { table, id } => {
            let obj = app.model().obj_of_ref_number(*table).ok_or_else(|| {
                HostError::text(
                    &format!(
                        "Неизвестный номер таблицы ссылки: {table} (реквизит {})",
                        f.name
                    ),
                    &format!(
                        "Unknown reference table number: {table} (attribute {})",
                        f.name
                    ),
                )
            })?;
            ref_value(app, RefValue { obj, id: *id })?
        }
        FieldValue::Storage(_) => {
            return Err(HostError::not_implemented("ХранилищеЗначения", &f.name));
        }
        FieldValue::Binary(_) => {
            return Err(HostError::not_implemented("ДвоичныеДанные", &f.name));
        }
        FieldValue::RowVersion(w) => Value::String(Str::from(data_version(w).as_str())),
    })
}

pub fn to_field(app: &Application, f: &FieldDef, v: &Value) -> Result<FieldValue, HostError> {
    match v {
        Value::Undefined => Ok(FieldValue::Undefined),
        Value::Null => Ok(FieldValue::Null),
        Value::Bool(b) => Ok(FieldValue::Bool(*b)),
        Value::Number(n) => number_from_q(n)
            .map(FieldValue::Number)
            .ok_or_else(|| bad(f, "number")),
        Value::Date(d) => date_from_q(*d)
            .map(FieldValue::Date)
            .ok_or_else(|| bad(f, "date")),
        Value::String(s) => Ok(FieldValue::String(s.to_string_lossy())),
        Value::Type(_) => Err(bad(f, "type")),
        Value::Object(_) => {
            if let Some(r) = as_ref(v) {
                let table = app
                    .model()
                    .storage_of(r.obj)
                    .and_then(|s| s.ref_number)
                    .ok_or_else(|| bad(f, "reference without a table number"))?;
                return Ok(FieldValue::Ref { table, id: r.id });
            }
            match bsl_stdlib::values::uuid_bytes(v) {
                Some(b) => Ok(FieldValue::Uuid(Uuid16::from_bytes(b))),
                None => Err(bad(f, "object")),
            }
        }
    }
}
