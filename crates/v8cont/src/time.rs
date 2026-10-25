use serde::{Deserialize, Serialize};

const TICKS_PER_SEC: u64 = 10_000;
const UNIX_EPOCH_DAYS: i64 = 719_162;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct V8Time(pub u64);

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
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

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

impl V8Time {
    pub const PACK_DEFAULT: V8Time = V8Time(637_134_336_000_000);

    pub fn from_civil(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<V8Time> {
        if !(1..=9999).contains(&y)
            || !(1..=12).contains(&mo)
            || d == 0
            || h >= 24
            || mi >= 60
            || s >= 60
        {
            return None;
        }
        let dim = match mo {
            2 if is_leap(y) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if d > dim {
            return None;
        }
        let days = days_from_civil(i64::from(y), i64::from(mo), i64::from(d)) + UNIX_EPOCH_DAYS;
        let secs = u64::try_from(days).ok()? * 86_400
            + u64::from(h) * 3600
            + u64::from(mi) * 60
            + u64::from(s);
        secs.checked_mul(TICKS_PER_SEC).map(V8Time)
    }

    pub fn to_civil(self) -> (i32, u32, u32, u32, u32, u32, u32) {
        let sub = (self.0 % TICKS_PER_SEC) as u32;
        let secs = self.0 / TICKS_PER_SEC;
        let days = (secs / 86_400) as i64 - UNIX_EPOCH_DAYS;
        let rem = (secs % 86_400) as u32;
        let (y, m, d) = civil_from_days(days);
        let y = i32::try_from(y).unwrap_or(i32::MAX);
        (
            y,
            m as u32,
            d as u32,
            rem / 3600,
            rem % 3600 / 60,
            rem % 60,
            sub,
        )
    }

    pub fn now() -> V8Time {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let unix_ticks = d.as_secs() * TICKS_PER_SEC + u64::from(d.subsec_micros() / 100);
        V8Time(UNIX_EPOCH_DAYS as u64 * 86_400 * TICKS_PER_SEC + unix_ticks)
    }
}
