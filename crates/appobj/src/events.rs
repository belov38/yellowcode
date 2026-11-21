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
    NewNumber,
    BeforeDelete,
    OnCopy,
    Filling,
    FillCheck,
    Posting,
    UndoPosting,
}

impl Event {
    pub(crate) const fn is_write(self) -> bool {
        !matches!(self, Event::OnCopy | Event::Filling)
    }

    const ALL: [Event; 10] = [
        Event::BeforeWrite,
        Event::OnWrite,
        Event::NewCode,
        Event::NewNumber,
        Event::BeforeDelete,
        Event::OnCopy,
        Event::Filling,
        Event::FillCheck,
        Event::Posting,
        Event::UndoPosting,
    ];

    pub(crate) const fn names(self) -> (&'static str, &'static str) {
        match self {
            Event::BeforeWrite => ("ПередЗаписью", "BeforeWrite"),
            Event::OnWrite => ("ПриЗаписи", "OnWrite"),
            Event::NewCode => ("ПриУстановкеНовогоКода", "OnSetNewCode"),
            Event::NewNumber => ("ПриУстановкеНовогоНомера", "OnSetNewNumber"),
            Event::BeforeDelete => ("ПередУдалением", "BeforeDelete"),
            Event::OnCopy => ("ПриКопировании", "OnCopy"),
            Event::Filling => ("ОбработкаЗаполнения", "Filling"),
            Event::FillCheck => ("ОбработкаПроверкиЗаполнения", "FillCheckProcessing"),
            Event::Posting => ("ОбработкаПроведения", "Posting"),
            Event::UndoPosting => ("ОбработкаУдаленияПроведения", "UndoPosting"),
        }
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

fn handler_in(module: &ModuleInstanceRef, event: Event) -> Option<&'static str> {
    let (ru, en) = event.names();
    [ru, en]
        .into_iter()
        .find(|n| module.module().method(n).is_some())
}

pub(crate) struct Owner<'a> {
    pub(crate) app: &'a Application,
    pub(crate) def: &'a ObjDef,
    pub(crate) module: Option<ModuleInstanceRef>,
    pub(crate) source: Value,
    pub(crate) category: &'static str,
}

pub(crate) fn handled(inst: &ObjectInstance, event: Event) -> bool {
    let app = &inst.app;
    inst.module()
        .is_some_and(|m| handler_in(&m, event).is_some())
        || !subscribed(app, app.model().obj(inst.obj), "Object", event).is_empty()
}

pub(crate) fn modes_seen(inst: &ObjectInstance) -> bool {
    let app = &inst.app;
    if let Some(module) = inst.module()
        && let Some(name) = handler_in(&module, Event::BeforeWrite)
        && module
            .module()
            .method(name)
            .is_none_or(|(_, code)| sees_modes(code, 1))
    {
        return true;
    }
    subscribed(app, app.model().obj(inst.obj), "Object", Event::BeforeWrite)
        .iter()
        .any(|(module_name, method)| {
            let Some(m) = app
                .modules()
                .module_ref(&format!("ОбщийМодуль.{module_name}.Модуль"))
            else {
                return true;
            };
            let Ok(compiled) = app.modules().compiled(m) else {
                return true;
            };
            compiled
                .method(method)
                .is_none_or(|(_, code)| sees_modes(code, 2))
        })
}

fn sees_modes(code: &bsl_compiler::bytecode::MethodCode, first: u32) -> bool {
    code.callable_only
        || code
            .code
            .iter()
            .any(|op| names_slots(*op, &[first, first + 1]))
}

fn names_slots(op: bsl_compiler::bytecode::Op, slots: &[u32]) -> bool {
    use bsl_compiler::bytecode::Op;
    match op {
        Op::LoadLocal(i)
        | Op::StoreLocal(i)
        | Op::RefLocal(i)
        | Op::IterStart(i)
        | Op::IterNext { slot: i, .. } => slots.contains(&i),
        Op::Execute | Op::Evaluate | Op::LoadOuter(_) | Op::StoreOuter(_) | Op::RefOuter(_) => true,
        Op::Const(_)
        | Op::Undef
        | Op::Null
        | Op::True
        | Op::False
        | Op::Pop
        | Op::Dup
        | Op::LoadModVar(_)
        | Op::StoreModVar(_)
        | Op::LoadGlobal(_)
        | Op::StoreGlobal(_)
        | Op::LoadContext(_)
        | Op::StoreContext(_)
        | Op::GetMember(_)
        | Op::SetMember(_)
        | Op::GetIndex
        | Op::SetIndex
        | Op::RefModVar(_)
        | Op::RefMember(_)
        | Op::RefIndex
        | Op::Omitted
        | Op::CallLocal { .. }
        | Op::CallApp { .. }
        | Op::CallPlatform { .. }
        | Op::CallContext { .. }
        | Op::CallMember { .. }
        | Op::New { .. }
        | Op::NewNoArgs { .. }
        | Op::NewDynamic { .. }
        | Op::Return
        | Op::ReturnValue
        | Op::Neg
        | Op::Pos
        | Op::Not
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Mod
        | Op::Eq
        | Op::Ne
        | Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge
        | Op::ToBool
        | Op::Jump(_)
        | Op::JumpIfFalse(_)
        | Op::JumpIfTrue(_)
        | Op::Raise
        | Op::RaiseExt(_)
        | Op::Reraise
        | Op::EnterHandler
        | Op::LeaveHandler(_)
        | Op::Await
        | Op::AddHandler
        | Op::RemoveHandler => false,
    }
}

pub(crate) fn has_write_subscriptions(app: &Application, def: &ObjDef, category: &str) -> bool {
    [Event::BeforeWrite, Event::OnWrite]
        .into_iter()
        .any(|e| !subscribed(app, def, category, e).is_empty())
}

pub(crate) fn subscription_handlers(
    app: &Application,
    def: &ObjDef,
    category: &str,
) -> Vec<(String, String)> {
    Event::ALL
        .into_iter()
        .flat_map(|e| subscribed(app, def, category, e))
        .collect()
}

fn subscribed(
    app: &Application,
    def: &ObjDef,
    category: &str,
    event: Event,
) -> Vec<(String, String)> {
    let kind_word = format!("cfg:{}{category}", def.kind.name(Lang::En));
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

fn fewer_fill_parameters(
    owner: &Owner<'_>,
    module: &ModuleInstanceRef,
    name: &str,
    event: Event,
    args: &mut Vec<Arg>,
    subscription: bool,
) -> CallResult<()> {
    if event != Event::Filling {
        return Ok(());
    }
    let Some((_, method)) = module.module().method(name) else {
        return Ok(());
    };
    let declared = method.params.len();
    if declared >= args.len() {
        return Ok(());
    }
    if subscription || declared != 2 {
        let whose = if subscription {
            "подписки"
        } else {
            "модуля объекта"
        };
        return Err(HostError::not_implemented(
            &format!("{}Объект", owner.def.kind.name(Lang::Ru)),
            &format!("ОбработкаЗаполнения {whose} с {declared} параметрами"),
        )
        .into());
    }
    args.truncate(declared);
    Ok(())
}

#[derive(Clone, Copy)]
struct Level {
    weight: u32,
    privileged: bool,
}

fn call(
    ex: &mut Execution,
    app: &Application,
    event: Event,
    module: &ModuleInstanceRef,
    name: &str,
    mut args: Vec<Arg>,
    level: Level,
) -> CallResult<()> {
    pad_missing(module, name, &mut args);
    let frame = HandlerFrame::enter(ex);
    let outcome = if level.privileged {
        ex.call_privileged_weighted(module, name, &mut args, level.weight)
    } else {
        ex.call_weighted(module, name, &mut args, level.weight)
    };
    frame.leave(ex);
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
    let owner = Owner {
        app: &app,
        def: app.model().obj(inst.obj),
        module: inst.module(),
        source: Value::Object(inst.clone()),
        category: "Object",
    };
    run_for(ex, &owner, event, params, cancel)
}

pub(crate) fn run_for(
    ex: &mut Execution,
    owner: &Owner<'_>,
    event: Event,
    params: &[Param],
    cancel: Option<&Place>,
) -> CallResult<bool> {
    run_with(ex, owner, event, params, cancel, false)
}

fn run_with(
    ex: &mut Execution,
    owner: &Owner<'_>,
    event: Event,
    params: &[Param],
    cancel: Option<&Place>,
    privileged: bool,
) -> CallResult<bool> {
    let app = owner.app;
    if event.is_write()
        && let Some(why) = crate::extensions::write_refusal(app, owner.def, owner.category)
    {
        let name = format!("{}.{}", owner.def.kind.name(Lang::Ru), owner.def.name);
        return Err(
            HostError::not_implemented(&name, &format!("{}: {why}", event.names().0)).into(),
        );
    }
    let stopped = || {
        cancel
            .and_then(Place::temp_value)
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    };
    if let Some(module) = &owner.module
        && let Some(name) = handler_in(module, event)
    {
        let mut args = args_of(None, params);
        fewer_fill_parameters(owner, module, name, event, &mut args, false)?;
        call(
            ex,
            app,
            event,
            module,
            name,
            args,
            Level {
                weight: MODULE_WEIGHT,
                privileged,
            },
        )?;
        if stopped() {
            return Ok(true);
        }
    }
    for (module_name, method) in subscribed(app, owner.def, owner.category, event) {
        let module = app
            .modules()
            .module_ref(&format!("ОбщийМодуль.{module_name}.Модуль"))
            .ok_or_else(|| internal(&format!("no common module {module_name}")))?;
        let instance = app.instance_in_call(ex, module).map_err(CallError::Exc)?;
        let mut args = args_of(Some(&owner.source), params);
        fewer_fill_parameters(owner, &instance, &method, event, &mut args, true)?;
        call(
            ex,
            app,
            event,
            &instance,
            &method,
            args,
            Level {
                weight: SUBSCRIPTION_WEIGHT,
                privileged,
            },
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

pub(crate) fn writing_document(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    event: Event,
    mode: &Value,
    posting: &Value,
) -> CallResult<()> {
    let cancel = Place::temp(Value::Bool(false));
    let mut params = vec![Param::Ref(cancel.clone())];
    let modes = [Place::temp(mode.clone()), Place::temp(posting.clone())];
    if event == Event::BeforeWrite {
        params.extend(modes.iter().cloned().map(Param::Ref));
    }
    let cancelled = run(ex, inst, event, &params, Some(&cancel))?;
    if event == Event::BeforeWrite {
        for (place, was, name) in [
            (&modes[0], mode, "РежимЗаписи"),
            (&modes[1], posting, "РежимПроведения"),
        ] {
            if !same_enum_value(place.temp_value().as_ref(), was) {
                return Err(HostError::not_implemented(
                    &inst.type_name(),
                    &format!("Записать: {name} изменён в ПередЗаписью"),
                )
                .into());
            }
        }
    }
    if cancelled {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            &format!("Записать: Отказ в {}", event.names().0),
        )
        .into());
    }
    Ok(())
}

pub(crate) fn posting(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    mode: &Value,
    privileged: bool,
) -> CallResult<()> {
    let app = inst.app.clone();
    let owner = Owner {
        app: &app,
        def: app.model().obj(inst.obj),
        module: inst.module(),
        source: Value::Object(inst.clone()),
        category: "Object",
    };
    let cancel = Place::temp(Value::Bool(false));
    let params = [Param::Ref(cancel.clone()), Param::Val(mode.clone())];
    if run_with(
        ex,
        &owner,
        Event::Posting,
        &params,
        Some(&cancel),
        privileged,
    )? {
        let real_time =
            bsl_stdlib::enums::as_enum_value(mode).is_some_and(|e| e.name_ru() == "Оперативный");
        if real_time {
            return Err(cant_post(inst)?.into());
        }
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "Записать: Отказ в ОбработкаПроведения",
        )
        .into());
    }
    Ok(())
}

fn cant_post(inst: &ObjectInstance) -> CallResult<HostError> {
    let app = &inst.app;
    let def = app.model().obj(inst.obj);
    let extended = match app
        .model()
        .config
        .object(def.md)
        .and_then(|o| o.prop("ExtendedObjectPresentation"))
    {
        Some(md_model::Value::LStr(l)) => crate::types::synonym(l, "", &app.model().lang_code),
        _ => String::new(),
    };
    if extended.is_empty() {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "Записать: Отказ в ОбработкаПроведения без расширенного представления объекта",
        )
        .into());
    }
    let text = |lang| {
        format!(
            "{}: \"{extended}\"!",
            errors::render(keys::REFERENCE_CANT_POST, &[], lang)
        )
    };
    let mut e = HostError::text(&text(Lang::Ru), &text(Lang::En));
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    Ok(e)
}

fn same_enum_value(now: Option<&Value>, was: &Value) -> bool {
    let (Some(a), Some(b)) = (
        now.and_then(bsl_stdlib::enums::as_enum_value),
        bsl_stdlib::enums::as_enum_value(was),
    ) else {
        return false;
    };
    a.type_idx() == b.type_idx() && a.index() == b.index()
}

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
    let guard = FillCheck::enter();
    let ok = check_filling_inner(ex, inst)?;
    Ok(ok && !guard.failed())
}

thread_local! {
    static FILL_CHECKS: std::cell::RefCell<Vec<bool>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct FillCheck;

impl FillCheck {
    fn enter() -> FillCheck {
        FILL_CHECKS.with(|s| s.borrow_mut().push(false));
        FillCheck
    }

    fn failed(&self) -> bool {
        FILL_CHECKS.with(|s| s.borrow().last().copied().unwrap_or(false))
    }
}

impl Drop for FillCheck {
    fn drop(&mut self) {
        FILL_CHECKS.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

pub(crate) fn in_fill_check() -> bool {
    FILL_CHECKS.with(|s| !s.borrow().is_empty())
}

pub(crate) fn fail_fill_check() {
    FILL_CHECKS.with(|s| {
        if let Some(last) = s.borrow_mut().last_mut() {
            *last = true;
        }
    });
}

fn check_filling_inner(ex: &mut Execution, inst: &Rc<ObjectInstance>) -> CallResult<bool> {
    let app = inst.app.clone();
    let def = app.model().obj(inst.obj);
    let mut paths: Vec<String> = def
        .fields
        .iter()
        .enumerate()
        .filter(|(i, f)| f.fill_check && !inst.hidden(*i))
        .map(|(_, f)| f.name.clone())
        .collect();
    let md = app.model().config.object(def.md);
    for t in &def.tabular {
        if section_checked(md, t.md) {
            paths.push(t.name.clone());
        }
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

fn section_checked(md: Option<&md_model::MdObject>, section: crate::model::Uuid16) -> bool {
    md.and_then(|o| o.children.iter().find(|c| c.id == section))
        .and_then(|c| c.props.iter().find(|p| p.name == "FillChecking"))
        .is_some_and(|p| matches!(&p.value, md_model::Value::Text(t) if t == "ShowError"))
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
        if crate::read::field_index(def, path).is_none()
            && let Some(t) = crate::read::tabular_index(def, path)
        {
            let empty = inst
                .state
                .borrow()
                .tabular
                .get(t)
                .is_none_or(|tab| tab.is_empty());
            if !empty {
                return Ok(true);
            }
            return Err(HostError::not_implemented(
                &inst.type_name(),
                &format!("ПроверитьЗаполнение: пустая табличная часть {path}"),
            )
            .into());
        }
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
