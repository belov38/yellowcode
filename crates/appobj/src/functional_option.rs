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
    if matches!(args.get(1), Some(Arg::Val(_) | Arg::Place(_))) {
        return Err(refused("параметры функциональной опции").into());
    }
    if !call_ctx(ex).ok_or_else(outside_call)?.sess.user().no_users {
        return Err(refused("сеанс с пользователями").into());
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
