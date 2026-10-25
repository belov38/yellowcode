use bsl_prim::{BslDate, Number};
use query_ir::fold;
use query_ir::{ExecError, QValue, RuntimeError};
use query_syntax::{Func, Kw, MsgId, QueryError};

use super::fatal;
use super::strings::int_arg;

const TPS: i64 = BslDate::TICKS_PER_SECOND;
const UNITS: [Kw; 10] = [
    Kw::Second,
    Kw::Minute,
    Kw::Hour,
    Kw::Day,
    Kw::Week,
    Kw::TenDays,
    Kw::Month,
    Kw::Quarter,
    Kw::HalfYear,
    Kw::Year,
];

pub fn unit_arg(k: Kw) -> QValue {
    QValue::String(query_ir::QStr::from_str(k.ru()))
}

pub fn unit_of(v: &QValue) -> Option<Kw> {
    let QValue::String(s) = v else { return None };
    let name = s.to_string_lossy().to_uppercase();
    UNITS
        .iter()
        .copied()
        .find(|k| k.ru().to_uppercase() == name || k.en().to_uppercase() == name)
}

fn unit_arg_at(args: &[QValue], i: usize) -> Result<Kw, ExecError> {
    args.get(i)
        .and_then(unit_of)
        .ok_or_else(|| fatal("a period unit is expected"))
}

fn date_at(args: &[QValue], i: usize) -> Result<BslDate, ExecError> {
    match args.get(i) {
        Some(QValue::Date(d)) => Ok(*d),
        _ => Err(fatal("a date argument is expected")),
    }
}

fn months_of(unit: Kw) -> Option<i128> {
    match unit {
        Kw::Month => Some(1),
        Kw::Quarter => Some(3),
        Kw::HalfYear => Some(6),
        Kw::Year => Some(12),
        _ => None,
    }
}

fn seconds_of(unit: Kw) -> Option<i128> {
    match unit {
        Kw::Second => Some(1),
        Kw::Minute => Some(60),
        Kw::Hour => Some(3600),
        Kw::Day => Some(86_400),
        Kw::Week => Some(7 * 86_400),
        Kw::TenDays => Some(10 * 86_400),
        _ => None,
    }
}

fn from_ticks_sat(t: i128) -> BslDate {
    if t < 0 {
        BslDate::EMPTY
    } else {
        i64::try_from(t)
            .ok()
            .and_then(BslDate::from_ticks)
            .unwrap_or(BslDate::MAX)
    }
}

pub(crate) fn add(d: BslDate, unit: Kw, k: i64) -> BslDate {
    let k = i128::from(k);
    if let Some(m) = months_of(unit) {
        let p = d.parts();
        let total = i128::from(p.year) * 12 + i128::from(p.month) - 1 + k * m;
        if total < 0 || total / 12 < 1 {
            return BslDate::EMPTY;
        }
        let (y, mo) = (total / 12, total % 12 + 1);
        if y > 9999 {
            return BslDate::MAX;
        }
        let (y, mo) = (y as u32, mo as u32);
        let day = p.day.min(bsl_prim::days_in_month(y, mo));
        return match BslDate::from_parts(y, mo, day, p.hour, p.minute, p.second) {
            Some(b) => from_ticks_sat(i128::from(b.ticks()) + i128::from(d.frac_ticks())),
            None => d,
        };
    }
    match seconds_of(unit) {
        Some(s) => from_ticks_sat(i128::from(d.ticks()) + k * s * i128::from(TPS)),
        None => d,
    }
}

pub(crate) fn end_of_period(d: BslDate, unit: Kw) -> BslDate {
    let b = fold::begin_of_period(d, unit);
    let p = b.parts();
    let day = 86_400 * TPS;
    let next = |months: u32| -> Option<i64> {
        let total = p.year * 12 + p.month - 1 + months;
        BslDate::from_parts(total / 12, total % 12 + 1, 1, 0, 0, 0).map(BslDate::ticks)
    };
    let next_start = match unit {
        Kw::Year => next(12),
        Kw::HalfYear => next(6),
        Kw::Quarter => next(3),
        Kw::Month => next(1),
        Kw::TenDays => {
            let dim = bsl_prim::days_in_month(p.year, p.month);
            if p.day == 21 {
                next(1)
            } else {
                Some(b.ticks() + i64::from((p.day + 10).min(dim + 1) - p.day) * day)
            }
        }
        Kw::Week => Some(b.ticks() + 7 * day),
        Kw::Day => Some(b.ticks() + day),
        Kw::Hour => Some(b.ticks() + 3600 * TPS),
        Kw::Minute => Some(b.ticks() + 60 * TPS),
        _ => return d.trunc_seconds(),
    };
    match next_start {
        Some(t) => BslDate::from_ticks(t - TPS).unwrap_or(BslDate::MAX),
        None => BslDate::MAX,
    }
}

fn field_overflow(detail: &str) -> ExecError {
    ExecError::Runtime(RuntimeError::sql(
        QueryError::new(MsgId::DBERROR_SQLFIELDOVERFLOW, None).with_detail(detail),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateDiffError {
    Unsupported,
    FieldOverflow,
}

pub(crate) fn diff_value(a: BslDate, b: BslDate, unit: Kw) -> Result<i64, DateDiffError> {
    let (sa, sb) = (a.whole_seconds(), b.whole_seconds());
    let months = |d: BslDate| {
        let p = d.parts();
        i64::from(p.year) * 12 + i64::from(p.month) - 1
    };
    let by = |step: i64| sb.div_euclid(step) - sa.div_euclid(step);
    let r = match unit {
        Kw::Second => sb - sa,
        Kw::Minute => by(60),
        Kw::Hour => by(3600),
        Kw::Day => by(86_400),
        Kw::Month => months(b) - months(a),
        Kw::Quarter => months(b).div_euclid(3) - months(a).div_euclid(3),
        Kw::Year => i64::from(b.parts().year) - i64::from(a.parts().year),
        _ => return Err(DateDiffError::Unsupported),
    };
    if i32::try_from(r).is_err() {
        return Err(DateDiffError::FieldOverflow);
    }
    Ok(r)
}

fn diff(a: BslDate, b: BslDate, unit: Kw) -> Result<i64, ExecError> {
    diff_value(a, b, unit).map_err(|error| match error {
        DateDiffError::Unsupported => fatal("unit is not supported by РАЗНОСТЬДАТ"),
        DateDiffError::FieldOverflow => field_overflow(" 'DATEDIFF'"),
    })
}

fn num(v: i64) -> QValue {
    QValue::Number(Number::from_i64(v))
}

pub(super) fn call(f: Func, args: &[QValue]) -> Result<QValue, ExecError> {
    Ok(match f {
        Func::DateAdd => {
            let (d, unit, n) = (date_at(args, 0)?, unit_arg_at(args, 1)?, args.get(2));
            let k = int_arg(n.ok_or_else(|| fatal("too few arguments"))?)?;
            QValue::Date(add(d, unit, k))
        }
        Func::DateDiff => {
            let (a, b, unit) = (date_at(args, 0)?, date_at(args, 1)?, unit_arg_at(args, 2)?);
            num(diff(a, b, unit)?)
        }
        Func::BeginOfPeriod => QValue::Date(fold::begin_of_period(
            date_at(args, 0)?,
            unit_arg_at(args, 1)?,
        )),
        Func::EndOfPeriod => QValue::Date(end_of_period(date_at(args, 0)?, unit_arg_at(args, 1)?)),
        _ => {
            let d = date_at(args, 0)?;
            let p = d.parts();
            match f {
                Func::Year => num(i64::from(p.year)),
                Func::Quarter => num(i64::from((p.month - 1) / 3 + 1)),
                Func::Month => num(i64::from(p.month)),
                Func::DayOfYear => num(i64::from(d.day_of_year())),
                Func::Day => num(i64::from(p.day)),
                Func::Hour => num(i64::from(p.hour)),
                Func::Minute => num(i64::from(p.minute)),
                Func::Second => num(i64::from(p.second)),
                Func::WeekDay => num(i64::from(d.day_of_week())),
                Func::Week => {
                    let jan1 = i64::from(d.day_of_year()) - 1;
                    let jan1_dow = (i64::from(d.day_of_week()) - 1 - jan1).rem_euclid(7);
                    num((jan1 + jan1_dow) / 7 + 1)
                }
                _ => return Err(fatal("not a date function")),
            }
        }
    })
}

pub fn week_begin(d: BslDate, week_start: u32) -> BslDate {
    let day = 86_400 * TPS;
    let start = d.ticks() - d.ticks().rem_euclid(day);
    let back = (i64::from(d.day_of_week()) - i64::from(week_start)).rem_euclid(7);
    BslDate::from_ticks(start - back * day).unwrap_or(BslDate::EMPTY)
}

pub fn week_call(f: Func, args: &[QValue], week_start: u32) -> Option<Result<QValue, ExecError>> {
    let QValue::Date(d) = args.first()? else {
        return None;
    };
    match f {
        Func::BeginOfPeriod | Func::EndOfPeriod => {
            if unit_of(args.get(1)?)? != Kw::Week {
                return None;
            }
            let b = week_begin(*d, week_start);
            Some(Ok(QValue::Date(if f == Func::BeginOfPeriod {
                b
            } else {
                BslDate::from_ticks(b.ticks() + 7 * 86_400 * TPS - TPS).unwrap_or(BslDate::MAX)
            })))
        }
        Func::Week => {
            let jan1 = i64::from(d.day_of_year()) - 1;
            let jan1_dow = (i64::from(d.day_of_week()) - 1 - jan1).rem_euclid(7) + 1;
            let offset = (jan1_dow - i64::from(week_start)).rem_euclid(7);
            Some(Ok(num((jan1 + offset) / 7 + 1)))
        }
        _ => None,
    }
}
