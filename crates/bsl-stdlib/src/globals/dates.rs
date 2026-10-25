use super::int_value;
use super::tz::{server_zone, to_local_in, utc_to_session};
use crate::bind::{arg_date, arg_number, global};
use bsl_intl::calendar::{Period, add_months, begin_of, end_of, week_of_year};
use bsl_prim::BslDate;
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};

pub(super) fn register(reg: &mut Registry) {
    parts(reg);
    periods(reg);
    global(reg, "ДобавитьМесяц", "AddMonth", |ex, a| {
        let d = arg_date(ex, a.first())?;
        let n = arg_number(ex, a.get(1), 2)?;
        Ok(Value::Date(add_months(d, &n)))
    });
    global(reg, "ТекущаяДата", "CurrentDate", |ex, _| {
        let now = ex.env().now_utc().trunc_seconds();
        Ok(Value::Date(to_local_in(server_zone(ex).as_ref(), now)))
    });
    global(
        reg,
        "ТекущаяДатаСеанса",
        "CurrentSessionDate",
        |ex, _| {
            let now = ex.env().now_utc().trunc_seconds();
            Ok(Value::Date(utc_to_session(ex, now)))
        },
    );
    global(
        reg,
        "ТекущаяУниверсальнаяДата",
        "CurrentUniversalDate",
        |ex, _| Ok(Value::Date(ex.env().now_utc().trunc_seconds())),
    );
}

fn part(ex: &mut Execution, a: &[Arg], f: fn(BslDate) -> u32) -> CallResult<Value> {
    Ok(int_value(i64::from(f(arg_date(ex, a.first())?))))
}

fn parts(reg: &mut Registry) {
    global(reg, "Год", "Year", |ex, a| {
        part(ex, a, |d| d.parts().year)
    });
    global(reg, "Месяц", "Month", |ex, a| {
        part(ex, a, |d| d.parts().month)
    });
    global(reg, "День", "Day", |ex, a| {
        part(ex, a, |d| d.parts().day)
    });
    global(reg, "Час", "Hour", |ex, a| {
        part(ex, a, |d| d.parts().hour)
    });
    global(reg, "Минута", "Minute", |ex, a| {
        part(ex, a, |d| d.parts().minute)
    });
    global(reg, "Секунда", "Second", |ex, a| {
        part(ex, a, |d| d.parts().second)
    });
    global(reg, "ДеньНедели", "WeekDay", |ex, a| {
        part(ex, a, BslDate::day_of_week)
    });
    global(reg, "ДеньГода", "DayOfYear", |ex, a| {
        part(ex, a, BslDate::day_of_year)
    });
    global(reg, "НеделяГода", "WeekOfYear", |ex, a| {
        part(ex, a, week_of_year)
    });
}

fn periods(reg: &mut Registry) {
    const PERIODS: [(&str, &str, Period); 7] = [
        ("Минуты", "Minute", Period::Minute),
        ("Часа", "Hour", Period::Hour),
        ("Дня", "Day", Period::Day),
        ("Недели", "Week", Period::Week),
        ("Месяца", "Month", Period::Month),
        ("Квартала", "Quarter", Period::Quarter),
        ("Года", "Year", Period::Year),
    ];
    for (ru, en, p) in PERIODS {
        global(
            reg,
            &format!("Начало{ru}"),
            &format!("BegOf{en}"),
            move |ex, a| Ok(Value::Date(begin_of(arg_date(ex, a.first())?, p))),
        );
        global(
            reg,
            &format!("Конец{ru}"),
            &format!("EndOf{en}"),
            move |ex, a| Ok(Value::Date(end_of(arg_date(ex, a.first())?, p))),
        );
    }
}
