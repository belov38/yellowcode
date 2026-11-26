use crate::model::{Posting, RecordsDeletion, RecordsWriting};
use crate::objects::ObjectInstance;
use crate::values::refs::RefValue;
use bsl_vm::{CallResult, Execution, HostError, Value};
use std::rc::Rc;

fn refuse(inst: &ObjectInstance, what: &str) -> bsl_vm::CallError {
    HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}")).into()
}

pub(crate) fn check(inst: &ObjectInstance, posted: bool, marked: bool) -> CallResult<()> {
    let def = inst.app.model().obj(inst.obj);
    if def.behavior.posting == Posting::Deny {
        return Err(refuse(inst, "проведение документа с запретом проведения"));
    }
    if marked {
        return Err(refuse(inst, "проведение помеченного на удаление"));
    }
    if !posted && !inst.state.borrow().is_new {
        return Err(refuse(inst, "проведение записанного документа"));
    }
    if posted && crate::regread::sets_touched(inst)? {
        return Err(refuse(inst, "перепроведение с прочитанными движениями"));
    }
    if def.behavior.register_records_writing != RecordsWriting::WriteSelected {
        return Err(refuse(
            inst,
            "запись движений при проведении не WriteSelected",
        ));
    }
    if def.behavior.register_records_deletion != RecordsDeletion::AutoDeleteOnUnpost {
        return Err(refuse(inst, "удаление движений не AutoDeleteOnUnpost"));
    }
    if crate::events::handled(inst, crate::events::Event::OnWrite) {
        return Err(refuse(inst, "обработчик ПриЗаписи при проведении"));
    }
    no_changed_sets(inst)
}

pub(crate) fn no_changed_sets(inst: &ObjectInstance) -> CallResult<()> {
    if crate::regread::any_set_changed(inst)? {
        return Err(refuse(inst, "движения документа изменены до проведения"));
    }
    Ok(())
}

pub(crate) fn post(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    mode: &Value,
    r: RefValue,
) -> CallResult<Vec<Value>> {
    let privileged = inst
        .app
        .model()
        .obj(inst.obj)
        .behavior
        .post_in_privileged_mode;
    let movements = inst.movements_of(ex)?;
    let repost = !inst
        .state
        .borrow()
        .written
        .as_ref()
        .is_some_and(|w| w.was_new);
    crate::regread::posting_begin(inst, &movements, r, repost)?;
    let handled = crate::events::posting(ex, inst, mode, privileged);
    crate::regread::posting_end(&movements, handled.is_err());
    handled?;
    let written = crate::regread::posting_finish(ex, inst, &movements);
    crate::regread::posting_end(&movements, true);
    written
}

fn refuse_in(inst: &ObjectInstance, method: &str, what: &str) -> bsl_vm::CallError {
    HostError::not_implemented(&inst.type_name(), &format!("{method}: {what}")).into()
}

pub(crate) fn undo_check(
    inst: &ObjectInstance,
    method: &str,
    handler: crate::events::Event,
) -> CallResult<()> {
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let refuse = |what: &str| refuse_in(inst, method, what);
    if def.behavior.register_records_deletion != RecordsDeletion::AutoDeleteOnUnpost {
        return Err(refuse("удаление движений не AutoDeleteOnUnpost"));
    }
    if crate::events::handled(inst, crate::events::Event::UndoPosting) {
        return Err(refuse("обработчик ОбработкаУдаленияПроведения"));
    }
    if crate::events::handled(inst, handler) {
        return Err(refuse(&format!(
            "обработчик {} при отмене проведения",
            handler.names().0
        )));
    }
    let model = app.model();
    for md in &def.behavior.register_records {
        let reg = model
            .by_uuid(*md)
            .map(|o| model.obj(o).kind)
            .unwrap_or(crate::model::ObjKind::Catalog);
        if reg != crate::model::ObjKind::AccumulationRegister {
            return Err(refuse("движения документа не в регистре накопления"));
        }
    }
    if inst.loading() {
        return Err(refuse("ОбменДанными.Загрузка документа"));
    }
    if inst.state.borrow().modified.get() {
        return Err(refuse("объект изменён до отмены проведения"));
    }
    untouched_sets(inst, "до отмены проведения")
        .map_err(|_| refuse("движения документа прочитаны или изменены до отмены проведения"))
}

pub(crate) fn untouched_sets(inst: &ObjectInstance, when: &str) -> CallResult<()> {
    if crate::regread::sets_touched(inst)? {
        return Err(refuse_in(
            inst,
            "Записать",
            &format!("движения документа прочитаны или изменены {when}"),
        ));
    }
    Ok(())
}

pub(crate) fn unpost_check(inst: &ObjectInstance, posted: bool, marked: bool) -> CallResult<()> {
    let def = inst.app.model().obj(inst.obj);
    let refuse = |what: &str| refuse_in(inst, "Записать", what);
    if def.behavior.posting == Posting::Deny {
        return Err(refuse("отмена проведения документа с запретом проведения"));
    }
    if inst.state.borrow().is_new {
        return Err(refuse("отмена проведения нового документа"));
    }
    if !posted {
        return Err(refuse("отмена проведения непроведённого документа"));
    }
    if marked {
        return Err(refuse("отмена проведения помеченного на удаление"));
    }
    undo_check(inst, "Записать", crate::events::Event::OnWrite)?;
    before_write_check(inst, "Записать", "отмене проведения")
}

pub(crate) fn before_write_check(
    inst: &ObjectInstance,
    method: &str,
    when: &str,
) -> CallResult<()> {
    if crate::events::handled(inst, crate::events::Event::BeforeWrite)
        && crate::events::modes_seen(inst)
    {
        return Err(refuse_in(
            inst,
            method,
            &format!("обработчик ПередЗаписью при {when}"),
        ));
    }
    Ok(())
}

pub(crate) fn unpost(
    ex: &mut Execution,
    inst: &ObjectInstance,
    txn: &Rc<crate::txn::CallTxn>,
    r: RefValue,
) -> CallResult<()> {
    let app = inst.app.clone();
    let regs: Vec<crate::model::ObjId> = {
        let model = app.model();
        model
            .obj(inst.obj)
            .behavior
            .register_records
            .iter()
            .filter_map(|md| model.by_uuid(*md))
            .collect()
    };
    for reg in regs {
        crate::regread::remove_records(ex, &app, txn, reg, r)?;
    }
    Ok(())
}
