use crate::error::{Error, Result};
use onecd::{DateTime, Numeric};

pub type Uuid16 = brace::Guid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct RefId(pub [u8; 16]);

impl RefId {
    pub const NIL: RefId = RefId([0; 16]);

    pub fn from_uuid(u: Uuid16) -> RefId {
        let b = u.as_bytes();
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            let src = match i {
                0..=7 => 8 + i,
                8..=9 => 6 + (i - 8),
                10..=11 => 4 + (i - 10),
                _ => i - 12,
            };
            *o = b.get(src).copied().unwrap_or(0);
        }
        RefId(out)
    }

    pub fn to_uuid(&self) -> Uuid16 {
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            let src = match i {
                0..=3 => 12 + i,
                4..=5 => 10 + (i - 4),
                6..=7 => 8 + (i - 6),
                _ => i - 8,
            };
            *o = self.0.get(src).copied().unwrap_or(0);
        }
        Uuid16::from_bytes(out)
    }

    pub fn is_nil(&self) -> bool {
        self.0 == [0; 16]
    }
}

pub mod type_code {
    pub const UNDEFINED: u8 = 1;
    pub const BOOL: u8 = 2;
    pub const NUMBER: u8 = 3;
    pub const DATE: u8 = 4;
    pub const STRING: u8 = 5;
    pub const UUID: u8 = 6;
    pub const REF: u8 = 8;
}

pub const TICKS_PER_SECOND: u64 = 10_000;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + 719_162
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days - 719_162 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

pub fn datetime_to_ticks(d: &DateTime) -> u64 {
    if d.year == 0 {
        return 0;
    }
    let days = days_from_civil(i64::from(d.year), i64::from(d.month), i64::from(d.day));
    let secs = days
        .saturating_mul(86_400)
        .saturating_add(i64::from(d.hour) * 3600 + i64::from(d.minute) * 60 + i64::from(d.second));
    u64::try_from(secs)
        .unwrap_or(0)
        .saturating_mul(TICKS_PER_SECOND)
}

pub fn datetime_from_ticks(t: u64) -> Result<DateTime> {
    if t == 0 {
        return Ok(DateTime::EMPTY);
    }
    if !t.is_multiple_of(TICKS_PER_SECOND) {
        return Err(Error::Unsupported(format!(
            "fractional second in .dt ticks {t}"
        )));
    }
    let out_of_range = || Error::Parse {
        what: "ticks",
        at: t.to_string(),
        why: "out of range".to_string(),
    };
    let secs = t / TICKS_PER_SECOND;
    let days = i64::try_from(secs / 86_400).map_err(|_| out_of_range())?;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let year = u16::try_from(y).map_err(|_| out_of_range())?;
    DateTime::new(
        year,
        u8::try_from(m).map_err(|_| out_of_range())?,
        u8::try_from(d).map_err(|_| out_of_range())?,
        u8::try_from(rem / 3600).map_err(|_| out_of_range())?,
        u8::try_from(rem % 3600 / 60).map_err(|_| out_of_range())?,
        u8::try_from(rem % 60).map_err(|_| out_of_range())?,
    )
    .ok_or_else(out_of_range)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnValue {
    Null,
    Bool(bool),
    Number(Numeric),
    Str(String),
    DateTime(DateTime),
    Bytes(Vec<u8>),
}

pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

pub fn numeric_fits(n: &Numeric, digits: u32, scale: u32) -> bool {
    let all = n.digits();
    let n_scale = n.scale() as usize;
    let int_len = all.len().saturating_sub(n_scale);
    let (int_part, frac_part) = all.split_at(int_len);
    let int_sig = int_part.iter().skip_while(|&&d| d == 0).count();
    let pad = n_scale.saturating_sub(frac_part.len());
    let trimmed = frac_part.iter().rposition(|&d| d != 0).map_or(0, |p| p + 1);
    let frac_sig = if trimmed == 0 { 0 } else { pad + trimmed };
    let int_max = digits.saturating_sub(scale) as usize;
    int_sig <= int_max && frac_sig <= scale as usize
}
