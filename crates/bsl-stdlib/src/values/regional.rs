use crate::bind::{arg_value, global, msg, session_locale, type_mismatch};
use crate::collections::copy_value as copied;
use crate::collections::{method_name, not_implemented, property_name};
use bsl_intl::{
    format::{FmtValue, FormatEnv, format},
    locale::{Locale, locale},
};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Cause, Exception, Execution, HostError, HostObject, IbRegionalSettings, Lang,
    MemberId, MsgKey, MsgRef, Registry, RetainedMemory, SharedValue, TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use std::{any::Any, cell::RefCell, rc::Rc, sync::Arc};

const PROPERTIES: [&str; 11] = [
    "UseCurrentSessionSettings",
    "LocaleCode",
    "FirstDayOfWeek",
    "NegativeNumberPresentation",
    "BooleanTruePresentation",
    "BooleanFalsePresentation",
    "NumbersDigitGroupSeparator",
    "NumbersDecimalSeparator",
    "NumbersDigitGroupFormat",
    "TimePresentationFormat",
    "DatePresentationFormat",
];

fn type_idx() -> TypeIdx {
    crate::typedesc::idx("InfoBaseRegionalSettings")
}

struct Regional {
    values: RefCell<[Value; 11]>,
}

fn live(
    ex: &mut Execution,
    values: &[Value; 11],
    extra: &Value,
    scratch: u64,
) -> Result<u64, Exception> {
    let settings = ex.env().ib_regional_settings();
    let bytes = ex.measure_retained(scratch, |visit| {
        visit.bytes(crate::collections::accounting::retained_rc_size::<Regional>())?;
        for value in values {
            visit.value(value)?;
        }
        if let Some(settings) = &settings {
            visit.work(settings.values.len() as u64)?;
            visit.allocation(
                Arc::as_ptr(settings) as usize,
                (std::mem::size_of::<IbRegionalSettings>() as u64)
                    .saturating_add(32)
                    .saturating_add((settings.values.len() as u64).saturating_mul(
                        2 * std::mem::size_of::<(String, SharedValue)>() as u64 + 64,
                    )),
            )?;
            for (name, value) in &settings.values {
                visit.allocation(name.as_ptr() as usize, name.capacity() as u64)?;
                match value {
                    SharedValue::Number(number) => {
                        visit.allocation(
                            std::ptr::from_ref(number) as usize,
                            (number.backing_capacity_limbs() as u64).saturating_mul(4),
                        )?;
                    }
                    SharedValue::String(text) => {
                        visit.allocation(
                            Arc::as_ptr(text).cast::<()>() as usize,
                            (text.len() as u64).saturating_mul(2).saturating_add(32),
                        )?;
                    }
                    _ => {}
                }
            }
        }
        visit.value(extra)
    })?;
    ex.reserve(bytes.saturating_add(scratch))?;
    Ok(bytes.saturating_add(scratch))
}

fn defaults(ex: &mut Execution) -> CallResult<Value> {
    let size = (ex.env().ib_locale().len() as u64).saturating_mul(2);
    ex.charge(size / 128 + 1)?;
    ex.reserve(
        crate::collections::accounting::retained_rc_size::<Regional>()
            .saturating_add(size)
            .saturating_add(11 * 32),
    )?;
    let mut values = std::array::from_fn(|_| Value::from(""));
    values[0] = Value::Bool(true);
    values[2] = Value::from(0i64);
    values[3] = Value::from(-1i64);
    values[1] = Value::from(ex.env().ib_locale());
    Ok(Value::Object(Rc::new(Regional {
        values: RefCell::new(values),
    })))
}

fn get_settings(ex: &mut Execution, _: &mut [Arg]) -> CallResult<Value> {
    let Some(settings) = ex.env().ib_regional_settings() else {
        return defaults(ex);
    };
    let value = defaults(ex)?;
    let object = value
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<Regional>())
        .ok_or_else(|| type_mismatch(1))?;
    let mut values = object.values.borrow_mut();
    for (index, name) in PROPERTIES.iter().enumerate() {
        if let Some(source) = settings.values.get(*name) {
            let retained = live(ex, &values, &Value::Undefined, 0)?;
            let extra = match source {
                SharedValue::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(8),
                SharedValue::String(s) => (s.len() as u64).saturating_mul(4),
                _ => 0,
            };
            ex.reserve(retained.saturating_add(extra))?;
            values[index] = match source {
                SharedValue::Number(number) => {
                    Value::Number(crate::collections::copy_number(ex, number, retained)?)
                }
                SharedValue::String(text) => {
                    ex.charge(text.len() as u64 / 64 + 1)?;
                    Value::String(Str::from_units(text))
                }
                SharedValue::Bool(value) => Value::Bool(*value),
                _ => return Err(type_mismatch(1).into()),
            };
        }
    }
    drop(values);
    Ok(value)
}

impl Regional {
    fn property(member: MemberId) -> Option<usize> {
        let name = property_name(type_idx(), member)?;
        PROPERTIES.iter().position(|p| *p == name)
    }

    fn locale(&self, ex: &mut Execution) -> CallResult<&'static Locale> {
        let values = self.values.borrow();
        let retained = live(ex, &values, &Value::Undefined, 0)?;
        let units = match &values[1] {
            Value::String(code) => code.len() as u64,
            _ => 0,
        };
        ex.charge(units.saturating_add(1))?;
        ex.reserve(retained.saturating_add(units.saturating_mul(4)))?;
        let code = if let Value::String(code) = &values[1] {
            code.to_string_lossy()
        } else {
            String::new()
        };
        locale(&code)
            .or_else(|| session_locale(ex))
            .ok_or_else(|| type_mismatch(1).into())
    }

    fn presentation(
        &self,
        ex: &mut Execution,
        member: MemberId,
        args: &[Arg],
    ) -> CallResult<Value> {
        if args.len() != 1 {
            return Err(HostError::key(if args.is_empty() {
                "bsl:IDS_RTE_PARAMS_NOTENOUGH"
            } else {
                "bsl:IDS_RTE_PARAMS_TOOMANY"
            })
            .raw()
            .into());
        }
        let values = self.values.borrow();
        let retained = live(ex, &values, &Value::Undefined, 0)?;
        let value = match args.first() {
            Some(Arg::Val(value)) => {
                let retained = if matches!(value, Value::Number(_)) {
                    live(ex, &values, value, 0)?
                } else {
                    retained
                };
                copied(ex, value, retained)?
            }
            Some(Arg::Place(place)) => place.get_guarded(ex, retained)?,
            _ => Value::Undefined,
        };
        let method = method_name(type_idx(), member).unwrap_or("");
        let use_session = matches!(values[0], Value::Bool(true));
        let input = values
            .iter()
            .map(|value| match value {
                Value::String(s) => s.len() as u64,
                _ => 0,
            })
            .sum::<u64>();
        let numeric = match &value {
            Value::Number(n) => n.size_hint_limbs(),
            _ => 0,
        };
        ex.charge(input.saturating_add(numeric).saturating_add(1))?;
        ex.reserve(
            retained
                .saturating_add(input.saturating_mul(256))
                .saturating_add(numeric.saturating_mul(64))
                .saturating_add(8192),
        )?;
        if use_session {
            drop(values);
            if let Value::Date(date) = value {
                let locale = session_locale(ex).ok_or_else(|| type_mismatch(1))?;
                let spec = match method {
                    "DatePresentation" => "ДЛФ=D",
                    "TimePresentation" => "ДЛФ=T",
                    _ => "ДЛФ=DT",
                };
                return Ok(Value::from(
                    format(
                        FmtValue::Date(date),
                        &spec.encode_utf16().collect::<Vec<_>>(),
                        &FormatEnv { session: locale },
                    )
                    .as_str(),
                ));
            }
            return Ok(Value::String(crate::present::present_for_collection(
                ex, &value, retained,
            )?));
        }
        let locale = self.locale(ex)?;
        let string = |index: usize| match &values[index] {
            Value::String(s) => s.to_string_lossy(),
            _ => String::new(),
        };
        let mut pairs = vec![("Л", locale.name().to_owned())];
        match &value {
            Value::Number(_) => {
                pairs.push(("ЧН", String::new()));
                for (key, index) in [("ЧРГ", 6), ("ЧРД", 7), ("ЧГ", 8)] {
                    let text = string(index);
                    if !text.is_empty() {
                        pairs.push((key, text));
                    }
                }
                if let Value::Number(style) = &values[3]
                    && let Some(style) = style.to_i64().filter(|s| *s >= 0)
                {
                    pairs.push(("ЧО", style.to_string()));
                }
            }
            Value::Bool(value) => {
                let text = string(if *value { 4 } else { 5 });
                if !text.is_empty() {
                    pairs.push((if *value { "БИ" } else { "БЛ" }, text));
                }
            }
            Value::Date(_) => {
                let pattern = string(if method == "TimePresentation" { 9 } else { 10 });
                if pattern.is_empty() {
                    pairs.push((
                        "ДЛФ",
                        match method {
                            "TimePresentation" => "T",
                            "DatePresentation" => "D",
                            _ => "DT",
                        }
                        .into(),
                    ));
                } else {
                    pairs.push(("ДФ", pattern));
                }
            }
            _ => return Ok(Value::Undefined),
        }
        let mut spec = String::new();
        for (key, value) in pairs {
            spec.push_str(key);
            spec.push_str("='");
            spec.push_str(&value.replace('\'', "''"));
            spec.push_str("';");
        }
        let formatted = match &value {
            Value::Number(n) => FmtValue::Num(n),
            Value::Bool(b) => FmtValue::Bool(*b),
            Value::Date(d) => FmtValue::Date(*d),
            _ => return Ok(Value::Undefined),
        };
        Ok(Value::from(
            format(
                formatted,
                &spec.encode_utf16().collect::<Vec<_>>(),
                &FormatEnv { session: locale },
            )
            .as_str(),
        ))
    }
}

impl HostObject for Regional {
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let descriptor = &platform_catalog::catalog().types()[usize::from(type_idx().0)];
        let name = match ex.script_variant() {
            Lang::Ru => descriptor.name_ru.as_str(),
            Lang::En => descriptor.name_en.as_str(),
        };
        ex.charge(name.len() as u64 / 64 + 1)?;
        ex.reserve((name.len() as u64).saturating_mul(2).saturating_add(32))?;
        Ok(Some(Str::from(name)))
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(type_idx())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.mutable();
        for value in self.values.borrow().iter() {
            visit.value(value)?;
        }
        Ok(true)
    }
    fn get(&self, ex: &mut Execution, member: MemberId) -> CallResult<Value> {
        let index = Self::property(member).ok_or_else(|| not_implemented(type_idx(), member))?;
        let values = self.values.borrow();
        let retained = live(ex, &values, &Value::Undefined, 0)?;
        copied(ex, &values[index], retained)
    }
    fn set(&self, ex: &mut Execution, member: MemberId, value: Value) -> CallResult<()> {
        let index = Self::property(member).ok_or_else(|| not_implemented(type_idx(), member))?;
        let retained = live(ex, &self.values.borrow(), &value, 0)?;
        let value = match index {
            0 => Value::Bool(bsl_vm::ops::to_bool(ex, &value)?),
            2 | 3 => match value {
                Value::Number(_) => value,
                _ => Value::Number(bsl_vm::ops::to_number(ex, &value)?),
            },
            _ => Value::String(crate::present::present_for_collection(
                ex, &value, retained,
            )?),
        };
        live(ex, &self.values.borrow(), &value, 0)?;
        self.values.borrow_mut()[index] = value;
        Ok(())
    }
    fn call(&self, ex: &mut Execution, member: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(type_idx(), member) {
            Some("GetFirstDayOfWeek") => {
                let values = self.values.borrow();
                if let Value::Number(value) = &values[2]
                    && !value.is_zero()
                {
                    let retained = live(ex, &values, &Value::Undefined, 0)?;
                    return copied(ex, &values[2], retained);
                }
                Ok(Value::from(i64::from(
                    self.locale(ex)?.data().first_day_of_week,
                )))
            }
            Some("Presentation" | "DatePresentation" | "TimePresentation") => {
                self.presentation(ex, member, args)
            }
            _ => Err(not_implemented(type_idx(), member).into()),
        }
    }
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        type_idx(),
        TypeImpl {
            ctor: Some(Arc::new(|ex, _| defaults(ex))),
        },
    );
    global(
        reg,
        "ПолучитьРегиональныеНастройкиИнформационнойБазы",
        "GetInfoBaseRegionalSettings",
        get_settings,
    );
    global(
        reg,
        "УстановитьРегиональныеНастройкиИнформационнойБазы",
        "SetInfoBaseRegionalSettings",
        set_settings,
    );
    global(
        reg,
        "КодЛокализацииИнформационнойБазы",
        "InfoBaseLocaleCode",
        |ex, _| {
            let settings = ex.env().ib_regional_settings();
            if let Some(SharedValue::String(code)) =
                settings.as_ref().and_then(|s| s.values.get("LocaleCode"))
            {
                ex.charge(code.len() as u64 / 64 + 1)?;
                ex.reserve((code.len() as u64).saturating_mul(4).saturating_add(64))?;
                return Ok(Value::String(Str::from_units(code)));
            }
            ex.charge(ex.env().ib_locale().len() as u64 / 64 + 1)?;
            ex.reserve(
                (ex.env().ib_locale().len() as u64)
                    .saturating_mul(2)
                    .saturating_add(32),
            )?;
            Ok(Value::from(ex.env().ib_locale()))
        },
    );
}

fn bad_locale(ex: &mut Execution, code: &str) -> HostError {
    let suffix = if code.is_empty() {
        String::new()
    } else {
        format!(" '{code}'")
    };
    let mut inner = HostError::key(msg::BAD_LOCALE_NAME);
    inner.msg = MsgRef::Key(MsgKey {
        key: msg::BAD_LOCALE_NAME,
        args: vec![],
        suffix: suffix.clone(),
    });
    let mut outer = HostError::key(msg::SET_IB_LOCALE);
    let inner_text =
        platform_catalog::messages::render_key(msg::BAD_LOCALE_NAME, &[], &suffix, ex.lang())
            .unwrap_or_default();
    outer.msg = MsgRef::Key(MsgKey {
        key: msg::SET_IB_LOCALE,
        args: vec![],
        suffix: format!("\n{inner_text}"),
    });
    outer.caused_by(Cause::Host(Box::new(inner)))
}

fn set_settings(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let value = arg_value(ex, args.first())?;
    let object = value
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<Regional>())
        .ok_or_else(|| type_mismatch(1))?;
    let values = object.values.borrow();
    let retained = live(ex, &values, &Value::Undefined, 0)?;
    let code_bytes = match &values[1] {
        Value::String(s) => (s.len() as u64).saturating_mul(16),
        _ => 0,
    };
    ex.charge(code_bytes / 16 + 1)?;
    ex.reserve(retained.saturating_add(code_bytes))?;
    let code = match &values[1] {
        Value::String(code) => code.to_string_lossy(),
        _ => String::new(),
    };
    if locale(&code).is_none() {
        return Err(bad_locale(ex, &code).into());
    }
    let envelope = (std::mem::size_of::<IbRegionalSettings>() as u64)
        .saturating_add(32)
        .saturating_add(
            PROPERTIES
                .iter()
                .map(|name| {
                    name.len() as u64 + 2 * std::mem::size_of::<(String, SharedValue)>() as u64 + 64
                })
                .sum::<u64>(),
        );
    let retained = live(
        ex,
        &values,
        &Value::Undefined,
        envelope.saturating_add(code_bytes),
    )?;
    let mut map = std::collections::BTreeMap::new();
    let mut copied_bytes = 0u64;
    for (name, value) in PROPERTIES.into_iter().zip(values.iter()) {
        ex.charge(name.len() as u64 + 1)?;
        copied_bytes = copied_bytes.saturating_add(match value {
            Value::Number(n) => (n.backing_capacity_limbs() as u64).saturating_mul(4),
            Value::String(s) => (s.len() as u64).saturating_mul(2).saturating_add(32),
            _ => 0,
        });
        ex.reserve(retained.saturating_add(copied_bytes))?;
        let shared = match copied(ex, value, retained.saturating_add(copied_bytes))? {
            Value::Number(n) => SharedValue::Number(n),
            Value::Bool(b) => SharedValue::Bool(b),
            Value::String(s) => {
                ex.charge(s.len() as u64 / 64 + 1)?;
                SharedValue::String(Arc::from(s.units()))
            }
            _ => return Err(type_mismatch(1).into()),
        };
        map.insert(name.to_owned(), shared);
    }
    let app = ex.runtime().app().clone();
    app.set_ib_regional_settings(Arc::new(IbRegionalSettings { values: map }))?;
    Ok(Value::Undefined)
}
