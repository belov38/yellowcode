use super::format::locale_or_err;
use crate::bind::{arg_date, arg_str, arg_value, global, intl_error, is_given};
use crate::collections::new_array;
use bsl_intl::calendar::universal_ms;
use bsl_intl::tz::{Zone, tzdb, zone_presentation};
use bsl_prim::{BslDate, Number};
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};

pub(super) fn register(reg: &mut Registry) {
    global(reg, "МестноеВремя", "ToLocalTime", |ex, a| {
        convert(ex, a, true)
    });
    global(
        reg,
        "УниверсальноеВремя",
        "ToUniversalTime",
        |ex, a| convert(ex, a, false),
    );
    global(
        reg,
        "СмещениеСтандартногоВремени",
        "StandardTimeOffset",
        |ex, a| offset(ex, a, false),
    );
    global(
        reg,
        "СмещениеЛетнегоВремени",
        "DaylightTimeOffset",
        |ex, a| offset(ex, a, true),
    );
    global(reg, "ЧасовойПояс", "TimeZone", |ex, _| {
        Ok(Value::from(ex.env().time_zone()))
    });
    global(
        reg,
        "ЧасовойПоясСеанса",
        "SessionTimeZone",
        |ex, _| Ok(Value::from(ex.session_time_zone())),
    );
    global(
        reg,
        "УстановитьЧасовойПоясСеанса",
        "SetSessionTimeZone",
        set_session_zone,
    );
    global(
        reg,
        "ПолучитьДопустимыеЧасовыеПояса",
        "GetAvailableTimeZones",
        |_, _| {
            let names = tzdb().valid_names();
            Ok(new_array(
                names.iter().map(|n| Value::from(n.as_str())).collect(),
            ))
        },
    );
    global(
        reg,
        "ПредставлениеЧасовогоПояса",
        "TimeZonePresentation",
        presentation,
    );
    global(
        reg,
        "ТекущаяУниверсальнаяДатаВМиллисекундах",
        "CurrentUniversalDateInMilliseconds",
        |ex, _| Ok(Value::Number(universal_ms(ex.env().now_utc()))),
    );
}

pub(crate) fn parse_zone(units: &[u16]) -> Result<Zone<'static>, bsl_intl::LibError> {
    tzdb().zone(units)
}

pub(crate) fn zone_named(name: &str) -> Option<Zone<'static>> {
    let units: Vec<u16> = name.encode_utf16().collect();
    tzdb().zone(&units).ok()
}

pub(crate) fn server_zone(ex: &Execution) -> Option<Zone<'static>> {
    zone_named(ex.env().time_zone())
}

pub(crate) fn session_zone(ex: &Execution) -> Option<Zone<'static>> {
    zone_named(ex.session_time_zone()).or_else(|| server_zone(ex))
}

pub(crate) fn to_local_in(zone: Option<&Zone<'_>>, utc: BslDate) -> BslDate {
    zone.map_or(utc, |z| z.to_local(utc))
}

pub(crate) fn utc_to_session(ex: &Execution, utc: BslDate) -> BslDate {
    to_local_in(session_zone(ex).as_ref(), utc)
}

pub(crate) fn session_offset_of_local(ex: &Execution, local: BslDate) -> i32 {
    session_zone(ex).map_or(0, |z| z.offsets(z.to_universal(local)).total)
}

fn zone_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Zone<'static>> {
    if is_given(a) && !matches!(arg_value(ex, a)?, Value::Undefined) {
        let s = arg_str(ex, a)?;
        return parse_zone(s.units()).map_err(|e| intl_error(e).into());
    }
    session_zone(ex).ok_or_else(|| {
        intl_error(
            bsl_intl::LibError::new(bsl_intl::msg::BAD_TZ)
                .with_suffix(format!(" '{}'", ex.session_time_zone())),
        )
        .into()
    })
}

fn convert(ex: &mut Execution, a: &mut [Arg], to_local: bool) -> CallResult<Value> {
    let d = arg_date(ex, a.first())?;
    let z = zone_arg(ex, a.get(1))?;
    Ok(Value::Date(if to_local {
        z.to_local(d)
    } else {
        z.to_universal(d)
    }))
}

fn offset(ex: &mut Execution, a: &mut [Arg], dst: bool) -> CallResult<Value> {
    let z = zone_arg(ex, a.first())?;
    let at = if is_given(a.get(1)) && !matches!(arg_value(ex, a.get(1))?, Value::Undefined) {
        arg_date(ex, a.get(1))?
    } else {
        ex.env().now_utc()
    };
    let o = z.offsets(at);
    Ok(Value::Number(Number::from_i64(i64::from(if dst {
        o.dst
    } else {
        o.std
    }))))
}

fn set_session_zone(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let name = if is_given(a.first()) && !matches!(arg_value(ex, a.first())?, Value::Undefined) {
        let s = arg_str(ex, a.first())?;
        parse_zone(s.units()).map_err(intl_error)?.id().to_string()
    } else {
        ex.env().time_zone().to_string()
    };
    ex.set_session_time_zone(name);
    Ok(Value::Undefined)
}

fn presentation(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let z = zone_arg(ex, a.first())?;
    let loc = locale_or_err(ex)?;
    Ok(Value::from(
        zone_presentation(&z, ex.env().now_utc(), loc).as_str(),
    ))
}
