use super::refusal;
use super::schedule::{Plain, Schedule, TYPE};
use crate::meta::arg_value;
use bsl_vm::{Arg, BslDate, CallResult, Execution, Value};

const DAY: i64 = 86_400;

pub(crate) fn call(ex: &mut Execution, s: &Schedule, args: &[Arg]) -> CallResult<Value> {
    let member = "ТребуетсяВыполнение";
    let date = |v: Value, what: &str| -> CallResult<Option<i64>> {
        match v {
            Value::Undefined => Ok(None),
            Value::Date(d) if d != BslDate::EMPTY => Ok(Some(d.whole_seconds())),
            Value::Date(_) => Ok(None),
            _ => Err(refusal(TYPE, &format!("{member}: {what} не Дата")).into()),
        }
    };
    let check = match arg_value(ex, args, 0)? {
        Value::Date(d) if d == BslDate::EMPTY => {
            return Err(refusal(TYPE, &format!("{member}: пустая дата проверки")).into());
        }
        v => date(v, "дата проверки")?,
    }
    .ok_or_else(|| refusal(TYPE, &format!("{member}: без даты проверки (текущая дата)")))?;
    let last = date(arg_value(ex, args, 1)?, "дата начала последнего запуска")?;
    let end = date(
        arg_value(ex, args, 2)?,
        "дата завершения последнего запуска",
    )?
    .filter(|_| last.is_some());
    let p = s
        .plain(false)
        .and_then(|p| super::present::unseen_combination(&p).map(|()| p))
        .map_err(|why| refusal(TYPE, &format!("{member}: {why}")))?;
    Ok(Value::Bool(required(&p, check, last, end)))
}

fn day_of(s: i64) -> i64 {
    s.div_euclid(DAY)
}

fn week_day(day: i64) -> i64 {
    day.rem_euclid(7) + 1
}

fn day_in_month(day: i64) -> (i64, i64) {
    let p = BslDate::from_ticks(day * DAY * BslDate::TICKS_PER_SECOND)
        .map(|d| d.parts())
        .unwrap_or_else(|| BslDate::EMPTY.parts());
    let len = bsl_prim::days_in_month(p.year, p.month);
    (i64::from(p.day), i64::from(len))
}

fn day_matches(p: &Plain, day: i64) -> bool {
    if !p.week_days.is_empty() && !p.week_days.contains(&week_day(day)) {
        return false;
    }
    if !p.months.is_empty() {
        let month = BslDate::from_ticks(day * DAY * BslDate::TICKS_PER_SECOND)
            .map(|d| i64::from(d.parts().month));
        if !month.is_some_and(|m| p.months.contains(&m)) {
            return false;
        }
    }
    let (d, len) = day_in_month(day);
    let dim = p.day_in_month;
    if dim != 0 {
        let want = if dim > 0 { dim } else { len + 1 + dim };
        if d != want {
            return false;
        }
    }
    let w = p.week_day_in_month;
    if w != 0 {
        let k = if w > 0 {
            (d - 1) / 7 + 1
        } else {
            -((len - d) / 7 + 1)
        };
        if k != w {
            return false;
        }
    }
    true
}

fn time_matches(p: &Plain, t: i64) -> bool {
    let a = p.begin_time;
    let mut b = p.end_time;
    if let Some(c) = p.completion_time {
        let a0 = a.unwrap_or(0);
        let pos = |x: i64| (x - a0).rem_euclid(DAY);
        let limit = b.map_or(DAY - a0, pos);
        if pos(c) < limit {
            b = Some(c);
        }
    }
    match (a, b) {
        (Some(a), Some(b)) if a > b => t >= a || t <= b,
        _ => a.is_none_or(|a| t >= a) && b.is_none_or(|b| t <= b),
    }
}

pub(crate) fn required(p: &Plain, check: i64, last: Option<i64>, end: Option<i64>) -> bool {
    let day = day_of(check);
    if p.begin_date.is_some_and(|b| day < b) || p.end_date.is_some_and(|e| day > e) {
        return false;
    }
    if !day_matches(p, day) || !time_matches(p, check.rem_euclid(DAY)) {
        return false;
    }
    let Some(last) = last else {
        return true;
    };
    let last_day = day_of(last);
    if last_day == day {
        if p.repeat_period_in_day == 0 || p.days_repeat_period > 1 || p.weeks_period > 1 {
            return false;
        }
        if check - last < p.repeat_period_in_day {
            return false;
        }
        if let Some(end) = end
            && p.repeat_pause > 0
            && check - end < p.repeat_pause
        {
            return false;
        }
        return true;
    }
    if p.days_repeat_period == 0 || day - last_day < p.days_repeat_period {
        return false;
    }
    if p.weeks_period > 1 {
        let monday = |d: i64| d - (week_day(d) - 1);
        let (wl, wc) = (monday(last_day), monday(day));
        if wl == wc || (wc - wl) / 7 < p.weeks_period {
            return false;
        }
    }
    true
}
