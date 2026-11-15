use crate::Application;
use crate::model::{ObjId, ObjKind};
use crate::txn::CallTxn;
use md_model::{MdKind, Value as MdValue};
use platform_catalog::messages::Lang;

pub(crate) fn uncommitted(app: &Application, obj: ObjId) -> Option<String> {
    let model = app.model();
    let def = model.obj(obj);
    let full = format!("{}.{}", def.kind.name(Lang::Ru), def.name);
    let md = model.config.object(def.md)?;
    if matches!(md.prop("DataHistory"), Some(MdValue::Text(t)) if t == "Use") {
        return Some(format!("история данных {full}"));
    }
    if def.kind == ObjKind::Constant
        && def
            .value
            .as_ref()
            .is_some_and(|f| matches!(f.types.single(), Some(crate::model::TypeAtom::ValueStorage)))
    {
        return Some(format!("хранилище значения {full}"));
    }
    for option in model.config.of_kind(MdKind::FunctionalOption) {
        let Some(MdValue::Ref(place)) = option.prop("Location") else {
            continue;
        };
        let by_id = place.id.and_then(|id| model.by_uuid(id)) == Some(obj);
        let by_name = {
            let mut parts = place.name.splitn(3, '.');
            match (parts.next(), parts.next()) {
                (Some(word), Some(name)) => ObjKind::from_word(word)
                    .and_then(|k| model.find(k, name))
                    .is_some_and(|o| o == obj),
                _ => false,
            }
        };
        if by_id || by_name {
            return Some(format!(
                "версия функциональных опций {full} ({})",
                option.name
            ));
        }
    }
    None
}

pub(crate) fn guard(app: &Application, txn: &CallTxn, obj: ObjId) {
    if let Some(what) = uncommitted(app, obj) {
        txn.refuse_commit(what);
    }
}
