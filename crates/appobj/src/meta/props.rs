use super::coll::{self, CollTy};
use super::object::{info, root_text, synonym_text};
use super::{MdCtx, MdKey, MdPath, fold, no_field, not_implemented, root_kind, value_of};
use crate::model::{FieldDef, FieldRole, ObjId};
use bsl_prim::{BslDate, Number, fold_name};
use bsl_vm::{CallResult, Execution, HostError, TypeId, Value};
use md_model::{Codec, MdKind, MdRef, Prop, TypedValue, Value as Mv};
use platform_catalog::{TypeIdx, TypeKind, messages::Lang};
use std::sync::Arc;

pub(crate) fn type_mismatch(n: usize) -> HostError {
    let mut e = HostError::key(bsl_stdlib::bind::msg::TYPE_MISMATCH);
    e.msg = bsl_vm::error::MsgRef::Key(bsl_vm::error::MsgKey {
        key: bsl_stdlib::bind::msg::TYPE_MISMATCH,
        args: vec![n.to_string()],
        suffix: String::new(),
    });
    e
}

fn prop_of<'a>(props: &'a [Prop], name: &str) -> Option<&'a Prop> {
    let xml = XML_NAMES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(name))
        .map_or(name, |(_, x)| *x);
    props.iter().find(|p| p.name.eq_ignore_ascii_case(xml))
}

const XML_NAMES: [(&str, &str); 4] = [
    ("FillingValue", "FillValue"),
    ("ActionsWritingOnPost", "RegisterRecordsWritingOnPost"),
    ("PrivilegedPostingMode", "PostInPrivilegedMode"),
    ("PrivilegedUnpostingMode", "UnpostInPrivilegedMode"),
];

fn is_undefined_prop(f: &str) -> bool {
    f.starts_with("модуль") || matches!(f, "справка" | "макет" | "предопределенные")
}

fn is_unmodeled_collection(f: &str) -> bool {
    matches!(
        f,
        "формы"
            | "команды"
            | "макеты"
            | "характеристики"
            | "агрегаты"
            | "графы"
            | "признакиучета"
            | "признакиучетасубконто"
            | "колонки"
            | "шаблоныurl"
            | "методы"
            | "операции"
            | "параметры"
            | "каналы"
            | "стандартныетабличныечасти"
            | "дополнительныеиндексы"
    )
}

pub(crate) fn obj_get(
    cx: &Arc<MdCtx>,
    ex: &mut Execution,
    path: MdPath,
    ru: &str,
    en: &str,
) -> CallResult<Value> {
    let f = fold(ru);
    let inf = info(cx, path).ok_or_else(|| no_field(ru))?;
    let (obj, tab) = match path {
        MdPath::Object(o) => (Some(o), None),
        MdPath::Tabular(o, t) => (Some(o), Some(t)),
        _ => (None, None),
    };
    let model = cx.model();
    match f.as_str() {
        "имя" => return Ok(Value::from(inf.name)),
        "синоним" => {
            let s = inf
                .synonym
                .map(|s| synonym_text(s, &cx.model().lang_code))
                .unwrap_or_default();
            return Ok(Value::from(s.as_str()));
        }
        "комментарий" => return Ok(Value::from(inf.comment)),
        "стандартныереквизиты" => {
            return Ok(match obj {
                Some(o) => coll::std_coll(cx, o, tab),
                None => coll::new_coll(cx, CollTy::Std, Vec::new()),
            });
        }
        "реквизиты" | "измерения" | "ресурсы" | "реквизитыадресации" =>
        {
            let role = match f.as_str() {
                "измерения" => FieldRole::Dimension,
                "ресурсы" => FieldRole::Resource,
                "реквизитыадресации" => FieldRole::AddressingAttribute,
                _ => FieldRole::Attribute,
            };
            return Ok(match obj {
                Some(o) => coll::fields_coll(cx, o, tab, role),
                None => coll::new_coll(cx, CollTy::Objects, Vec::new()),
            });
        }
        "табличныечасти" => {
            return Ok(match (obj, tab) {
                (Some(o), None) => coll::tabular_coll(cx, o),
                _ => coll::new_coll(cx, CollTy::Objects, Vec::new()),
            });
        }
        "значенияперечисления" => {
            return Ok(match (obj, tab) {
                (Some(o), None) => coll::enum_values_coll(cx, o),
                _ => coll::new_coll(cx, CollTy::Objects, Vec::new()),
            });
        }
        "стандартныетабличныечасти"
            if obj.is_some_and(|o| {
                tab.is_none() && model.obj(o).kind == crate::model::ObjKind::ChartOfCalculationTypes
            }) =>
        {
            return Ok(super::stdtab::tabs(cx));
        }
        "формы" => {
            return Ok(match (obj, tab) {
                (Some(o), None) => coll::forms_coll(cx, o),
                _ => coll::new_coll(cx, CollTy::Objects, Vec::new()),
            });
        }
        s if is_undefined_prop(s) => return Ok(Value::Undefined),
        s if is_unmodeled_collection(s) => return Err(not_implemented(ru).into()),
        _ => {}
    }
    if let MdPath::Subscription(i) = path
        && let Some(def) = model.subscriptions.get(i)
    {
        match f.as_str() {
            "событие" => return Ok(Value::from(event_ru(&def.event).as_str())),
            "обработчик" => {
                let h = format!("{}.{}", def.handler.0, def.handler.1);
                return Ok(Value::from(h.as_str()));
            }
            _ => {}
        }
    }
    let kind = match path {
        MdPath::Object(o) => Some(model.obj(o).kind),
        _ => None,
    };
    let Some(p) = prop_of(inf.props, en) else {
        if f == "принадлежностьобъекта" {
            return enum_by_name(cx, "ПринадлежностьОбъекта", "Native")
                .ok_or_else(|| not_implemented(ru).into());
        }
        return Err(not_implemented(ru).into());
    };
    convert(cx, ex, kind.map(|k| k.name(Lang::Ru)), ru, p)
}

pub(crate) fn predefined_names(
    cx: &Arc<MdCtx>,
    ex: &mut Execution,
    path: MdPath,
) -> CallResult<Value> {
    let names: Vec<Value> = match path {
        MdPath::Object(o) => cx
            .cfg_index
            .get(&cx.model().obj(o).md)
            .and_then(|i| cx.model().config.objects.get(*i))
            .and_then(|m| m.predefined())
            .map(|p| p.iter().map(|i| Value::from(i.name.as_str())).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let array = bsl_stdlib::new_array(names);
    let mut args = [bsl_vm::Arg::Val(array)];
    Ok(ex.construct_by_name("ФиксированныйМассив", &mut args)?)
}

fn event_ru(xml: &str) -> String {
    let ru = match xml {
        "BeforeWrite" => "ПередЗаписью",
        "OnWrite" => "ПриЗаписи",
        "BeforeDelete" => "ПередУдалением",
        "Posting" => "ОбработкаПроведения",
        "UndoPosting" => "ОбработкаУдаленияПроведения",
        "Filling" => "ОбработкаЗаполнения",
        "FillCheckProcessing" => "ОбработкаПроверкиЗаполнения",
        "OnCopy" => "ПриКопировании",
        "OnSetNewNumber" => "ПриУстановкеНовогоНомера",
        "OnSetNewCode" => "ПриУстановкеНовогоКода",
        other => other,
    };
    ru.to_string()
}

pub(crate) fn root_get(
    cx: &Arc<MdCtx>,
    ex: &mut Execution,
    ru: &str,
    en: &str,
) -> CallResult<Value> {
    let f = fold(ru);
    if let Some(kind) = root_kind(en) {
        return Ok(coll::root_coll(cx, kind));
    }
    if is_undefined_prop(&f) {
        return Ok(Value::Undefined);
    }
    let props = &cx.model().config.props;
    match f.as_str() {
        "имя" => return Ok(Value::from(root_text(cx, "Name").as_str())),
        "синоним" => {
            return Ok(match prop_of(props, "Synonym").map(|p| &p.value) {
                Some(Mv::LStr(s)) => Value::from(synonym_text(s, &cx.model().lang_code).as_str()),
                _ => Value::from(""),
            });
        }
        _ => {}
    }
    let p = prop_of(props, en).ok_or_else(|| no_field(ru))?;
    convert(cx, ex, None, ru, p)
}

pub(crate) fn std_get(
    cx: &Arc<MdCtx>,
    ex: &mut Execution,
    obj: ObjId,
    fd: &FieldDef,
    ru: &str,
    en: &str,
) -> CallResult<Value> {
    let f = fold(ru);
    let def = cx.model().obj(obj);
    let sa = cx
        .cfg_index
        .get(&def.md)
        .and_then(|i| cx.model().config.objects.get(*i))
        .and_then(|m| {
            let want = fd.std.map(|s| fold_name(s.names().1))?;
            m.standard_attributes()
                .iter()
                .find(|a| fold_name(&a.name) == want)
        });
    let xml = sa.and_then(|a| prop_of(&a.props, en));
    match f.as_str() {
        "имя" => Ok(Value::from(fd.name.as_str())),
        "синоним" => {
            let s = match xml.map(|p| &p.value) {
                Some(Mv::LStr(s)) => synonym_text(s, &cx.model().lang_code),
                _ => synonym_text(&fd.synonym, &cx.model().lang_code),
            };
            Ok(Value::from(s.as_str()))
        }
        "комментарий" => Ok(Value::from(match xml.map(|p| &p.value) {
            Some(Mv::Text(t)) => t.as_str(),
            _ => "",
        })),
        "тип" => Err(not_implemented("Тип").into()),
        "проверказаполнения" => {
            let lit = match xml.map(|p| &p.value) {
                Some(Mv::Text(t)) => t.as_str(),
                _ if fd.fill_check => "ShowError",
                _ => "DontCheck",
            };
            enum_by_name(cx, "ПроверкаЗаполнения", lit).ok_or_else(|| not_implemented(ru).into())
        }
        _ => match xml {
            Some(p) => convert(cx, ex, None, ru, p),
            None => Err(not_implemented(ru).into()),
        },
    }
}

fn enum_by_name(cx: &MdCtx, type_ru: &str, literal: &str) -> Option<Value> {
    let t = cx.catalog.find_type(type_ru)?;
    bsl_stdlib::enums::enum_value(t, literal)
}

const ENUM_TYPES: [(&str, &str, &str); 38] = [
    ("ТипКода", "Справочник", "ТипКодаСправочника"),
    ("ТипКода", "ПланВидовРасчета", "ТипКодаПланаВидовРасчета"),
    (
        "ТипКода",
        "ПланВидовХарактеристик",
        "ТипКодаПланаВидовХарактеристик",
    ),
    ("ТипКода", "ПланСчетов", "ТипКодаПланаСчетов"),
    ("ТипНомера", "Документ", "ТипНомераДокумента"),
    ("ТипНомера", "БизнесПроцесс", "ТипНомераБизнесПроцесса"),
    ("ТипНомера", "Задача", "ТипНомераЗадачи"),
    (
        "ПериодичностьНомера",
        "Документ",
        "ПериодичностьНомераДокумента",
    ),
    (
        "ПериодичностьНомера",
        "БизнесПроцесс",
        "ПериодичностьНомераБизнесПроцесса",
    ),
    ("СерииКодов", "Справочник", "СерииКодовСправочника"),
    ("СерииКодов", "ПланСчетов", "СерииКодовПланаСчетов"),
    (
        "СерииКодов",
        "ПланВидовХарактеристик",
        "СерииКодовПланаВидовХарактеристик",
    ),
    ("ДопустимаяДлинаКода", "", "ДопустимаяДлина"),
    ("ДопустимаяДлинаНомера", "", "ДопустимаяДлина"),
    (
        "ОсновноеПредставление",
        "Справочник",
        "ОсновноеПредставлениеСправочника",
    ),
    (
        "ОсновноеПредставление",
        "ПланВидовХарактеристик",
        "ОсновноеПредставлениеВидаХарактеристики",
    ),
    (
        "ОсновноеПредставление",
        "ПланВидовРасчета",
        "ОсновноеПредставлениеВидаРасчета",
    ),
    (
        "ОсновноеПредставление",
        "ПланСчетов",
        "ОсновноеПредставлениеСчета",
    ),
    (
        "ОсновноеПредставление",
        "Задача",
        "ОсновноеПредставлениеЗадачи",
    ),
    (
        "ОсновноеПредставление",
        "ПланОбмена",
        "ОсновноеПредставлениеПланаОбмена",
    ),
    ("Использование", "", "ИспользованиеРеквизита"),
    ("ВидРегистра", "", "ВидРегистраНакопления"),
    ("ОперативноеПроведение", "", "Проведение"),
    (
        "Периодичность",
        "РегистрСведений",
        "ПериодичностьРегистраСведений",
    ),
    (
        "Периодичность",
        "РегистрРасчета",
        "ПериодичностьРегистраРасчета",
    ),
    ("РежимЗаписи", "", "РежимЗаписиРегистра"),
    (
        "ЗаписьДвиженийПриПроведении",
        "",
        "ЗаписьДвиженийПриПроведении",
    ),
    (
        "УдалениеДвижений",
        "",
        "УдалениеДвиженийПриОтменеПроведения",
    ),
    (
        "РежимУправленияБлокировкойДанных",
        "",
        "РежимУправленияБлокировкойДанных",
    ),
    (
        "РежимУправленияБлокировкойДанных",
        "Корень",
        "РежимУправленияБлокировкойДанныхПоУмолчанию",
    ),
    ("ИспользованиеПодчинения", "", "ИспользованиеПодчинения"),
    ("ВидИерархии", "", "ВидИерархии"),
    ("ИсторияДанных", "", "ИспользованиеИсторииДанных"),
    (
        "ПолнотекстовыйПоиск",
        "",
        "ИспользованиеПолнотекстовогоПоиска",
    ),
    ("БыстрыйВыбор", "", "ИспользованиеБыстрогоВыбора"),
    ("ВыборГруппИЭлементов", "", "ГруппыИЭлементы"),
    (
        "ПовторноеИспользованиеСеансов",
        "",
        "РежимПовторногоИспользованияСеансов",
    ),
    (
        "ОсновнойРежимЗапуска",
        "",
        "РежимЗапускаКлиентскогоПриложения",
    ),
];

fn enum_type(cx: &MdCtx, kind: Option<&str>, ru: &str, literal: &str) -> Option<TypeIdx> {
    let ok = |t: TypeIdx| {
        cx.catalog.type_info(t).kind == TypeKind::Enum
            && cx.catalog.find_enum_value(t, literal).is_some()
    };
    for (prop, k, en) in ENUM_TYPES {
        if prop == ru
            && (k.is_empty() || Some(k) == kind)
            && let Some(t) = cx.catalog.find_type(en).filter(|t| ok(*t))
        {
            return Some(t);
        }
    }
    cx.catalog.find_type(ru).filter(|t| ok(*t))
}

const CURRENT_VERSION: &str = "Version8_5_1";

fn number_of(text: &str) -> Value {
    match Number::parse(text) {
        Some(n) => Value::Number(n),
        None => Value::from(text),
    }
}

pub(crate) fn convert(
    cx: &Arc<MdCtx>,
    ex: &mut Execution,
    kind: Option<&str>,
    ru: &str,
    p: &Prop,
) -> CallResult<Value> {
    match &p.value {
        Mv::Bool(b) => Ok(Value::Bool(*b)),
        Mv::LStr(s) => Ok(Value::from(synonym_text(s, &cx.model().lang_code).as_str())),
        Mv::Text(t) => match p.codec {
            Codec::Enum => {
                let lit = if t == CURRENT_VERSION {
                    "DontUse"
                } else {
                    t.as_str()
                };
                enum_type(cx, kind, ru, lit)
                    .and_then(|ty| bsl_stdlib::enums::enum_value(ty, lit))
                    .ok_or_else(|| not_implemented(ru).into())
            }
            Codec::Int => Number::parse(t)
                .map(Value::Number)
                .ok_or_else(|| not_implemented(ru).into()),
            Codec::Event => Ok(Value::from(event_ru(t).as_str())),
            Codec::Str | Codec::Method | Codec::Color | Codec::Shortcut | Codec::CmdGroup => {
                Ok(Value::from(t.as_str()))
            }
            _ => Err(not_implemented(ru).into()),
        },
        Mv::Ref(r) => Ok(match resolve_ref(cx, r) {
            Some(path) => value_of(cx, ex, MdKey::Path(path)),
            None => Value::Undefined,
        }),
        Mv::RefList(list) => {
            let items = list
                .iter()
                .filter_map(|r| resolve_ref(cx, r))
                .map(MdKey::Path)
                .collect();
            Ok(coll::new_coll(cx, CollTy::Values, items))
        }
        Mv::Typed(t) => typed(cx, t, ru),
        Mv::Type(_) | Mv::FieldList(_) | Mv::StandardAttributes(_) | Mv::Xml(_) => {
            Err(not_implemented(ru).into())
        }
    }
}

fn typed(cx: &MdCtx, t: &TypedValue, ru: &str) -> CallResult<Value> {
    Ok(match t {
        TypedValue::Nil => Value::Undefined,
        TypedValue::Str(s) => Value::from(s.as_str()),
        TypedValue::Number(s) => number_of(s),
        TypedValue::Bool(b) => Value::Bool(*b),
        TypedValue::Date(s) => parse_date(s).ok_or_else(|| not_implemented(ru))?,
        TypedValue::DesignTimeRef(s) => {
            design_time_ref(cx, s)?.ok_or_else(|| not_implemented(ru))?
        }
        TypedValue::EmptyTypeDescription | TypedValue::FixedArray(_) => {
            return Err(not_implemented(ru).into());
        }
    })
}

fn design_time_ref(cx: &MdCtx, text: &str) -> CallResult<Option<Value>> {
    let seg: Vec<&str> = text.split('.').collect();
    let (Some(head), Some(rest)) = (seg.get(..2), seg.get(2..)) else {
        return Ok(None);
    };
    let Some(MdPath::Object(obj)) = cx.model().find_full_name(&head.join(".")) else {
        return Ok(None);
    };
    let app = &cx.app;
    Ok(match rest {
        [e] if e.eq_ignore_ascii_case("EmptyRef") => {
            Some(crate::values::refs::empty_ref(app, obj)?)
        }
        [v, item] if v.eq_ignore_ascii_case("EnumValue") => {
            crate::managers::enum_value(app, obj, item)?
        }
        [item] => {
            let want = fold_name(item);
            if !app
                .predefined_item_names(obj)
                .iter()
                .any(|(_, n)| fold_name(n) == want)
            {
                return Ok(None);
            }
            let r =
                app.predefined()
                    .by_name(app, obj, &crate::predefined::AreaKey::default(), item)?;
            Some(crate::values::refs::ref_value(app, r)?)
        }
        _ => None,
    })
}

fn parse_date(s: &str) -> Option<Value> {
    let (d, t) = s.split_once('T')?;
    let mut dp = d.split('-').map(|x| x.parse::<u32>().ok());
    let mut tp = t.split(':').map(|x| x.parse::<u32>().ok());
    let (y, mo, da) = (dp.next()??, dp.next()??, dp.next()??);
    let (h, mi, se) = (tp.next()??, tp.next()??, tp.next()??);
    BslDate::from_parts(y, mo, da, h, mi, se).map(Value::Date)
}

fn resolve_ref(cx: &MdCtx, r: &MdRef) -> Option<MdPath> {
    let model = cx.model();
    if let Some(id) = r.id {
        if let Some(o) = model.by_uuid(id) {
            return Some(MdPath::Object(o));
        }
        if let Some(i) = model.common_modules.iter().position(|m| m.md == id) {
            return Some(MdPath::CommonModule(i));
        }
        if let Some(i) = model.session_params.iter().position(|m| m.md == id) {
            return Some(MdPath::SessionParam(i));
        }
        if let Some(i) = model.subscriptions.iter().position(|m| m.md == id) {
            return Some(MdPath::Subscription(i));
        }
        if let Some(i) = cx.cfg_index.get(&id) {
            return Some(MdPath::Other(*i));
        }
    }
    if r.name.is_empty() {
        return None;
    }
    resolve_full(cx, &r.name)
}

pub(crate) fn resolve_full(cx: &MdCtx, full: &str) -> Option<MdPath> {
    if let Some(p) = cx.model().find_full_name(full) {
        return Some(p);
    }
    let seg: Vec<&str> = full.split('.').collect();
    let [word, name] = seg.as_slice() else {
        return None;
    };
    let w = fold_name(word);
    let kind = MdKind::all().iter().copied().find(|k| {
        fold_name(k.as_str()) == w
            || cx
                .catalog
                .find_type(&format!("MetadataObject: {}", k.as_str()))
                .and_then(|t| {
                    cx.catalog
                        .type_info(t)
                        .name_ru
                        .strip_prefix("ОбъектМетаданных: ")
                        .map(fold_name)
                })
                .is_some_and(|ru| ru == w)
    })?;
    let n = fold_name(name);
    cx.model()
        .config
        .objects
        .iter()
        .position(|o| o.kind == kind && fold_name(&o.name) == n)
        .map(MdPath::Other)
}

pub(crate) fn find_by_type(cx: &MdCtx, t: TypeId) -> Option<MdPath> {
    let types = cx.app.types()?;
    if let Some((o, tab, _)) = types.describe_tabular(t) {
        return Some(MdPath::Tabular(o, tab));
    }
    types.describe(t).map(|(o, _)| MdPath::Object(o))
}
