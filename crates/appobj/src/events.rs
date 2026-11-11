use crate::Application;
use crate::errors::{self, keys};
use crate::model::{ObjDef, StdAttr, TypeAtom};
use crate::objects::ObjectInstance;
use crate::session::call_ctx;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallError, CallResult, Category, CategorySet, Cause, ErrorInfo, Exception, Execution,
    HostError, ModuleInstanceRef, Place, UserMessage, Value,
};
use md_model::ScriptVariant;
use platform_catalog::messages::Lang;
use std::rc::Rc;

const MODULE_WEIGHT: u32 = 991;

const SUBSCRIPTION_WEIGHT: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    BeforeWrite,
    OnWrite,
    NewCode,
    BeforeDelete,
    OnCopy,
    Filling,
    FillCheck,
}

impl Event {
    pub(crate) const fn names(self) -> (&'static str, &'static str) {
        match self {
            Event::BeforeWrite => ("ПередЗаписью", "BeforeWrite"),
            Event::OnWrite => ("ПриЗаписи", "OnWrite"),
            Event::NewCode => ("ПриУстановкеНовогоКода", "OnSetNewCode"),
            Event::BeforeDelete => ("ПередУдалением", "BeforeDelete"),
            Event::OnCopy => ("ПриКопировании", "OnCopy"),
            Event::Filling => ("ОбработкаЗаполнения", "Filling"),
            Event::FillCheck => ("ОбработкаПроверкиЗаполнения", "FillCheckProcessing"),
        }
    }

    const fn disables_safe_mode(self) -> bool {
        !matches!(self, Event::FillCheck)
    }
}

pub(crate) enum Param {
    Val(Value),
    Ref(Place),
}

fn args_of(source: Option<&Value>, params: &[Param]) -> Vec<Arg> {
    source
        .map(|s| Arg::Val(s.clone()))
        .into_iter()
        .chain(params.iter().map(|p| match p {
            Param::Val(v) => Arg::Val(v.clone()),
            Param::Ref(place) => Arg::Place(place.clone()),
        }))
        .collect()
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub(crate) fn module_handler(inst: &ObjectInstance, event: Event) -> Option<&'static str> {
    let module = inst.module()?;
    let (ru, en) = event.names();
    [ru, en]
        .into_iter()
        .find(|n| module.module().method(n).is_some())
}

fn subscribed(app: &Application, def: &ObjDef, event: Event) -> Vec<(String, String)> {
    let kind_word = format!("cfg:{}Object", def.kind.name(Lang::En));
    app.model()
        .subscriptions
        .iter()
        .filter(|s| {
            s.event == event.names().1
                && s.source.atoms.iter().any(|a| match a {
                    TypeAtom::Platform(q) => {
                        q == &kind_word
                            || q.strip_prefix(kind_word.as_str())
                                .and_then(|rest| rest.strip_prefix('.'))
                                .is_some_and(|name| fold_name(name) == fold_name(&def.name))
                    }
                    _ => false,
                })
        })
        .map(|s| s.handler.clone())
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HandlerMode {
    masked: bool,
    shown: bool,
}

struct HandlerFrame {
    flag_before: Option<bool>,
    masked: bool,
}

impl HandlerFrame {
    fn enter(ex: &mut Execution) -> HandlerFrame {
        let flag_before =
            call_ctx(ex).map(|c| std::mem::replace(&mut c.sess.safe_mode_disabled, true));
        let masked = mask_explicit_mode(ex);
        if let Some(c) = call_ctx(ex) {
            c.handler_modes.push(HandlerMode {
                masked,
                shown: false,
            });
        }
        HandlerFrame {
            flag_before,
            masked,
        }
    }

    fn leave(self, ex: &mut Execution) {
        if let Some(c) = call_ctx(ex) {
            c.handler_modes.pop();
            if let Some(before) = self.flag_before {
                c.sess.safe_mode_disabled = before;
            }
        }
        if self.masked {
            let _ = ex.set_safe_mode(true);
        }
    }
}

fn mask_explicit_mode(ex: &mut Execution) -> bool {
    if !ex.effective_flags().safe_mode || ex.set_safe_mode(false).is_err() {
        return false;
    }
    if ex.effective_flags().safe_mode {
        let _ = ex.set_safe_mode(true);
        return false;
    }
    true
}

pub(crate) fn flag_taken_back(ex: &mut Execution) -> CallResult<()> {
    let show = call_ctx(ex)
        .and_then(|c| c.handler_modes.last_mut())
        .is_some_and(|m| m.masked && !std::mem::replace(&mut m.shown, true));
    if show {
        ex.set_safe_mode(true)?;
    }
    Ok(())
}

fn failed_text(variant: ScriptVariant, event: Event, lang: Lang) -> String {
    let (ru, en) = event.names();
    let name = match variant {
        ScriptVariant::Russian => ru,
        ScriptVariant::English => en,
    };
    format!(
        "{} - '{name}'",
        errors::render(keys::HANDLER_FAILED, &[], lang)
    )
}

fn handler_failed(app: &Application, event: Event, cause: Rc<ErrorInfo>) -> HostError {
    let script = app.model().script;
    let mut e = HostError::text(
        &failed_text(script, event, Lang::Ru),
        &failed_text(script, event, Lang::En),
    );
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::OtherError,
    ]));
    e.caused_by(Cause::Bsl(cause))
}

fn pad_missing(module: &ModuleInstanceRef, name: &str, args: &mut Vec<Arg>) {
    let Some((_, method)) = module.module().method(name) else {
        return;
    };
    for param in method.params.iter().skip(args.len()) {
        if param.default.is_some() {
            break;
        }
        args.push(Arg::Val(Value::Undefined));
    }
}

fn call(
    ex: &mut Execution,
    app: &Application,
    event: Event,
    module: &ModuleInstanceRef,
    name: &str,
    mut args: Vec<Arg>,
    weight: u32,
) -> CallResult<()> {
    pad_missing(module, name, &mut args);
    let frame = event.disables_safe_mode().then(|| HandlerFrame::enter(ex));
    let outcome = ex.call_weighted(module, name, &mut args, weight);
    if let Some(frame) = frame {
        frame.leave(ex);
    }
    match outcome {
        Ok(_) => Ok(()),
        Err(Exception::Catchable(info)) => Err(handler_failed(app, event, info).into()),
        Err(fatal) => Err(CallError::Exc(fatal)),
    }
}

pub(crate) fn run(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    event: Event,
    params: &[Param],
    cancel: Option<&Place>,
) -> CallResult<bool> {
    let app = inst.app.clone();
    let stopped = || {
        cancel
            .and_then(Place::temp_value)
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    };
    if let (Some(module), Some(name)) = (inst.module(), module_handler(inst, event)) {
        call(
            ex,
            &app,
            event,
            &module,
            name,
            args_of(None, params),
            MODULE_WEIGHT,
        )?;
        if stopped() {
            return Ok(true);
        }
    }
    let source = Value::Object(inst.clone());
    for (module_name, method) in subscribed(&app, app.model().obj(inst.obj), event) {
        let module = app
            .modules()
            .module_ref(&format!("ОбщийМодуль.{module_name}.Модуль"))
            .ok_or_else(|| internal(&format!("no common module {module_name}")))?;
        let instance = app.instance_in_call(ex, module).map_err(CallError::Exc)?;
        call(
            ex,
            &app,
            event,
            &instance,
            &method,
            args_of(Some(&source), params),
            SUBSCRIPTION_WEIGHT,
        )?;
    }
    Ok(stopped())
}

fn presentation(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<String> {
    let app = &inst.app;
    let def = app.model().obj(inst.obj);
    if crate::present::has_handler(ex, app, inst.obj) {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "представление объекта с обработчиком модуля менеджера в сообщении об отказе",
        )
        .into());
    }
    let position = |a: StdAttr| def.fields.iter().position(|f| f.std == Some(a));
    let (description, code) = (position(StdAttr::Description), position(StdAttr::Code));
    let by_code = code.filter(|_| def.behavior.present_by_code || description.is_none());
    let Some(i) = by_code.or(description) else {
        return Ok("<>".to_string());
    };
    let value = inst.attr(ex, i)?;
    let text = ex
        .present(&value)
        .map_err(CallError::Exc)?
        .to_string_lossy();
    Ok(if by_code.is_none() && text.is_empty() {
        "<>".to_string()
    } else {
        text
    })
}

pub(crate) fn cant(
    ex: &mut Execution,
    inst: &ObjectInstance,
    key: &'static str,
) -> CallResult<HostError> {
    let app = &inst.app;
    let def = app.model().obj(inst.obj);
    let synonym = crate::types::synonym(&def.synonym, &def.name, &app.model().lang_code);
    let tail = if inst.state.borrow().r.is_some() {
        format!(" \"{} ({synonym})\"!", presentation(ex, inst)?)
    } else {
        format!(": \"{synonym}\"!")
    };
    let text = |lang| format!("{}{tail}", errors::render(key, &[], lang));
    let mut e = HostError::text(&text(Lang::Ru), &text(Lang::En));
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    Ok(e)
}

pub(crate) fn writing(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    event: Event,
) -> CallResult<()> {
    let cancel = Place::temp(Value::Bool(false));
    if run(
        ex,
        inst,
        event,
        &[Param::Ref(cancel.clone())],
        Some(&cancel),
    )? {
        return Err(cant(ex, inst, keys::REFERENCE_CANT_SAVE)?.into());
    }
    Ok(())
}

#[allow(dead_code, reason = "called by the delete (G)")]
pub(crate) fn before_delete(ex: &mut Execution, inst: &Rc<ObjectInstance>) -> CallResult<()> {
    let cancel = Place::temp(Value::Bool(false));
    if run(
        ex,
        inst,
        Event::BeforeDelete,
        &[Param::Ref(cancel.clone())],
        Some(&cancel),
    )? {
        return Err(cant(ex, inst, keys::REFERENCE_CANT_DELETE)?.into());
    }
    Ok(())
}

pub(crate) fn filling(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    data: Value,
    text: &Place,
    standard: &Place,
) -> CallResult<()> {
    run(
        ex,
        inst,
        Event::Filling,
        &[
            Param::Val(data),
            Param::Ref(text.clone()),
            Param::Ref(standard.clone()),
        ],
        None,
    )
    .map(drop)
}

pub(crate) fn on_copy(
    ex: &mut Execution,
    copy: &Rc<ObjectInstance>,
    source: &Rc<ObjectInstance>,
) -> CallResult<()> {
    run(
        ex,
        copy,
        Event::OnCopy,
        &[Param::Val(Value::Object(source.clone()))],
        None,
    )
    .map(drop)
}

fn is_space(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0A | 0x0D | 0x0C | 0x0B | 0xA0 | 0x3000)
}

fn filled(ex: &mut Execution, v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(_) | Value::Type(_) => true,
        Value::Number(n) => !n.is_zero(),
        Value::String(s) => !s.units().iter().all(|&c| is_space(c)),
        Value::Date(d) => *d != bsl_prim::BslDate::EMPTY,
        Value::Object(o) => !o.is_empty_value() && o.iter_len(ex).is_none_or(|n| n > 0),
    }
}

fn shown(app: &Application, synonym: &md_model::LocalString, name: &str) -> String {
    crate::types::synonym(synonym, name, &app.model().lang_code)
}

pub(crate) fn check_filling(ex: &mut Execution, inst: &Rc<ObjectInstance>) -> CallResult<bool> {
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let mut paths: Vec<String> = def
        .fields
        .iter()
        .enumerate()
        .filter(|(i, f)| f.fill_check && !inst.hidden(*i))
        .map(|(_, f)| f.name.clone())
        .collect();
    for t in &def.tabular {
        paths.extend(
            t.fields
                .iter()
                .filter(|f| f.fill_check)
                .map(|f| format!("{}.{}", t.name, f.name)),
        );
    }
    crate::read_budget::reserve(
        ex,
        paths
            .iter()
            .fold(256u64, |n, p| n.saturating_add(64 + 2 * p.len() as u64)),
    )?;
    let list = Place::temp(bsl_stdlib::new_array(
        paths.iter().map(|p| Value::from(p.as_str())).collect(),
    ));
    let cancel = Place::temp(Value::Bool(false));
    let params = [Param::Ref(cancel.clone()), Param::Ref(list.clone())];
    let refused = run(ex, inst, Event::FillCheck, &params, Some(&cancel))?;
    let wanted = list
        .temp_value()
        .and_then(|v| bsl_stdlib::array_items(&v))
        .unwrap_or_default();
    let mut ok = !refused;
    for path in &wanted {
        let Value::String(path) = path else {
            return Err(HostError::not_implemented(
                &inst.type_name(),
                "ПроверитьЗаполнение: элемент ПроверяемыеРеквизиты не строка",
            )
            .into());
        };
        ok &= check_path(ex, inst, &path.to_string_lossy())?;
    }
    Ok(ok)
}

fn check_path(ex: &mut Execution, inst: &ObjectInstance, path: &str) -> CallResult<bool> {
    let app = &inst.app;
    let def = app.model().obj(inst.obj);
    let unknown = || {
        HostError::not_implemented(
            &inst.type_name(),
            &format!("ПроверитьЗаполнение: реквизит {path}"),
        )
    };
    let message = |ex: &mut Execution, text: String| {
        ex.push_message(UserMessage { text, status: None });
    };
    let Some((section, column)) = path.split_once('.') else {
        let i = crate::read::field_index(def, path).ok_or_else(unknown)?;
        let field = def.fields.get(i).ok_or_else(unknown)?;
        let value = inst.attr(ex, i)?;
        if filled(ex, &value) {
            return Ok(true);
        }
        let name = shown(app, &field.synonym, &field.name);
        let text = errors::render(keys::VALUE_NOT_FILLED, &[name.as_str()], ex.lang());
        message(ex, text);
        return Ok(false);
    };
    let t = crate::read::tabular_index(def, section).ok_or_else(unknown)?;
    let tab = def.tabular.get(t).ok_or_else(unknown)?;
    let c = tab
        .fields
        .iter()
        .position(|f| fold_name(&f.name) == fold_name(column))
        .ok_or_else(unknown)?;
    let column = tab.fields.get(c).ok_or_else(unknown)?;
    let lines = inst
        .state
        .borrow()
        .tabular
        .get(t)
        .map(|tab| tab.lines())
        .unwrap_or_default();
    let (column_name, list_name) = (
        shown(app, &column.synonym, &column.name),
        shown(app, &tab.synonym, &tab.name),
    );
    let mut ok = true;
    for (n, line) in lines.iter().enumerate() {
        ex.charge(1)?;
        if line.get(c).is_some_and(|v| filled(ex, v)) {
            continue;
        }
        ok = false;
        let row = (n + 1).to_string();
        let text = errors::render(
            keys::TABLE_VALUE_NOT_FILLED,
            &[column_name.as_str(), row.as_str(), list_name.as_str()],
            ex.lang(),
        );
        message(ex, text);
    }
    Ok(ok)
}
