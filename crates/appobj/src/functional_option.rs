use crate::Application;
use crate::managers::constants;
use crate::meta::arg_value;
use crate::model::{ObjId, ObjKind, TypeAtom};
use crate::session::call_ctx;
use crate::session_info::outside_call;
use bsl_prim::fold_name;
use bsl_vm::{Arg, CallResult, Category, CategorySet, Execution, HostError, Registry, Value};
use md_model::{MdKind, MdObject, Value as MdValue};
use std::sync::Arc;

fn refused(what: &str) -> HostError {
    HostError::not_implemented(
        "Глобальный контекст",
        &format!("ПолучитьФункциональнуюОпцию: {what}"),
    )
}

fn not_found() -> HostError {
    let mut e = HostError::text(
        "Функциональная опция не найдена",
        "The functional option is not found",
    );
    e.categories = Some(CategorySet::of(&[Category::OtherError]));
    e
}

fn constant_of(app: &Application, option: &MdObject) -> Option<ObjId> {
    let Some(MdValue::Ref(place)) = option.prop("Location") else {
        return None;
    };
    let model = app.model();
    let target = place.id.and_then(|id| model.by_uuid(id)).or_else(|| {
        let (word, name) = place.name.split_once('.')?;
        model.find(ObjKind::from_word(word)?, name)
    })?;
    (model.obj(target).kind == ObjKind::Constant).then_some(target)
}

fn functional_option(app: &Application, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let Value::String(name) = arg_value(ex, args, 0)? else {
        return Err(refused("имя не строка").into());
    };
    let name = fold_name(&name.to_string_lossy());
    let option = app
        .model()
        .config
        .of_kind(MdKind::FunctionalOption)
        .find(|o| fold_name(&o.name) == name)
        .ok_or_else(not_found)?;
    let parameters = matches!(args.get(1), Some(Arg::Val(_) | Arg::Place(_)));
    if !call_ctx(ex).ok_or_else(outside_call)?.sess.user().no_users {
        return Err(refused("сеанс с пользователями").into());
    }
    if parameters {
        let Some((catalog, field)) = attribute_of(app, option) else {
            return Err(refused("параметры функциональной опции").into());
        };
        let given = arg_value(ex, args, 1)?;
        return attribute_value(app, ex, catalog, field, &given);
    }
    let Some(constant) = constant_of(app, option) else {
        return Err(refused("хранение не в константе").into());
    };
    match constants::get(ex, app, constant)? {
        Value::Undefined if is_boolean(app, constant) => Ok(Value::Bool(false)),
        Value::Undefined => Err(refused("константа без строки не Булево").into()),
        value => Ok(value),
    }
}

fn attribute_of(app: &Application, option: &MdObject) -> Option<(ObjId, usize)> {
    let Some(MdValue::Ref(place)) = option.prop("Location") else {
        return None;
    };
    let words: Vec<&str> = place.name.split('.').collect();
    let [word, object, "Attribute", attribute] = words.as_slice() else {
        return None;
    };
    let model = app.model();
    let catalog = model.find(ObjKind::from_word(word)?, object)?;
    let def = model.obj(catalog);
    if def.kind != ObjKind::Catalog {
        return None;
    }
    let field = def.fields.iter().position(|f| {
        f.std.is_none()
            && match (place.id, f.md) {
                (Some(id), Some(md)) => id == md,
                _ => fold_name(&f.name) == fold_name(attribute),
            }
    })?;
    Some((catalog, field))
}

fn attribute_value(
    app: &Application,
    ex: &mut Execution,
    catalog: ObjId,
    field: usize,
    given: &Value,
) -> CallResult<Value> {
    let plain = platform_catalog::catalog()
        .find_type("Структура")
        .is_some_and(|t| given.type_id() == bsl_vm::TypeId::of_catalog(t));
    let pairs = bsl_stdlib::structure_fields(given).filter(|_| plain);
    let Some([(key, value)]) = pairs.as_deref() else {
        return Err(refused("параметры функциональной опции").into());
    };
    let model = app.model();
    let used = model
        .config
        .of_kind(MdKind::FunctionalOptionsParameter)
        .filter(|p| fold_name(&p.name) == fold_name(key))
        .any(|p| match p.prop("Use") {
            Some(MdValue::RefList(list)) => list.iter().any(|r| {
                r.id.and_then(|id| model.by_uuid(id)).or_else(|| {
                    let (word, name) = r.name.split_once('.')?;
                    model.find(ObjKind::from_word(word)?, name)
                }) == Some(catalog)
            }),
            _ => false,
        });
    if !used {
        return Err(refused("параметры функциональной опции").into());
    }
    let def = model.obj(catalog);
    let Some(r) = crate::values::refs::as_ref(value).filter(|r| r.obj == catalog) else {
        return Err(refused(&format!("параметр {key} не ссылка Catalog.{}", def.name)).into());
    };
    if r.is_empty() {
        return Err(refused(&format!("параметр {key} пуст")).into());
    }
    let Some(f) = def
        .fields
        .get(field)
        .filter(|f| f.types.single() == Some(&TypeAtom::Bool))
    else {
        return Err(refused("реквизит не Булево").into());
    };
    let name = f.name.clone();
    ex.get_member(value, &name).map_err(Into::into)
}

fn is_boolean(app: &Application, constant: ObjId) -> bool {
    app.model()
        .obj(constant)
        .value
        .as_ref()
        .is_some_and(|field| field.types.single() == Some(&TypeAtom::Bool))
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let app = app.clone();
    reg.global_fn(
        "ПолучитьФункциональнуюОпцию",
        "GetFunctionalOption",
        Arc::new(move |ex, args| functional_option(&app, ex, args)),
    );
}
