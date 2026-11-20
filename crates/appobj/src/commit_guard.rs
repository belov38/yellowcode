use crate::Application;
use crate::model::{ObjId, ObjKind};
use crate::txn::CallTxn;
use md_model::{MdKind, Value as MdValue};
use platform_catalog::messages::Lang;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Options {
    None,
    Written,
    Refused(String),
}

pub(crate) fn options(app: &Application, obj: ObjId) -> Options {
    let model = app.model();
    let def = model.obj(obj);
    let full = format!("{}.{}", def.kind.name(Lang::Ru), def.name);
    let mut found = Options::None;
    for option in model.config.of_kind(MdKind::FunctionalOption) {
        let Some(MdValue::Ref(place)) = option.prop("Location") else {
            continue;
        };
        let parts: Vec<&str> = place.name.split('.').collect();
        let by_id = place.id.and_then(|id| model.by_uuid(id)) == Some(obj);
        let by_name = match parts.as_slice() {
            [word, name, ..] => ObjKind::from_word(word)
                .and_then(|k| model.find(k, name))
                .is_some_and(|o| o == obj),
            _ => false,
        };
        if !(by_id || by_name) {
            continue;
        }
        let seen = match def.kind {
            ObjKind::Constant => true,
            ObjKind::Catalog => matches!(parts.as_slice(), [_, _, "Attribute" | "Реквизит", _]),
            _ => false,
        };
        let separated = model
            .storage_of(obj)
            .and_then(|st| app.store().storage().try_table(st.main))
            .is_some_and(|ti| !ti.separators.is_empty());
        if !seen || separated {
            return Options::Refused(format!(
                "версия функциональных опций {full} ({})",
                option.name
            ));
        }
        found = Options::Written;
    }
    found
}

pub(crate) fn guard(app: &Application, txn: &CallTxn, obj: ObjId) {
    let def = app.model().obj(obj);
    if crate::history::kept(app, obj)
        && !matches!(
            def.kind,
            ObjKind::Catalog | ObjKind::Document | ObjKind::InformationRegister
        )
    {
        txn.refuse_commit(format!(
            "история данных {}.{}: вид объекта",
            def.kind.name(Lang::Ru),
            def.name
        ));
    }
    match options(app, obj) {
        Options::None => {}
        Options::Refused(what) => txn.refuse_commit(what),
        Options::Written => txn.options_changed(),
    }
}
