use crate::Application;
use crate::meta::{arg_value, key_of, type_mismatch};
use crate::model::MdPath;
use crate::read_budget;
use crate::session::call_ctx;
use bsl_vm::{Arg, CallResult, Execution, HostError, Registry, Value};
use platform_catalog::messages::Lang;
use platform_catalog::{TypeIdx, catalog};
use std::sync::Arc;

const LEVEL: &str = "УровеньЖурналаРегистрации";
const MODE: &str = "РежимТранзакцииЗаписиЖурналаРегистрации";

fn levels_of(code: u32) -> Option<&'static [&'static str]> {
    Some(match code {
        0 => &["Note", "Information", "Warning", "Error"],
        1 => &["Information", "Warning", "Error"],
        2 => &["Warning", "Error"],
        3 => &["Error"],
        100 => &[],
        _ => return None,
    })
}

fn refusal(function: &str, what: &str) -> HostError {
    HostError::not_implemented("Глобальный контекст", &format!("{function}: {what}"))
}

fn enum_type(name: &str) -> Option<TypeIdx> {
    catalog().find_type(name)
}

fn enum_name(v: &Value, name: &str) -> Option<&'static str> {
    let e = bsl_stdlib::enums::as_enum_value(v)?;
    (Some(e.type_idx()) == enum_type(name)).then(|| e.name_en())
}

fn usage(app: &Application, ex: &mut Execution) -> CallResult<Value> {
    const NAME: &str = "ПолучитьИспользованиеЖурналаРегистрации";
    let code = app
        .options()
        .event_log_level
        .ok_or_else(|| refusal(NAME, "настройки журнала базы не прочитаны"))?;
    let names = levels_of(code).ok_or_else(|| refusal(NAME, &format!("уровень {code}")))?;
    let t = enum_type(LEVEL).ok_or_else(|| refusal(NAME, LEVEL))?;
    let items = names
        .iter()
        .map(|n| bsl_stdlib::enums::enum_value(t, n).ok_or_else(|| refusal(NAME, n)))
        .collect::<Result<Vec<Value>, HostError>>()?;
    read_budget::reserve(ex, 256)?;
    Ok(bsl_stdlib::new_array(items))
}

fn level(v: &Value) -> Option<evlog::Level> {
    Some(match v {
        Value::Undefined => evlog::Level::Note,
        _ => match enum_name(v, LEVEL)? {
            "Information" => evlog::Level::Information,
            "Error" => evlog::Level::Error,
            "Warning" => evlog::Level::Warning,
            "Note" => evlog::Level::Note,
            _ => return None,
        },
    })
}

fn metadata(app: &Application, v: &Value) -> Result<evlog::Meta, HostError> {
    const NAME: &str = "ЗаписьЖурналаРегистрации";
    if matches!(v, Value::Undefined) {
        return Ok(evlog::Meta::None);
    }
    match key_of(v) {
        Some(crate::meta::MdKey::Path(MdPath::Object(obj))) => {
            let def = app.model().obj(obj);
            Ok(evlog::Meta::One(evlog::MetaRef {
                uuid: def.md,
                name: format!("{}.{}", def.kind.name(Lang::Ru), def.name),
            }))
        }
        _ => Err(refusal(NAME, "объект метаданных не объект конфигурации")),
    }
}

fn write_event(app: &Application, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
    let event = match arg_value(ex, args, 0)? {
        Value::String(s) => s.to_string_lossy(),
        _ => return Err(type_mismatch(1).into()),
    };
    let level = level(&arg_value(ex, args, 1)?).ok_or_else(|| type_mismatch(2))?;
    let metadata = metadata(app, &arg_value(ex, args, 2)?)?;
    let (data, data_presentation) = match arg_value(ex, args, 3)? {
        Value::Undefined => (evlog::data::undefined(), String::new()),
        Value::String(s) => (evlog::data::string(&s.to_string_lossy()), String::new()),
        other => (
            evlog::data::undefined(),
            ex.present(&other)?.to_string_lossy(),
        ),
    };
    let comment = match arg_value(ex, args, 4)? {
        Value::Undefined => String::new(),
        Value::String(s) => s.to_string_lossy(),
        _ => return Err(type_mismatch(5).into()),
    };
    let mode = arg_value(ex, args, 5)?;
    if !matches!(mode, Value::Undefined) && enum_name(&mode, MODE).is_none() {
        return Err(type_mismatch(6).into());
    }
    let text = [&event, &comment, &data_presentation]
        .iter()
        .fold(1024u64, |n, s| {
            n.saturating_add((s.len() as u64).saturating_mul(2))
        });
    read_budget::reserve(ex, text)?;
    let time = ex.env().now_utc();
    let (user, number, computer) = match call_ctx(ex) {
        Some(c) => {
            let u = c.sess.user();
            let user = (!u.no_users).then(|| evlog::UserRef {
                uuid: u.uuid,
                name: u.name.clone(),
            });
            (user, c.sess.number(), c.sess.computer().to_string())
        }
        None => (None, 0, String::new()),
    };
    app.event_sink().write(evlog::Event {
        time,
        level,
        status: evlog::TxStatus::None,
        txn: None,
        user,
        computer,
        app: String::new(),
        connection: number,
        session: number,
        event,
        comment,
        metadata,
        data,
        data_presentation,
        server: String::new(),
        main_port: 0,
        aux_port: 0,
        separators: Vec::new(),
    });
    Ok(Value::Undefined)
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let on_usage = app.clone();
    reg.global_fn(
        "ПолучитьИспользованиеЖурналаРегистрации",
        "GetEventLogUsing",
        Arc::new(move |ex, _| usage(&on_usage, ex)),
    );
    let on_write = app.clone();
    reg.global_fn(
        "ЗаписьЖурналаРегистрации",
        "WriteLogEvent",
        Arc::new(move |ex, args| write_event(&on_write, ex, args)),
    );
}
