use crate::number::{Number, RoundMode};

const TPS: i64 = 10_000;
const SECS_PER_DAY: i64 = 86_400;
const MAX_TICKS: i64 = 3_652_059 * SECS_PER_DAY * TPS - 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BslDate(i64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateParts {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub fn is_leap_year(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

pub fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 306
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 306;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

impl BslDate {
    pub const EMPTY: BslDate = BslDate(0);
    pub const MAX: BslDate = BslDate(MAX_TICKS);
    pub const TICKS_PER_SECOND: i64 = TPS;

    pub fn from_parts(y: u32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<BslDate> {
        if !(1..=9999).contains(&y) || !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo)
        {
            return None;
        }
        if h >= 24 || mi >= 60 || s >= 60 {
            return None;
        }
        let days = days_from_civil(i64::from(y), i64::from(mo), i64::from(d));
        let secs = days * SECS_PER_DAY + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
        Some(BslDate(secs * TPS))
    }

    pub fn from_ticks(t: i64) -> Option<BslDate> {
        (0..=MAX_TICKS).contains(&t).then_some(BslDate(t))
    }

    pub fn ticks(self) -> i64 {
        self.0
    }

    pub fn whole_seconds(self) -> i64 {
        self.0 / TPS
    }

    pub fn frac_ticks(self) -> u32 {
        (self.0 % TPS) as u32
    }

    fn days(self) -> i64 {
        self.whole_seconds() / SECS_PER_DAY
    }

    pub fn parts(self) -> DateParts {
        let secs = self.whole_seconds();
        let (y, m, d) = civil_from_days(secs / SECS_PER_DAY);
        let sod = secs % SECS_PER_DAY;
        DateParts {
            year: y as u32,
            month: m as u32,
            day: d as u32,
            hour: (sod / 3600) as u32,
            minute: (sod % 3600 / 60) as u32,
            second: (sod % 60) as u32,
        }
    }

    pub fn add_seconds(self, n: &Number) -> BslDate {
        let delta = n
            .mul(&Number::from_i64(TPS))
            .round(0, RoundMode::HalfAwayFromZero)
            .trunc_to_i64_saturating();
        BslDate(self.0.saturating_add(delta).clamp(0, MAX_TICKS))
    }

    pub fn sub_date(self, other: BslDate) -> Number {
        Number::from_scaled_i64(self.0 - other.0, 4)
    }

    pub fn trunc_seconds(self) -> BslDate {
        BslDate(self.0 - self.0 % TPS)
    }

    pub fn day_of_week(self) -> u32 {
        (self.days() % 7) as u32 + 1
    }

    pub fn day_of_year(self) -> u32 {
        let p = self.parts();
        let start = days_from_civil(i64::from(p.year), 1, 1);
        (self.days() - start) as u32 + 1
    }

    pub fn to_xml_string(self) -> String {
        let p = self.parts();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            p.year, p.month, p.day, p.hour, p.minute, p.second
        )
    }

    pub fn boost_hash(self) -> i32 {
        self.whole_seconds() as u32 as i32
    }
}
