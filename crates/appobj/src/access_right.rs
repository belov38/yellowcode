use crate::Application;
use crate::meta::{MdKey, MdPath, arg_value, full_name, key_with_app, type_idx};
use crate::model::AppModel;
use crate::session::{call_ctx, internal};
use crate::session_info::outside_call;
use bsl_prim::fold_name;
use bsl_vm::{Arg, CallResult, Execution, HostError, Registry, Value};
use md_model::MdKind;
use platform_catalog::catalog;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const NAMES: [(&str, &str); 70] = [
    ("Чтение", "Read"),
    ("Добавление", "Insert"),
    ("Изменение", "Update"),
    ("Удаление", "Delete"),
    ("Проведение", "Posting"),
    ("ОтменаПроведения", "UndoPosting"),
    ("Просмотр", "View"),
    ("ИнтерактивноеДобавление", "InteractiveInsert"),
    ("Редактирование", "Edit"),
    ("ИнтерактивноеУдаление", "InteractiveDelete"),
    ("ИнтерактивнаяПометкаУдаления", "InteractiveSetDeletionMark"),
    (
        "ИнтерактивноеСнятиеПометкиУдаления",
        "InteractiveClearDeletionMark",
    ),
    ("ИнтерактивноеУдалениеПомеченных", "InteractiveDeleteMarked"),
    ("ИнтерактивноеПроведение", "InteractivePosting"),
    (
        "ИнтерактивноеПроведениеНеОперативное",
        "InteractivePostingRegular",
    ),
    ("ИнтерактивнаяОтменаПроведения", "InteractiveUndoPosting"),
    (
        "ИнтерактивноеИзменениеПроведенных",
        "InteractiveChangeOfPosted",
    ),
    ("ВводПоСтроке", "InputByString"),
    ("УправлениеИтогами", "TotalsControl"),
    ("Использование", "Use"),
    ("Администрирование", "Administration"),
    ("АдминистрированиеДанных", "DataAdministration"),
    ("МонопольныйРежим", "ExclusiveMode"),
    ("АктивныеПользователи", "ActiveUsers"),
    ("ЖурналРегистрации", "EventLog"),
    ("ВнешнееСоединение", "ExternalConnection"),
    ("Automation", "Automation"),
    (
        "ИнтерактивноеОткрытиеВнешнихОбработок",
        "InteractiveOpenExtDataProcessors",
    ),
    (
        "ИнтерактивноеОткрытиеВнешнихОтчетов",
        "InteractiveOpenExtReports",
    ),
    ("Получение", "Get"),
    ("Установка", "Set"),
    ("ИнтерактивнаяАктивация", "InteractiveActivate"),
    ("Старт", "Start"),
    ("ИнтерактивныйСтарт", "InteractiveStart"),
    ("Выполнение", "Execute"),
    ("ИнтерактивноеВыполнение", "InteractiveExecute"),
    ("Вывод", "Output"),
    (
        "ОбновлениеКонфигурацииБазыДанных",
        "UpdateDataBaseConfiguration",
    ),
    ("ТонкийКлиент", "ThinClient"),
    ("ВебКлиент", "WebClient"),
    ("ТолстыйКлиент", "ThickClient"),
    ("РежимВсеФункции", "AllFunctionsMode"),
    ("СохранениеДанныхПользователя", "SaveUserData"),
    (
        "ИзменениеСтандартнойАутентификации",
        "StandardAuthenticationChange",
    ),
    (
        "ИзменениеСтандартнойАутентификацииСеанса",
        "SessionStandardAuthenticationChange",
    ),
    (
        "ИзменениеАутентификацииОССеанса",
        "SessionOSAuthenticationChange",
    ),
    (
        "ИнтерактивноеУдалениеПредопределенныхДанных",
        "InteractiveDeletePredefinedData",
    ),
    (
        "ИнтерактивнаяПометкаУдаленияПредопределенныхДанных",
        "InteractiveSetDeletionMarkPredefinedData",
    ),
    (
        "ИнтерактивноеСнятиеПометкиУдаленияПредопределенныхДанных",
        "InteractiveClearDeletionMarkPredefinedData",
    ),
    (
        "ИнтерактивноеУдалениеПомеченныхПредопределенныхДанных",
        "InteractiveDeleteMarkedPredefinedData",
    ),
    (
        "АдминистрированиеРасширенийКонфигурации",
        "ConfigurationExtensionsAdministration",
    ),
    ("ЧтениеИсторииДанных", "ReadDataHistory"),
    ("ПросмотрИсторииДанных", "ViewDataHistory"),
    (
        "ЧтениеИсторииДанныхОтсутствующихДанных",
        "ReadDataHistoryOfMissingData",
    ),
    ("ИзменениеИсторииДанных", "UpdateDataHistory"),
    (
        "ИзменениеИсторииДанныхОтсутствующихДанных",
        "UpdateDataHistoryOfMissingData",
    ),
    (
        "ИзменениеНастроекИсторииДанных",
        "UpdateDataHistorySettings",
    ),
    (
        "ИзменениеКомментарияВерсииИсторииДанных",
        "UpdateDataHistoryVersionComment",
    ),
    (
        "РедактированиеКомментарияВерсииИсторииДанных",
        "EditDataHistoryVersionComment",
    ),
    ("ПереходНаВерсиюИсторииДанных", "SwitchToDataHistoryVersion"),
    (
        "РегистрацияИнформационнойБазыСистемыВзаимодействия",
        "CollaborationSystemInfoBaseRegistration",
    ),
    ("МобильныйКлиент", "MobileClient"),
    ("РежимОсновногоОкнаОбычный", "MainWindowModeNormal"),
    ("РежимОсновногоОкнаРабочееМесто", "MainWindowModeWorkplace"),
    (
        "РежимОсновногоОкнаВстроенноеРабочееМесто",
        "MainWindowModeEmbeddedWorkplace",
    ),
    (
        "РежимОсновногоОкнаПолноэкранноеРабочееМесто",
        "MainWindowModeFullscreenWorkplace",
    ),
    ("РежимОсновногоОкнаКиоск", "MainWindowModeKiosk"),
    ("РежимТехническогоСпециалиста", "TechnicalSpecialistMode"),
    (
        "ЗавершениеМонопольногоРежимаПриНачалеСеанса",
        "ExclusiveModeTerminationAtSessionStart",
    ),
    ("КлиентСистемыАналитики", "AnalyticsSystemClient"),
];

fn vocabulary_name(en: &'static str) -> &'static str {
    if en == "AllFunctionsMode" {
        "TechnicalSpecialistMode"
    } else {
        en
    }
}

fn known_right(name: &str) -> Option<&'static str> {
    static BY_NAME: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    BY_NAME
        .get_or_init(|| {
            NAMES
                .iter()
                .flat_map(|&(ru, en)| [(fold_name(ru), en), (fold_name(en), en)])
                .collect()
        })
        .get(&fold_name(name))
        .copied()
        .map(vocabulary_name)
}

fn applies(kind: MdKind, right: &str) -> Option<bool> {
    mdschema::static_rights_tables()
        .rights_by_kind()
        .find(|(name, _)| *name == kind.as_str())
        .map(|(_, rights)| rights.iter().any(|name| name == right))
}

fn vocabulary_kind(model: &AppModel, key: MdKey) -> Option<MdKind> {
    match key {
        MdKey::Path(MdPath::Configuration) => Some(MdKind::Configuration),
        MdKey::Path(MdPath::Object(id)) => Some(model.obj(id).kind.md_kind()),
        MdKey::Path(MdPath::SessionParam(_)) => Some(MdKind::SessionParameter),
        MdKey::Path(MdPath::Subsystem(_)) => Some(MdKind::Subsystem),
        MdKey::Path(MdPath::Other(i)) => model.config.objects.get(i).map(|o| o.kind),
        _ => None,
    }
}

fn refused(what: &str) -> HostError {
    HostError::not_implemented("Глобальный контекст", &format!("ПравоДоступа: {what}"))
}

fn given(args: &[Arg], i: usize) -> bool {
    matches!(args.get(i), Some(Arg::Val(_) | Arg::Place(_)))
}

fn kind_label(app: &Application, key: MdKey) -> String {
    let Some(cx) = app.md_ctx() else {
        return String::new();
    };
    let ty = match key {
        MdKey::Path(path) => type_idx(&cx, path),
        MdKey::Std(..) => Some(cx.std.idx),
    };
    match (ty, key) {
        (Some(t), _) => catalog().type_info(t).name_ru.clone(),
        (None, MdKey::Path(MdPath::Other(i))) => cx
            .model()
            .config
            .objects
            .get(i)
            .map_or_else(String::new, |o| o.kind.as_str().to_string()),
        (None, _) => String::new(),
    }
}

fn access_right(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if given(args, 2) {
        return Err(refused("пользователь или роль").into());
    }
    if given(args, 3) {
        return Err(refused("стандартный реквизит").into());
    }
    if !call_ctx(ex).ok_or_else(outside_call)?.sess.user().no_users {
        return Err(refused("сеанс с пользователями").into());
    }
    let Value::String(name) = arg_value(ex, args, 0)? else {
        return Err(refused("право не строка").into());
    };
    let name = name.to_string_lossy();
    let Some(right) = known_right(&name) else {
        return Err(HostError::text(
            &format!("Указанное право не существует: {name}"),
            &format!("The right specified doesn't exist: {name}"),
        )
        .into());
    };
    let object = arg_value(ex, args, 1)?;
    let Some((key, owner)) = key_with_app(&object) else {
        return Err(refused("объект не из Метаданные").into());
    };
    let verdict = vocabulary_kind(owner.model(), key).and_then(|kind| applies(kind, right));
    match (verdict, key) {
        (Some(true), _) => Ok(Value::Bool(true)),
        (Some(false), MdKey::Path(path)) => {
            let cx = owner
                .md_ctx()
                .ok_or_else(|| internal("the metadata context is gone"))?;
            let object = full_name(&cx, path);
            Err(HostError::text(
                &format!("Право {name} неприменимо к {object}"),
                &format!("Cannot apply the {name} right to {object}"),
            )
            .into())
        }
        _ => Err(refused(&format!(
            "право {name} для вида «{}»",
            kind_label(owner, key)
        ))
        .into()),
    }
}

pub(crate) fn register(reg: &mut Registry) {
    reg.global_fn("ПравоДоступа", "AccessRight", Arc::new(access_right));
}
