use super::{due, present, refusal};
use crate::meta::member_name;
use bsl_prim::Str;
use bsl_vm::{
    Arg, BslDate, CallResult, Exception, Execution, HostError, HostObject, MemberId, Number,
    RetainedMemory, TypeId, TypeIdx, Value, creation_number,
};
use platform_catalog::catalog;
use std::any::Any;
use std::cell::RefCell;

pub(crate) const TYPE: &str = "РасписаниеРегламентногоЗадания";

const CLASS: &str = "28193e3e-c6a3-4d30-b4e7-9f8ff23b3931";

#[derive(Clone, Debug)]
pub(crate) struct Schedule {
    pub begin_date: BslDate,
    pub end_date: BslDate,
    pub begin_time: BslDate,
    pub end_time: BslDate,
    pub completion_time: BslDate,
    pub completion_interval: Number,
    pub repeat_period_in_day: Number,
    pub repeat_pause: Number,
    pub week_days: Vec<Value>,
    pub week_day_in_month: Number,
    pub day_in_month: Number,
    pub months: Vec<Value>,
    pub weeks_period: Number,
    pub days_repeat_period: Number,
    pub detailed: Vec<Value>,
}

impl Default for Schedule {
    fn default() -> Schedule {
        let zero = Number::from_i64(0);
        Schedule {
            begin_date: BslDate::EMPTY,
            end_date: BslDate::EMPTY,
            begin_time: BslDate::EMPTY,
            end_time: BslDate::EMPTY,
            completion_time: BslDate::EMPTY,
            completion_interval: zero.clone(),
            repeat_period_in_day: zero.clone(),
            repeat_pause: zero.clone(),
            week_days: Vec::new(),
            week_day_in_month: zero.clone(),
            day_in_month: zero.clone(),
            months: Vec::new(),
            weeks_period: Number::from_i64(1),
            days_repeat_period: zero,
            detailed: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plain {
    pub week_days: Vec<i64>,
    pub months: Vec<i64>,
    pub detailed: bool,
    pub begin_date: Option<i64>,
    pub end_date: Option<i64>,
    pub begin_time: Option<i64>,
    pub end_time: Option<i64>,
    pub completion_time: Option<i64>,
    pub completion_interval: i64,
    pub repeat_period_in_day: i64,
    pub repeat_pause: i64,
    pub week_day_in_month: i64,
    pub day_in_month: i64,
    pub weeks_period: i64,
    pub days_repeat_period: i64,
}

const SECS_PER_DAY: i64 = 86_400;

fn whole(n: &Number, what: &str) -> Result<i64, String> {
    match n.to_i64() {
        Some(v) if v >= i64::from(i32::MIN) && v <= i64::from(i32::MAX) => Ok(v),
        _ => Err(format!("{what}: не целое число")),
    }
}

fn date_only(d: BslDate, what: &str) -> Result<Option<i64>, String> {
    if d == BslDate::EMPTY {
        return Ok(None);
    }
    let s = d.whole_seconds();
    if s % SECS_PER_DAY != 0 || d.frac_ticks() != 0 {
        return Err(format!("{what}: дата со временем"));
    }
    Ok(Some(s / SECS_PER_DAY))
}

fn time_only(d: BslDate, what: &str) -> Result<Option<i64>, String> {
    if d == BslDate::EMPTY {
        return Ok(None);
    }
    let s = d.whole_seconds();
    if s >= SECS_PER_DAY || d.frac_ticks() != 0 {
        return Err(format!("{what}: время с датой"));
    }
    Ok(Some(s))
}

fn list_of(items: &[Value], n: i64, what: &str) -> Result<Vec<i64>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let seen: Vec<i64> = items
        .iter()
        .map(|v| match v {
            Value::Number(x) => x.to_i64(),
            _ => None,
        })
        .collect::<Option<Vec<i64>>>()
        .ok_or_else(|| format!("{what}: не числа"))?;
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    if sorted == (1..=n).collect::<Vec<i64>>() {
        return Ok(Vec::new());
    }
    let ascending = seen.iter().zip(seen.iter().skip(1)).all(|(a, b)| a < b);
    if !ascending || seen.iter().any(|v| !(1..=n).contains(v)) {
        return Err(format!("{what}: список не по возрастанию"));
    }
    Ok(seen)
}

fn detailed_window(items: &[Value]) -> Result<(Option<i64>, Option<i64>), String> {
    let [Value::Object(o)] = items else {
        return Err("ДетальныеРасписанияДня: не одно расписание".into());
    };
    let d = o
        .as_any()
        .downcast_ref::<JobSchedule>()
        .ok_or("ДетальныеРасписанияДня: не РасписаниеРегламентногоЗадания")?
        .value
        .borrow()
        .clone();
    let only_times = Schedule {
        begin_time: d.begin_time,
        end_time: d.end_time,
        ..Schedule::default()
    };
    if format!("{d:?}") != format!("{only_times:?}")
        || d.begin_time == BslDate::EMPTY
        || d.end_time == BslDate::EMPTY
    {
        return Err("ДетальныеРасписанияДня: свойства кроме времени начала и конца".into());
    }
    Ok((
        time_only(d.begin_time, "ВремяНачала")?,
        time_only(d.end_time, "ВремяКонца")?,
    ))
}

impl Schedule {
    pub(crate) fn plain(&self, shown: bool) -> Result<Plain, String> {
        let mut begin_time = time_only(self.begin_time, "ВремяНачала")?;
        let mut end_time = time_only(self.end_time, "ВремяКонца")?;
        let detailed = !self.detailed.is_empty();
        if detailed {
            if begin_time.is_some() || end_time.is_some() {
                return Err("ДетальныеРасписанияДня со временем расписания".into());
            }
            (begin_time, end_time) = detailed_window(&self.detailed)?;
        }
        let period = &self.repeat_period_in_day;
        let repeat_period_in_day = if shown
            && !period.is_integer()
            && !period.is_negative()
            && !period.trunc().is_zero()
        {
            whole(&period.trunc(), "ПериодПовтораВТечениеДня")?
        } else {
            whole(period, "ПериодПовтораВТечениеДня")?
        };
        let p = Plain {
            week_days: list_of(&self.week_days, 7, "ДниНедели")?,
            months: list_of(&self.months, 12, "Месяцы")?,
            detailed,
            begin_date: date_only(self.begin_date, "ДатаНачала")?,
            end_date: date_only(self.end_date, "ДатаКонца")?,
            begin_time,
            end_time,
            completion_time: time_only(self.completion_time, "ВремяЗавершения")?,
            completion_interval: whole(&self.completion_interval, "ИнтервалЗавершения")?,
            repeat_period_in_day,
            repeat_pause: whole(&self.repeat_pause, "ПаузаПовтора")?,
            week_day_in_month: whole(&self.week_day_in_month, "ДеньНеделиВМесяце")?,
            day_in_month: whole(&self.day_in_month, "ДеньВМесяце")?,
            weeks_period: whole(&self.weeks_period, "ПериодНедель")?,
            days_repeat_period: whole(&self.days_repeat_period, "ПериодПовтораДней")?,
        };
        let negative = [
            ("ИнтервалЗавершения", p.completion_interval),
            ("ПериодПовтораВТечениеДня", p.repeat_period_in_day),
            ("ПаузаПовтора", p.repeat_pause),
            ("ПериодНедель", p.weeks_period),
            ("ПериодПовтораДней", p.days_repeat_period),
        ];
        if let Some((what, _)) = negative.iter().find(|(_, v)| *v < 0) {
            return Err(format!("{what}: отрицательное"));
        }
        if !(-31..=31).contains(&p.day_in_month) {
            return Err("ДеньВМесяце".into());
        }
        if !(-2..=5).contains(&p.week_day_in_month) {
            return Err(format!(
                "ДеньНеделиВМесяце {} не наблюдался",
                p.week_day_in_month
            ));
        }
        Ok(p)
    }

    pub(crate) fn of_metadata(m: &md_model::parts::Schedule) -> Result<Schedule, String> {
        if !m.detailed.is_empty() {
            return Err("детальные расписания метаданных".into());
        }
        let ymd = |t: &str| -> Result<BslDate, String> {
            let p: Vec<u32> = t
                .split('-')
                .map(|x| x.parse::<u32>())
                .collect::<Result<_, _>>()
                .map_err(|_| format!("дата {t}"))?;
            match p.as_slice() {
                [1, 1, 1] => Ok(BslDate::EMPTY),
                [y, mo, d] => {
                    BslDate::from_parts(*y, *mo, *d, 0, 0, 0).ok_or_else(|| format!("дата {t}"))
                }
                _ => Err(format!("дата {t}")),
            }
        };
        let hms = |t: &str| -> Result<BslDate, String> {
            let p: Vec<u32> = t
                .split(':')
                .map(|x| x.parse::<u32>())
                .collect::<Result<_, _>>()
                .map_err(|_| format!("время {t}"))?;
            match p.as_slice() {
                [0, 0, 0] => Ok(BslDate::EMPTY),
                [h, mi, se] => {
                    BslDate::from_parts(1, 1, 1, *h, *mi, *se).ok_or_else(|| format!("время {t}"))
                }
                _ => Err(format!("время {t}")),
            }
        };
        let num = |n: i64| Number::from_i64(n);
        let list = |v: &[i64]| {
            v.iter()
                .map(|x| Value::Number(Number::from_i64(*x)))
                .collect()
        };
        Ok(Schedule {
            begin_date: ymd(&m.begin_date)?,
            end_date: ymd(&m.end_date)?,
            begin_time: hms(&m.begin_time)?,
            end_time: hms(&m.end_time)?,
            completion_time: hms(&m.completion_time)?,
            completion_interval: num(m.completion_interval),
            repeat_period_in_day: num(m.repeat_period_in_day),
            repeat_pause: num(m.repeat_pause),
            week_days: list(&m.week_days),
            week_day_in_month: num(m.week_day_in_month),
            day_in_month: num(m.day_in_month),
            months: list(&m.months),
            weeks_period: num(m.weeks_period),
            days_repeat_period: num(m.days_repeat_period),
            detailed: Vec::new(),
        })
    }

    pub(crate) fn from_internal(text: &str) -> Result<Schedule, String> {
        let text = text.trim_start_matches('\u{feff}');
        let node = brace::parse(text).map_err(|e| format!("расписание: {e}"))?;
        let items = node.as_list().ok_or("расписание: не список")?;
        match items {
            [tag, class, body] if tag.text() == Some("#") && class.text() == Some(CLASS) => {
                Schedule::from_fields(body, true)
            }
            _ => Err("расписание: не значение расписания".into()),
        }
    }

    pub(crate) fn to_internal(&self) -> Result<String, String> {
        if !self.detailed.is_empty() {
            return Err("детальные расписания дня".into());
        }
        let num = |n: &Number, what: &str| -> Result<String, String> {
            n.to_i64()
                .map(|v| v.to_string())
                .ok_or_else(|| format!("{what}: не целое число"))
        };
        let list = |items: &[Value], what: &str| -> Result<Vec<String>, String> {
            let mut out = vec![items.len().to_string()];
            for v in items {
                match v {
                    Value::Number(n) => out.push(num(n, what)?),
                    _ => return Err(format!("{what}: не числа")),
                }
            }
            Ok(out)
        };
        let mut f: Vec<String> = [
            self.begin_date,
            self.end_date,
            self.begin_time,
            self.end_time,
            self.completion_time,
        ]
        .iter()
        .map(|d| date14(*d))
        .collect();
        f.push(num(&self.completion_interval, "ИнтервалЗавершения")?);
        f.push(num(&self.repeat_period_in_day, "ПериодПовтораВТечениеДня")?);
        f.push(num(&self.repeat_pause, "ПаузаПовтора")?);
        f.extend(list(&self.week_days, "ДниНедели")?);
        f.push(num(&self.week_day_in_month, "ДеньНеделиВМесяце")?);
        f.push(num(&self.day_in_month, "ДеньВМесяце")?);
        f.extend(list(&self.months, "Месяцы")?);
        f.push(num(&self.weeks_period, "ПериодНедель")?);
        f.push(num(&self.days_repeat_period, "ПериодПовтораДней")?);
        f.push("0".into());
        Ok(format!("{{\"#\",{CLASS},\r\n{{{}}}\r\n}}", f.join(",")))
    }

    pub(crate) fn of_value(v: &Value) -> Option<Schedule> {
        match v {
            Value::Object(o) => o
                .as_any()
                .downcast_ref::<JobSchedule>()
                .map(|s| s.value.borrow().clone()),
            _ => None,
        }
    }

    fn from_fields(node: &brace::Node, top: bool) -> Result<Schedule, String> {
        let items = node.as_list().ok_or("расписание: поля не список")?;
        let mut at = 0usize;
        let mut next = || -> Result<&brace::Node, String> {
            let n = items.get(at).ok_or("расписание: мало полей")?;
            at += 1;
            Ok(n)
        };
        let date = |n: &brace::Node| -> Result<BslDate, String> {
            let t = n.text().ok_or("расписание: дата")?;
            parse_date14(t).ok_or_else(|| format!("расписание: дата {t}"))
        };
        let num = |n: &brace::Node| -> Result<i64, String> {
            n.text()
                .and_then(|t| t.parse::<i64>().ok())
                .ok_or_else(|| "расписание: число".to_string())
        };
        let mut s = Schedule {
            begin_date: date(next()?)?,
            end_date: date(next()?)?,
            begin_time: date(next()?)?,
            end_time: date(next()?)?,
            completion_time: date(next()?)?,
            ..Schedule::default()
        };
        s.completion_interval = Number::from_i64(num(next()?)?);
        s.repeat_period_in_day = Number::from_i64(num(next()?)?);
        s.repeat_pause = Number::from_i64(num(next()?)?);
        let n = num(next()?)?;
        for _ in 0..n.clamp(0, 7) {
            s.week_days
                .push(Value::Number(Number::from_i64(num(next()?)?)));
        }
        if n > 7 {
            return Err("расписание: дни недели".into());
        }
        s.week_day_in_month = Number::from_i64(num(next()?)?);
        s.day_in_month = Number::from_i64(num(next()?)?);
        let m = num(next()?)?;
        if m > 12 {
            return Err("расписание: месяцы".into());
        }
        for _ in 0..m.clamp(0, 12) {
            s.months
                .push(Value::Number(Number::from_i64(num(next()?)?)));
        }
        s.weeks_period = Number::from_i64(num(next()?)?);
        s.days_repeat_period = Number::from_i64(num(next()?)?);
        if top {
            let k = num(next()?)?;
            if k != 0 {
                return Err("расписание: детальные расписания дня".into());
            }
        }
        if at != items.len() {
            return Err("расписание: лишние поля".into());
        }
        Ok(s)
    }
}

fn date14(d: BslDate) -> String {
    let p = d.parts();
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    )
}

fn parse_date14(t: &str) -> Option<BslDate> {
    if t.len() != 14 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let part = |a: usize, b: usize| t.get(a..b).and_then(|x| x.parse::<u32>().ok());
    BslDate::from_parts(
        part(0, 4)?,
        part(4, 6)?,
        part(6, 8)?,
        part(8, 10)?,
        part(10, 12)?,
        part(12, 14)?,
    )
}

pub(crate) struct JobSchedule {
    ty: TypeIdx,
    value: RefCell<Schedule>,
    hash: i32,
}

impl JobSchedule {
    pub(crate) fn new_value(ty: TypeIdx, s: Schedule) -> Value {
        Value::Object(std::rc::Rc::new(JobSchedule {
            ty,
            value: RefCell::new(s),
            hash: creation_number() as i32,
        }))
    }

    fn name(&self, m: MemberId) -> Option<(bool, &'static str, &'static str)> {
        member_name(catalog(), self.ty, m)
    }
}

fn copy_array(items: &[Value]) -> Value {
    bsl_stdlib::new_array(items.to_vec())
}

fn take_array(v: &Value, what: &str) -> CallResult<Vec<Value>> {
    bsl_stdlib::array_items(v)
        .ok_or_else(|| refusal(TYPE, &format!("{what}: значение не Массив")).into())
}

fn take_date(v: Value, what: &str) -> CallResult<BslDate> {
    match v {
        Value::Date(d) => Ok(d),
        _ => Err(refusal(TYPE, &format!("{what}: значение не Дата")).into()),
    }
}

fn take_number(v: Value, what: &str) -> CallResult<Number> {
    match v {
        Value::Number(n) => Ok(n),
        _ => Err(refusal(TYPE, &format!("{what}: значение не Число")).into()),
    }
}

fn keeps_empty(empty: bool, what: &str) -> CallResult<()> {
    if empty {
        Ok(())
    } else {
        Err(refusal(TYPE, &format!("{what}: значение другого типа")).into())
    }
}

impl HostObject for JobSchedule {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let s = self.value.borrow();
        let v = match self.name(m) {
            Some((false, _, "BeginDate")) => Value::Date(s.begin_date),
            Some((false, _, "EndDate")) => Value::Date(s.end_date),
            Some((false, _, "BeginTime")) => Value::Date(s.begin_time),
            Some((false, _, "EndTime")) => Value::Date(s.end_time),
            Some((false, _, "CompletionTime")) => Value::Date(s.completion_time),
            Some((false, _, "CompletionInterval")) => Value::Number(s.completion_interval.clone()),
            Some((false, _, "RepeatPeriodInDay")) => Value::Number(s.repeat_period_in_day.clone()),
            Some((false, _, "RepeatPause")) => Value::Number(s.repeat_pause.clone()),
            Some((false, _, "WeekDayInMonth")) => Value::Number(s.week_day_in_month.clone()),
            Some((false, _, "DayInMonth")) => Value::Number(s.day_in_month.clone()),
            Some((false, _, "WeeksPeriod")) => Value::Number(s.weeks_period.clone()),
            Some((false, _, "DaysRepeatPeriod")) => Value::Number(s.days_repeat_period.clone()),
            Some((false, _, "WeekDays")) => copy_array(&s.week_days),
            Some((false, _, "Months")) => copy_array(&s.months),
            Some((false, _, "DetailedDailySchedules")) => copy_array(&s.detailed),
            Some((_, ru, _)) => return Err(refusal(TYPE, ru).into()),
            None => return Err(crate::meta::no_field(&m.0.to_string()).into()),
        };
        Ok(v)
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let Some((false, ru, en)) = self.name(m) else {
            return Err(crate::meta::no_field(&m.0.to_string()).into());
        };
        let mut s = self.value.borrow_mut();
        match (en, v) {
            ("DaysRepeatPeriod", Value::String(t)) => {
                let t = t.to_string_lossy();
                if t != "1" {
                    return Err(refusal(TYPE, &format!("{ru}: значение не Число")).into());
                }
                s.days_repeat_period = Number::parse(&t)
                    .ok_or_else(|| refusal(TYPE, &format!("{ru}: значение не Число")))?;
            }
            ("WeekDays", Value::Number(_)) => keeps_empty(s.week_days.is_empty(), ru)?,
            ("BeginDate", Value::Number(_)) => keeps_empty(s.begin_date == BslDate::EMPTY, ru)?,
            ("BeginTime", Value::String(t))
                if !t.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) =>
            {
                keeps_empty(s.begin_time == BslDate::EMPTY, ru)?
            }
            (en, v) => set_typed(&mut s, en, ru, v)?,
        }
        Ok(())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match self.name(m) {
            Some((true, _, "ExecutionRequired")) => {
                let s = self.value.borrow().clone();
                due::call(ex, &s, args)
            }
            Some((_, ru, _)) => Err(refusal(TYPE, ru).into()),
            None => Err(crate::meta::no_field(&m.0.to_string()).into()),
        }
    }

    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        let s = self.value.borrow();
        let plain = s
            .plain(true)
            .map_err(|why| HostError::not_implemented(TYPE, &format!("Представление: {why}")))?;
        present::text(&plain)
            .map(|t| Some(Str::from(t.as_str())))
            .map_err(|why| {
                HostError::not_implemented(TYPE, &format!("Представление: {why}")).into()
            })
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }

    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(false)
    }
}

fn set_typed(s: &mut Schedule, en: &str, ru: &str, v: Value) -> CallResult<()> {
    match en {
        "BeginDate" => s.begin_date = take_date(v, ru)?,
        "EndDate" => s.end_date = take_date(v, ru)?,
        "BeginTime" => s.begin_time = take_date(v, ru)?,
        "EndTime" => s.end_time = take_date(v, ru)?,
        "CompletionTime" => s.completion_time = take_date(v, ru)?,
        "CompletionInterval" => s.completion_interval = take_number(v, ru)?,
        "RepeatPeriodInDay" => s.repeat_period_in_day = take_number(v, ru)?,
        "RepeatPause" => s.repeat_pause = take_number(v, ru)?,
        "WeekDayInMonth" => s.week_day_in_month = take_number(v, ru)?,
        "DayInMonth" => s.day_in_month = take_number(v, ru)?,
        "WeeksPeriod" => s.weeks_period = take_number(v, ru)?,
        "DaysRepeatPeriod" => s.days_repeat_period = take_number(v, ru)?,
        "WeekDays" => s.week_days = take_array(&v, ru)?,
        "Months" => s.months = take_array(&v, ru)?,
        "DetailedDailySchedules" => s.detailed = take_array(&v, ru)?,
        _ => return Err(refusal(TYPE, ru).into()),
    }
    Ok(())
}
