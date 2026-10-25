use super::same_name;
use crate::Application;
use crate::errors::{self, keys};
use crate::meta::type_mismatch;
use crate::model::{ObjId, ObjKind};
use crate::values::refs::{RefValue, as_ref, ref_value};
use bsl_vm::{HostError, Value};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;

pub(super) fn find(app: &Application, obj: ObjId, name: &str) -> Option<usize> {
    app.model()
        .obj(obj)
        .enum_values
        .iter()
        .position(|v| same_name(&v.name, name))
}

pub(super) fn count(app: &Application, obj: ObjId) -> usize {
    app.model().obj(obj).enum_values.len()
}

pub(super) fn value_at(
    app: &Application,
    obj: ObjId,
    i: usize,
) -> Result<Option<Value>, HostError> {
    match app.model().obj(obj).enum_values.get(i) {
        Some(v) => ref_value(
            app,
            RefValue {
                obj,
                id: RefId::from_uuid(v.md),
            },
        )
        .map(Some),
        None => Ok(None),
    }
}

pub(super) fn index_of(app: &Application, obj: ObjId, v: &Value) -> Result<i64, HostError> {
    let r = as_ref(v).ok_or_else(|| type_mismatch(1))?;
    if r.obj != obj && app.model().obj(r.obj).kind == ObjKind::Enum {
        return Err(type_mismatch(1));
    }
    let pos = (r.obj == obj)
        .then(|| {
            app.model()
                .obj(obj)
                .enum_values
                .iter()
                .position(|e| RefId::from_uuid(e.md) == r.id)
        })
        .flatten();
    pos.and_then(|i| i64::try_from(i).ok())
        .ok_or_else(|| HostError::text(&not_found_text(Lang::Ru), &not_found_text(Lang::En)))
}

fn not_found_text(lang: Lang) -> String {
    errors::render(keys::DB_OBJECT_NOT_FOUND, &[], lang)
}
