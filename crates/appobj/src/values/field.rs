use super::refs::{RefValue, as_ref, empty_ref, ref_value};
use crate::Application;
use crate::model::{FieldDef, StdAttr, TypeAtom};
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{CallResult, Execution, HostError, Value};
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

pub(crate) const EMPTY_STORAGE: [u8; 18] = [
    0x01, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEF, 0xBB, 0xBF, 0x7B, 0x22, 0x55,
    0x22, 0x7D,
];

pub fn empty_storage() -> Value {
    bsl_stdlib::value_storage_from_bytes(std::sync::Arc::from(&EMPTY_STORAGE[..]))
}

pub(crate) fn empty_storages(row: &mut [FieldValue], hints: &[Option<&FieldDef>]) {
    for (cell, hint) in row.iter_mut().zip(hints) {
        if matches!(
            hint.and_then(|h| h.types.single()),
            Some(TypeAtom::ValueStorage)
        ) && matches!(cell, FieldValue::Storage(b) if b.is_empty())
        {
            *cell = FieldValue::Storage(EMPTY_STORAGE.to_vec());
        }
    }
}

pub(crate) fn storage_of(app: &Application, bytes: &[u8]) -> Result<Value, HostError> {
    if bytes.is_empty() {
        return Ok(empty_storage());
    }
    if let Some(e) = super::storage_check::storage_refusal(app, bytes) {
        return Err(e);
    }
    Ok(bsl_stdlib::value_storage_from_bytes(std::sync::Arc::from(
        bytes,
    )))
}

pub(crate) fn cell_storage_of(
    app: &Application,
    bytes: &[u8],
    cell: [u8; 32],
) -> Result<Value, HostError> {
    if bytes.is_empty() {
        return Ok(empty_storage());
    }
    if let Some(e) = super::storage_check::storage_refusal(app, bytes) {
        return Err(e);
    }
    Ok(bsl_stdlib::value_storage_of_cell(
        std::sync::Arc::from(bytes),
        cell,
    ))
}

pub fn is_empty_storage(v: &Value) -> bool {
    bsl_stdlib::value_storage_bytes(v).is_some_and(|b| *b == EMPTY_STORAGE[..])
}

pub fn default_of(app: &Application, f: &FieldDef) -> Result<Value, HostError> {
    Ok(match f.types.single() {
        Some(TypeAtom::Bool) => Value::Bool(false),
        Some(TypeAtom::Number) => Value::Number(Number::zero()),
        Some(TypeAtom::String) => Value::String(Str::from("")),
        Some(TypeAtom::Date) => Value::Date(BslDate::EMPTY),
        Some(TypeAtom::Uuid) => bsl_stdlib::values::new_uuid([0; 16]),
        Some(TypeAtom::Ref(obj)) => empty_ref(app, *obj)?,
        Some(TypeAtom::ValueStorage) => empty_storage(),
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
        FieldValue::Storage(b) if b.is_empty() => empty_storage(),
        FieldValue::Binary(b) if b.is_empty() => Value::Undefined,
        FieldValue::Storage(b) => storage_of(app, b)?,
        FieldValue::Binary(_) => {
            return Err(HostError::not_implemented("ДвоичныеДанные", &f.name));
        }
        FieldValue::RowVersion(w) => Value::String(Str::from(data_version(w).as_str())),
    })
}

pub(crate) fn is_deferred(f: &FieldDef, v: &FieldValue) -> bool {
    match v {
        FieldValue::Storage(b) => !b.is_empty(),
        FieldValue::Binary(b) => !b.is_empty() && f.std == Some(StdAttr::ValueType),
        _ => false,
    }
}

pub(crate) fn stored_value(
    ex: &mut Execution,
    app: &Application,
    f: &FieldDef,
    v: &FieldValue,
) -> CallResult<Value> {
    match v {
        FieldValue::Binary(b) if !b.is_empty() && f.std == Some(StdAttr::ValueType) => {
            let storage = bsl_stdlib::value_storage_from_bytes(std::sync::Arc::from(b.as_slice()));
            let out = ex.call_method(&storage, "Получить", &mut [], true)?;
            if bsl_stdlib::type_description_of(&out).is_none() {
                return Err(HostError::not_implemented(
                    "Реквизит",
                    &format!("{}: в хранилище не описание типов", f.name),
                )
                .into());
            }
            Ok(out)
        }
        _ => Ok(from_field(app, f, v)?),
    }
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
            if let Some(b) = bsl_stdlib::value_storage_bytes(v) {
                return Ok(FieldValue::Storage(b.to_vec()));
            }
            match bsl_stdlib::values::uuid_bytes(v) {
                Some(b) => Ok(FieldValue::Uuid(Uuid16::from_bytes(b))),
                None => Err(bad(f, "object")),
            }
        }
    }
}
