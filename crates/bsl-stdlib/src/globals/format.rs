use super::enum_of;
use crate::bind::{
    arg_date, arg_number, arg_str, arg_value, global, intl_error, is_given, session_locale,
};
use bsl_intl::format::{FmtValue, FormatEnv, format};
use bsl_intl::locale::Locale;
use bsl_intl::nstr::nstr;
use bsl_intl::period::period_presentation;
use bsl_intl::plural::{NumberKind, string_with_number};
use bsl_intl::words::number_in_words;
use bsl_prim::Number;
use bsl_vm::{Arg, CallResult, Execution, HostError, Registry, Value};

const NUMBER_TEXT: u64 = 1500;
const DATE_LETTER: u64 = 64;

pub(super) fn register(reg: &mut Registry) {
    global(reg, "Формат", "Format", format_fn);
    global(reg, "ЧислоПрописью", "NumberInWords", words_fn);
    global(reg, "ПредставлениеПериода", "PeriodPresentation", period_fn);
    global(reg, "СтрокаСЧислом", "StringWithNumber", numcase_fn);
    global(reg, "НСтр", "NStr", nstr_fn);
    global(
        reg,
        "ТекущийКодЛокализации",
        "CurrentLocaleCode",
        |ex, _| Ok(Value::from(ex.env().locale())),
    );
}

pub(crate) fn locale_or_err(ex: &Execution) -> CallResult<&'static Locale> {
    session_locale(ex).ok_or_else(|| {
        HostError::text("Данные локали недоступны", "Locale data are not available").into()
    })
}

fn opt_str(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Option<Vec<u16>>> {
    if !is_given(a) || matches!(arg_value(ex, a)?, Value::Undefined) {
        return Ok(None);
    }
    Ok(Some(arg_str(ex, a)?.units().to_vec()))
}

fn drop_empty_lang(fmt: Option<Vec<u16>>) -> Option<Vec<u16>> {
    let fmt = fmt?;
    let kept: Vec<&[u16]> = fmt
        .split(|&u| u == u16::from(b';'))
        .filter(|part| {
            let Some(eq) = part.iter().position(|&u| u == u16::from(b'=')) else {
                return true;
            };
            let key = String::from_utf16_lossy(&part[..eq]).trim().to_lowercase();
            let empty = part[eq + 1..]
                .iter()
                .all(|&u| char::from_u32(u32::from(u)).is_some_and(char::is_whitespace));
            !(matches!(key.as_str(), "л" | "l") && empty)
        })
        .collect();
    Some(kept.join(&u16::from(b';')))
}

fn number_text_len(n: &Number, formatted: bool) -> u64 {
    if formatted {
        return NUMBER_TEXT;
    }
    n.size_hint_limbs().saturating_mul(9).clamp(32, NUMBER_TEXT)
}

fn bytes(units: usize, factor: u64) -> u64 {
    (units as u64).saturating_mul(factor).saturating_mul(2)
}

fn format_fn(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let v = arg_value(ex, a.first())?;
    let fmt = opt_str(ex, a.get(1))?.unwrap_or_default();
    let loc = locale_or_err(ex)?;
    let env = FormatEnv { session: loc };
    let text = match &v {
        Value::Number(n) => {
            let reps = fmt
                .iter()
                .filter(|&&u| matches!(u, 0x427 | 0x4E | 0x6E))
                .count()
                .max(1);
            let text = number_text_len(n, !fmt.is_empty());
            ex.reserve(
                text.saturating_mul(reps as u64)
                    .saturating_add(fmt.len() as u64)
                    .saturating_mul(2),
            )?;
            format(FmtValue::Num(n), &fmt, &env)
        }
        Value::Date(d) => {
            ex.reserve(bytes(fmt.len(), DATE_LETTER / 2).saturating_add(256))?;
            format(FmtValue::Date(*d), &fmt, &env)
        }
        Value::Bool(b) => format(FmtValue::Bool(*b), &fmt, &env),
        Value::Undefined | Value::Null => return Ok(Value::from("")),
        Value::String(s) => return Ok(Value::String(s.clone())),
        _ => return Ok(Value::String(ex.present(&v)?)),
    };
    Ok(Value::from(text.as_str()))
}

fn words_fn(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let n = arg_number(ex, a.first(), 1)?;
    let fmt = opt_str(ex, a.get(1))?;
    let subject = opt_str(ex, a.get(2))?;
    let loc = locale_or_err(ex)?;
    let units = fmt.as_ref().map_or(0, Vec::len) + subject.as_ref().map_or(0, Vec::len);
    ex.reserve(bytes(units + 1024, 4))?;
    let text = number_in_words(&n, fmt.as_deref(), subject.as_deref(), loc).map_err(intl_error)?;
    Ok(Value::from(text.as_str()))
}

fn period_fn(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let begin = arg_date(ex, a.first())?;
    let end = arg_date(ex, a.get(1))?;
    let fmt = drop_empty_lang(opt_str(ex, a.get(2))?);
    let loc = locale_or_err(ex)?;
    ex.reserve(4096)?;
    let text = period_presentation(begin, end, fmt.as_deref(), loc);
    Ok(Value::from(text.as_str()))
}

fn numcase_fn(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let forms = arg_str(ex, a.first())?;
    let n = arg_number(ex, a.get(1), 2)?;
    let kind = if is_given(a.get(2)) {
        let v = arg_value(ex, a.get(2))?;
        enum_of(&v, "ВидЧисловогоЗначения", 3)?
    } else {
        None
    };
    let Some(kind) = kind else {
        return Err(crate::bind::type_mismatch(3).into());
    };
    let kind = if kind == "Порядковое" {
        NumberKind::Ordinal
    } else {
        NumberKind::Cardinal
    };
    let fmt = drop_empty_lang(opt_str(ex, a.get(3))?);
    let loc = locale_or_err(ex)?;
    let marks = forms
        .units()
        .windows(2)
        .filter(|w| *w == [0x25, 0x31])
        .count() as u64;
    let one = number_text_len(&n, fmt.is_some());
    ex.reserve(
        (forms.units().len() as u64)
            .saturating_add(marks.saturating_mul(one))
            .saturating_mul(2),
    )?;
    let text =
        string_with_number(forms.units(), &n, kind, fmt.as_deref(), loc).map_err(intl_error)?;
    Ok(Value::from(text.as_str()))
}

fn nstr_fn(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let lang = opt_str(ex, a.get(1))?;
    let default = ex.env().config_language().to_string();
    let text = nstr(s.units(), lang.as_deref(), &default);
    Ok(Value::from(text.as_str()))
}
