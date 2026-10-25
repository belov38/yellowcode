use super::{enum_arg, int_value, reserve_units, str_value};
use crate::bind::{
    arg_bool, arg_i64, arg_str, arg_value, global, is_given, lib_error, type_mismatch,
};
use crate::collections::{array_items, new_array};
use bsl_lib::strings::{self, Direction};
use bsl_lib::{case, lines, template, trim};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};
use std::cmp::Ordering;

type Args<'a> = &'a mut [Arg];

fn len_value(units: usize) -> Value {
    int_value(i64::try_from(units).unwrap_or(i64::MAX))
}

pub(super) fn register(reg: &mut Registry) {
    global(reg, "СтрДлина", "StrLen", |ex, a| {
        Ok(len_value(arg_str(ex, a.first())?.len()))
    });
    global(reg, "Лев", "Left", |ex, a| {
        let s = arg_str(ex, a.first())?;
        let n = arg_i64(ex, a.get(1), 2)?;
        Ok(str_value(strings::left(s.units(), n).to_vec()))
    });
    global(reg, "Прав", "Right", |ex, a| {
        let s = arg_str(ex, a.first())?;
        let n = arg_i64(ex, a.get(1), 2)?;
        Ok(str_value(strings::right(s.units(), n).to_vec()))
    });
    global(reg, "Сред", "Mid", |ex, a| {
        let s = arg_str(ex, a.first())?;
        let start = arg_i64(ex, a.get(1), 2)?;
        let len = opt_i64(ex, a.get(2), 3)?;
        Ok(str_value(strings::mid(s.units(), start, len).to_vec()))
    });
    global(reg, "СтрНайти", "StrFind", str_find);
    global(reg, "Найти", "Find", |ex, a| {
        let s = arg_str(ex, a.first())?;
        let sub = arg_str(ex, a.get(1))?;
        Ok(int_value(strings::find_legacy(s.units(), sub.units())))
    });
    global(
        reg,
        "СтрЧислоВхождений",
        "StrOccurrenceCount",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let sub = arg_str(ex, a.get(1))?;
            Ok(int_value(strings::count(s.units(), sub.units())))
        },
    );
    global(reg, "СтрЗаменить", "StrReplace", str_replace);
    global(reg, "СтрРазделить", "StrSplit", str_split);
    global(reg, "СтрСоединить", "StrConcat", str_concat);
    global(
        reg,
        "СтрНачинаетсяС",
        "StrStartsWith",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let p = arg_str(ex, a.get(1))?;
            let r = strings::starts_with(s.units(), p.units()).map_err(lib_error)?;
            Ok(Value::Bool(r))
        },
    );
    global(
        reg,
        "СтрЗаканчиваетсяНа",
        "StrEndsWith",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let p = arg_str(ex, a.get(1))?;
            let r = strings::ends_with(s.units(), p.units()).map_err(lib_error)?;
            Ok(Value::Bool(r))
        },
    );
    global(reg, "Символ", "Char", |ex, a| {
        let code = arg_i64(ex, a.first(), 1)?;
        Ok(str_value(strings::char_from_code(code)))
    });
    global(reg, "КодСимвола", "CharCode", |ex, a| {
        let s = arg_str(ex, a.first())?;
        let pos = opt_i64(ex, a.get(1), 2)?.unwrap_or(1);
        Ok(int_value(strings::char_code(s.units(), pos)))
    });
    global(reg, "ВРег", "Upper", |ex, a| {
        map_text(ex, a, 3, case::upper)
    });
    global(reg, "НРег", "Lower", |ex, a| {
        map_text(ex, a, 3, case::lower)
    });
    global(reg, "ТРег", "Title", |ex, a| {
        map_text(ex, a, 3, case::title)
    });
    global(reg, "СокрЛ", "TrimL", |ex, a| {
        map_text(ex, a, 0, |t| trim::trim_left(t).to_vec())
    });
    global(reg, "СокрП", "TrimR", |ex, a| {
        map_text(ex, a, 0, |t| trim::trim_right(t).to_vec())
    });
    global(reg, "СокрЛП", "TrimAll", |ex, a| {
        map_text(ex, a, 0, |t| trim::trim(t).to_vec())
    });
    global(reg, "ПустаяСтрока", "IsBlankString", |ex, a| {
        Ok(Value::Bool(trim::is_blank(arg_str(ex, a.first())?.units())))
    });
    global(
        reg,
        "СтрЧислоСтрок",
        "StrLineCount",
        |ex, a| {
            Ok(int_value(lines::line_count(
                arg_str(ex, a.first())?.units(),
            )))
        },
    );
    global(
        reg,
        "СтрПолучитьСтроку",
        "StrGetLine",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let n = arg_i64(ex, a.get(1), 2)?;
            Ok(str_value(lines::line(s.units(), n).to_vec()))
        },
    );
    global(reg, "СтрШаблон", "StrTemplate", str_template);
    global(reg, "СтрСравнить", "StrCompare", |ex, a| {
        let x = arg_str(ex, a.first())?;
        let y = arg_str(ex, a.get(1))?;
        let ord = ex.env().collation().compare_secondary(x.units(), y.units());
        Ok(int_value(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    });
}

fn opt_i64(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Option<i64>> {
    if is_given(a) {
        Ok(Some(arg_i64(ex, a, param)?))
    } else {
        Ok(None)
    }
}

fn map_text(
    ex: &mut Execution,
    a: Args,
    grow: usize,
    f: impl Fn(&[u16]) -> Vec<u16>,
) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    if grow > 0 {
        reserve_units(ex, s.len(), grow)?;
    }
    Ok(str_value(f(s.units())))
}

fn str_find(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let sub = arg_str(ex, a.get(1))?;
    let dir = match enum_arg(ex, a.get(2), "НаправлениеПоиска", 3)? {
        Some("СКонца") => Direction::FromEnd,
        _ => Direction::FromStart,
    };
    let start = opt_i64(ex, a.get(3), 4)?;
    let occurrence = opt_i64(ex, a.get(4), 5)?;
    let r = strings::find(s.units(), sub.units(), dir, start, occurrence).map_err(lib_error)?;
    Ok(int_value(r))
}

fn str_replace(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let from = arg_str(ex, a.get(1))?;
    let to = arg_str(ex, a.get(2))?;
    if to.len() > from.len() && !from.is_empty() {
        let matches = s.len() / from.len();
        let total = s
            .len()
            .saturating_add(matches.saturating_mul(to.len() - from.len()));
        reserve_units(ex, total, 1)?;
    }
    Ok(str_value(strings::replace(
        s.units(),
        from.units(),
        to.units(),
    )))
}

fn str_split(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let delims = arg_str(ex, a.get(1))?;
    let keep_empty = if is_given(a.get(2)) {
        arg_bool(ex, a.get(2))?
    } else {
        true
    };
    reserve_units(ex, s.len(), 8)?;
    let parts = strings::split(s.units(), delims.units(), keep_empty);
    Ok(new_array(parts.into_iter().map(str_value).collect()))
}

fn str_concat(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let v = arg_value(ex, a.first())?;
    let Some(items) = array_items(&v) else {
        return Err(type_mismatch(1).into());
    };
    let sep = arg_str(ex, a.get(1))?;
    let mut total = sep.len().saturating_mul(items.len());
    for it in &items {
        total = total.saturating_add(match it {
            Value::String(s) => s.len(),
            _ => 64,
        });
    }
    reserve_units(ex, total, 1)?;
    let mut out: Vec<u16> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep.units());
        }
        match it {
            Value::String(s) => out.extend_from_slice(s.units()),
            other => out.extend_from_slice(ex.present(other)?.units()),
        }
    }
    Ok(str_value(out))
}

fn str_template(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let fmt = arg_str(ex, a.first())?;
    let mut values: Vec<Vec<u16>> = Vec::new();
    let mut longest = 0;
    for i in 1..a.len() {
        let s: Str = arg_str(ex, a.get(i))?;
        longest = longest.max(s.len());
        values.push(s.units().to_vec());
    }
    let marks = fmt.units().iter().filter(|&&u| u == 0x25).count();
    reserve_units(
        ex,
        fmt.len().saturating_add(marks.saturating_mul(longest)),
        1,
    )?;
    let out = template::template(fmt.units(), &values).map_err(lib_error)?;
    Ok(str_value(out))
}
