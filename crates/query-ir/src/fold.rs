use crate::value::{QStr, QValue};
use bsl_prim::{BslDate, Number};
use query_syntax::{Kw, ast::BinOp};

const MAX_DIGITS: usize = 38;
const TPS: i64 = BslDate::TICKS_PER_SECOND;
const DAY_TICKS: i64 = 86_400 * TPS;

#[derive(Clone, Debug, PartialEq)]
pub enum FoldError {
    TooLong(String),
    TooBigDate,
    BadDate { arg: Option<usize> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Folded {
    Value(QValue),
    Runtime,
}

pub fn number_literal(digits: &str) -> Option<Number> {
    Number::parse(digits)
}

pub fn canonical_digits(n: &Number) -> String {
    let int = n.int_digits();
    let mut s = int.trim_start_matches('0').to_string();
    s.push_str(n.frac_digits().trim_end_matches('0'));
    s
}

fn checked(n: Number) -> Result<Folded, FoldError> {
    let d = canonical_digits(&n);
    if d.len() > MAX_DIGITS {
        return Err(FoldError::TooLong(d));
    }
    Ok(Folded::Value(QValue::Number(n)))
}

pub const FOLD_MAX_UNITS: usize = 64 << 10;

pub fn arith(op: BinOp, l: &QValue, r: &QValue) -> Result<Folded, FoldError> {
    match (l, r) {
        (QValue::Number(a), QValue::Number(b)) => match op {
            BinOp::Add => checked(a.add(b)),
            BinOp::Sub => checked(a.sub(b)),
            BinOp::Mul => checked(a.mul(b)),
            BinOp::Div => match a.div(b) {
                Ok(q) => checked(q),
                Err(_) => Ok(Folded::Runtime),
            },
            BinOp::And | BinOp::Or => Ok(Folded::Runtime),
        },
        (QValue::String(a), QValue::String(b))
            if op == BinOp::Add && a.0.len() + b.0.len() > FOLD_MAX_UNITS =>
        {
            Ok(Folded::Runtime)
        }
        (QValue::String(a), QValue::String(b)) if op == BinOp::Add => {
            crate::probe::hit_n(a.0.len() + b.0.len() + 1);
            let v: Vec<u16> = a.0.iter().chain(b.0.iter()).copied().collect();
            Ok(Folded::Value(QValue::String(QStr(v.into()))))
        }
        _ => Ok(Folded::Runtime),
    }
}

pub fn neg(v: &QValue) -> Option<QValue> {
    match v {
        QValue::Number(n) => Some(QValue::Number(n.neg())),
        _ => None,
    }
}

fn limit() -> Option<BslDate> {
    BslDate::from_parts(3999, 12, 31, 23, 59, 59)
}

fn limited(ticks: i64) -> Result<BslDate, FoldError> {
    let max = limit().map(BslDate::ticks).unwrap_or(i64::MAX);
    match BslDate::from_ticks(ticks) {
        Some(d) if ticks <= max => Ok(d),
        _ => Err(FoldError::TooBigDate),
    }
}

pub fn datetime(args: &[i64]) -> Result<BslDate, FoldError> {
    let bad = FoldError::BadDate { arg: None };
    let (y, rest) = args.split_first().ok_or(bad.clone())?;
    if !(1..=9999).contains(y) || !matches!(args.len(), 3 | 6) {
        return Err(if (1..=9999).contains(y) {
            bad
        } else {
            FoldError::BadDate { arg: Some(0) }
        });
    }
    let mut f = [0u32; 5];
    for (slot, v) in f.iter_mut().zip(rest) {
        *slot = u32::try_from(*v).map_err(|_| bad.clone())?;
    }
    let year = u32::try_from(*y).map_err(|_| bad.clone())?;
    let [mo, d, h, mi, s] = f;
    let date = BslDate::from_parts(year, mo, d, h, mi, s).ok_or(bad)?;
    if year > 3999 {
        return Err(FoldError::TooBigDate);
    }
    Ok(date)
}

fn months_of(unit: Kw) -> Option<i64> {
    match unit {
        Kw::Month => Some(1),
        Kw::Quarter => Some(3),
        Kw::HalfYear => Some(6),
        Kw::Year => Some(12),
        _ => None,
    }
}

fn seconds_of(unit: Kw) -> Option<i64> {
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

pub fn date_add(d: BslDate, unit: Kw, n: &Number) -> Result<BslDate, FoldError> {
    let k = i128::from(n.trunc().trunc_to_i64_saturating());
    if let Some(m) = months_of(unit) {
        let p = d.parts();
        let total = i128::from(p.year) * 12 + i128::from(p.month) - 1 + k * i128::from(m);
        if total < 0 {
            return Ok(BslDate::EMPTY);
        }
        let (y, mo) = (total / 12, total % 12 + 1);
        if y < 1 {
            return Ok(BslDate::EMPTY);
        }
        if y > 3999 {
            return Err(FoldError::TooBigDate);
        }
        let (y, mo) = (y as u32, mo as u32);
        let day = p.day.min(bsl_prim::days_in_month(y, mo));
        let base = BslDate::from_parts(y, mo, day, p.hour, p.minute, p.second)
            .ok_or(FoldError::BadDate { arg: None })?;
        return limited(base.ticks() + i64::from(d.frac_ticks()));
    }
    let Some(secs) = seconds_of(unit) else {
        return Ok(d);
    };
    let ticks = i128::from(d.ticks()) + k * i128::from(secs) * i128::from(TPS);
    if ticks < 0 {
        return Ok(BslDate::EMPTY);
    }
    let ticks = i64::try_from(ticks).map_err(|_| FoldError::TooBigDate)?;
    limited(ticks)
}

fn date_of(y: u32, mo: u32, d: u32) -> BslDate {
    BslDate::from_parts(y, mo, d, 0, 0, 0).unwrap_or(BslDate::EMPTY)
}

pub fn begin_of_period(d: BslDate, unit: Kw) -> BslDate {
    let p = d.parts();
    let day_start = d.ticks() - d.ticks() % DAY_TICKS;
    let from_ticks = |t: i64| BslDate::from_ticks(t).unwrap_or(BslDate::EMPTY);
    match unit {
        Kw::Year => date_of(p.year, 1, 1),
        Kw::HalfYear => date_of(p.year, if p.month <= 6 { 1 } else { 7 }, 1),
        Kw::Quarter => date_of(p.year, (p.month - 1) / 3 * 3 + 1, 1),
        Kw::Month => date_of(p.year, p.month, 1),
        Kw::TenDays => date_of(p.year, p.month, ((p.day - 1) / 10).min(2) * 10 + 1),
        Kw::Week => from_ticks(day_start - i64::from(d.day_of_week() - 1) * DAY_TICKS),
        Kw::Day => from_ticks(day_start),
        Kw::Hour => from_ticks(d.ticks() - d.ticks() % (3600 * TPS)),
        Kw::Minute => from_ticks(d.ticks() - d.ticks() % (60 * TPS)),
        _ => d.trunc_seconds(),
    }
}

pub fn end_of_period(d: BslDate, unit: Kw) -> Result<BslDate, FoldError> {
    let b = begin_of_period(d, unit);
    let p = b.parts();
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
                Some(b.ticks() + i64::from((p.day + 10).min(dim + 1) - p.day) * DAY_TICKS)
            }
        }
        Kw::Week => Some(b.ticks() + 7 * DAY_TICKS),
        Kw::Day => Some(b.ticks() + DAY_TICKS),
        Kw::Hour => Some(b.ticks() + 3600 * TPS),
        Kw::Minute => Some(b.ticks() + 60 * TPS),
        _ => return Ok(d.trunc_seconds()),
    };
    match next_start {
        Some(t) => limited(t - TPS),
        None => Err(FoldError::TooBigDate),
    }
}
