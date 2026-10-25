use bsl_prim::{BslDate, Number, days_in_month};

use crate::error::{LibError, msg};

const SECS_PER_DAY: i64 = 86_400;
const TPS: i64 = BslDate::TICKS_PER_SECOND;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

fn at_secs(secs: i64) -> BslDate {
    BslDate::from_ticks(secs.saturating_mul(TPS)).unwrap_or(if secs < 0 {
        BslDate::EMPTY
    } else {
        BslDate::MAX
    })
}

fn month_start(y: u32, m: u32) -> BslDate {
    BslDate::from_parts(y, m, 1, 0, 0, 0).unwrap_or(BslDate::EMPTY)
}

pub fn begin_of(d: BslDate, p: Period) -> BslDate {
    let secs = d.whole_seconds();
    let parts = d.parts();
    match p {
        Period::Minute => at_secs(secs - secs % 60),
        Period::Hour => at_secs(secs - secs % 3600),
        Period::Day => at_secs(secs - secs % SECS_PER_DAY),
        Period::Week => {
            let day = secs - secs % SECS_PER_DAY;
            at_secs(day - i64::from(d.day_of_week() - 1) * SECS_PER_DAY)
        }
        Period::Month => month_start(parts.year, parts.month),
        Period::Quarter => month_start(parts.year, (parts.month - 1) / 3 * 3 + 1),
        Period::Year => month_start(parts.year, 1),
    }
}

pub fn end_of(d: BslDate, p: Period) -> BslDate {
    let start = begin_of(d, p);
    let parts = start.parts();
    let next_secs = match p {
        Period::Minute => Some(start.whole_seconds() + 60),
        Period::Hour => Some(start.whole_seconds() + 3600),
        Period::Day => Some(start.whole_seconds() + SECS_PER_DAY),
        Period::Week => Some(start.whole_seconds() + 7 * SECS_PER_DAY),
        Period::Month | Period::Quarter | Period::Year => {
            let step = match p {
                Period::Month => 1,
                Period::Quarter => 3,
                _ => 12,
            };
            let idx = parts.year * 12 + parts.month - 1 + step;
            BslDate::from_parts(idx / 12, idx % 12 + 1, 1, 0, 0, 0).map(BslDate::whole_seconds)
        }
    };
    let last = BslDate::MAX.trunc_seconds();
    match next_secs {
        Some(n) if n - 1 < last.whole_seconds() => at_secs(n - 1),
        _ => last,
    }
}

pub fn add_months(d: BslDate, months: &Number) -> BslDate {
    let n = months.trunc_to_i64_saturating();
    let p = d.parts();
    let idx = i64::from(p.year) * 12 + i64::from(p.month) - 1;
    let target = idx.saturating_add(n);
    if target < 12 {
        return d;
    }
    if target >= 10_000 * 12 {
        return BslDate::EMPTY;
    }
    let (y, m) = ((target / 12) as u32, (target % 12) as u32 + 1);
    let day = p.day.min(days_in_month(y, m));
    BslDate::from_parts(y, m, day, p.hour, p.minute, p.second).unwrap_or(BslDate::EMPTY)
}

pub fn week_of_year(d: BslDate) -> u32 {
    let jan1 = month_start(d.parts().year, 1);
    (d.day_of_year() - 1 + jan1.day_of_week() - 1) / 7 + 1
}

fn bad_date() -> LibError {
    LibError::new(msg::CONVERT_2DATE)
}

pub fn date_from_numbers(
    y: &Number,
    mo: &Number,
    d: &Number,
    h: &Number,
    mi: &Number,
    s: &Number,
) -> Result<BslDate, LibError> {
    let part = |n: &Number| u32::try_from(n.trunc_to_i64_saturating()).map_err(|_| bad_date());
    BslDate::from_parts(part(y)?, part(mo)?, part(d)?, part(h)?, part(mi)?, part(s)?)
        .ok_or_else(bad_date)
}

pub fn date_from_digits(s: &[u16]) -> Result<BslDate, LibError> {
    if !matches!(s.len(), 8 | 12 | 14) || !s.iter().all(|u| (0x30..=0x39).contains(u)) {
        return Err(bad_date());
    }
    let mut v = [0u32; 14];
    for (i, u) in s.iter().enumerate() {
        v[i] = u32::from(*u) - 0x30;
    }
    let num = |a: usize, b: usize| v[a..b].iter().fold(0, |acc, x| acc * 10 + x);
    let (y, mo, d) = (num(0, 4), num(4, 6), num(6, 8));
    let (h, mi, sec) = (num(8, 10), num(10, 12), num(12, 14));
    if y == 0 && mo == 0 && d == 0 && h == 0 && mi == 0 && sec == 0 {
        return Ok(BslDate::EMPTY);
    }
    BslDate::from_parts(y, mo, d, h, mi, sec).ok_or_else(bad_date)
}

pub fn universal_ms(d: BslDate) -> Number {
    Number::from_i64(d.whole_seconds() * 1000 + i64::from(d.frac_ticks()) / 10)
}
