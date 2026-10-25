use crate::bind::{
    arg_bool, arg_date, arg_number, arg_str, arg_value, global, intl_error, is_given, msg,
    parse_number, raw, type_mismatch,
};
use crate::globals::tz::utc_to_session;
use bsl_lib::xml::{xml_bool, xml_date, xml_number};
use bsl_prim::{BslDate, Number};
use bsl_vm::ops::{self, CmpOp};
use bsl_vm::{Arg, CallResult, Execution, HostError, Registry, TypeId, Value};

pub(crate) fn register(reg: &mut Registry) {
    global(reg, "Строка", "String", |ex, args| {
        Ok(Value::String(arg_str(ex, args.first())?))
    });
    global(reg, "Число", "Number", |ex, args| {
        Ok(Value::Number(arg_number(ex, args.first(), 1)?))
    });
    global(reg, "Булево", "Boolean", |ex, args| {
        Ok(Value::Bool(arg_bool(ex, args.first())?))
    });
    global(reg, "Дата", "Date", date_ctor);
    global(reg, "XMLСтрока", "XMLString", xml_string);
    global(reg, "XMLЗначение", "XMLValue", xml_value);
    global(reg, "Мин", "Min", |ex, args| extreme(ex, args, false));
    global(reg, "Макс", "Max", |ex, args| extreme(ex, args, true));
    global(reg, "ЗначениеЗаполнено", "ValueIsFilled", value_is_filled);
}

fn date_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    match args.len() {
        1 => Ok(Value::Date(arg_date(ex, args.first())?)),
        3..=6 => {
            let mut parts = Vec::with_capacity(6);
            for i in 0..6 {
                let a = args.get(i);
                parts.push(if i >= 3 && !is_given(a) {
                    Number::zero()
                } else {
                    date_part(ex, a)?
                });
            }
            let d = bsl_intl::calendar::date_from_numbers(
                &parts[0], &parts[1], &parts[2], &parts[3], &parts[4], &parts[5],
            )
            .map_err(|e| intl_error(e).raw())?;
            Ok(Value::Date(d))
        }
        _ => Err(raw(msg::CONVERT_2DATE)),
    }
}

fn date_part(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Number> {
    match arg_value(ex, a)? {
        Value::Number(n) => Ok(n),
        Value::String(s) => parse_number(ex, s.units()).ok_or_else(|| raw(msg::CONVERT_2DATE)),
        _ => Err(raw(msg::CONVERT_2DATE)),
    }
}

pub(crate) fn xml_string(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let v = arg_value(ex, args.first())?;
    if let Value::String(s) = v {
        return Ok(Value::String(s));
    }
    Ok(Value::from(xml_text(ex, &v)?.as_str()))
}

pub(crate) fn xml_text(ex: &mut Execution, v: &Value) -> CallResult<String> {
    Ok(match v {
        Value::String(s) => {
            ex.reserve((s.len() as u64).saturating_mul(3))?;
            s.to_string_lossy()
        }
        Value::Undefined | Value::Null => String::new(),
        Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
        Value::Number(n) => {
            ex.reserve(n.size_hint_limbs().saturating_mul(18))?;
            n.to_xml_string()
        }
        Value::Date(d) => d.to_xml_string(),
        Value::Type(_) => return Err(HostError::key(msg::XML_CONVERSION).into()),
        v @ Value::Object(_) => match crate::serial::xmlvalue::xml_string(ex, v)? {
            Some(text) => text,
            None => return Err(HostError::key(msg::XML_CONVERSION).into()),
        },
    })
}

pub(crate) fn xml_value(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let Value::Type(t) = arg_value(ex, args.first())? else {
        return Err(type_mismatch(1).into());
    };
    let text = arg_str(ex, args.get(1))?;
    xml_from_text(ex, t, &text)
}

pub(crate) fn xml_from_text(
    ex: &mut Execution,
    t: TypeId,
    text: &bsl_prim::Str,
) -> CallResult<Value> {
    let units = text.units();
    let bad = || HostError::key(msg::XML_CONVERSION);
    let v = match t {
        TypeId::NUMBER => {
            Value::Number(xml_number(crate::serial::xmlvalue::strip_plus(units)).ok_or_else(bad)?)
        }
        TypeId::BOOL => {
            Value::Bool(xml_bool(&crate::serial::xmlvalue::ascii_lower(units)).ok_or_else(bad)?)
        }
        TypeId::DATE => {
            let x = xml_date(units)
                .or_else(|| xml_date(crate::serial::xmlvalue::date_prefix(units)))
                .ok_or_else(bad)?;
            Value::Date(match x.offset {
                None => x.date,
                Some(o) => utc_to_session(ex, x.date.add_seconds(&Number::from_i64(-i64::from(o)))),
            })
        }
        TypeId::STRING => Value::String(text.clone()),
        TypeId::UNDEFINED if units.is_empty() => Value::Undefined,
        TypeId::NULL if units.is_empty() => Value::Null,
        _ => match crate::serial::xmlvalue::xml_value(ex, t, units) {
            Some(r) => return r,
            None => return Err(bad().into()),
        },
    };
    Ok(v)
}

fn extreme(ex: &mut Execution, args: &mut [Arg], max: bool) -> CallResult<Value> {
    let mut best = arg_value(ex, args.first())?;
    for a in args.iter().skip(1) {
        let x = arg_value(ex, Some(a))?;
        let better = if max {
            ops::compare(ex, CmpOp::Lt, &best, &x)?
        } else {
            ops::compare(ex, CmpOp::Lt, &x, &best)?
        };
        if better {
            best = x;
        }
    }
    Ok(best)
}

fn value_is_filled(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let v = arg_value(ex, args.first())?;
    if crate::values::is_empty_value(&v) {
        return Ok(Value::Bool(false));
    }
    let filled = match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(_) | Value::Type(_) => true,
        Value::Number(n) => !n.is_zero(),
        Value::String(s) => !bsl_lib::trim::is_blank(s.units()),
        Value::Date(d) => d != BslDate::EMPTY,
        Value::Object(o) => !o.is_empty_value() && o.iter_len(ex).is_none_or(|n| n > 0),
    };
    Ok(Value::Bool(filled))
}
