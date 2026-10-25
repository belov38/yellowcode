use crate::error::{LibError, msg};
use crate::xml::{XmlDate, xml_date};
use bsl_prim::BslDate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateFormat {
    Iso,
    Microsoft,
    JavaScript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateVariant {
    Local,
    LocalWithOffset,
    Universal,
}

pub fn write_date(
    local: BslDate,
    offset: i32,
    f: DateFormat,
    v: DateVariant,
) -> Result<String, LibError> {
    match f {
        DateFormat::Iso => Ok(match v {
            DateVariant::Local => local.to_xml_string(),
            DateVariant::LocalWithOffset => {
                format!("{}{}", local.to_xml_string(), offset_suffix(offset))
            }
            DateVariant::Universal => format!("{}Z", utc_date(local, offset).to_xml_string()),
        }),
        DateFormat::Microsoft | DateFormat::JavaScript => {
            if v != DateVariant::Universal {
                let key = if f == DateFormat::Microsoft {
                    msg::DATE_MS
                } else {
                    msg::DATE_JS
                };
                return Err(LibError::new(key));
            }
            let ms = unix_millis(local, offset);
            Ok(if f == DateFormat::Microsoft {
                format!("/Date({ms})/")
            } else {
                format!("new Date({ms})")
            })
        }
    }
}

fn utc_date(local: BslDate, offset: i32) -> BslDate {
    let t = local.ticks() - i64::from(offset) * BslDate::TICKS_PER_SECOND;
    let t = t.clamp(BslDate::EMPTY.ticks(), BslDate::MAX.ticks());
    BslDate::from_ticks(t).unwrap_or(BslDate::EMPTY)
}

fn offset_suffix(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let a = i64::from(offset).abs();
    format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

fn unix_millis(local: BslDate, offset: i32) -> i64 {
    let epoch = BslDate::from_parts(1970, 1, 1, 0, 0, 0).map_or(0, BslDate::ticks);
    let t = local.ticks() - i64::from(offset) * BslDate::TICKS_PER_SECOND - epoch;
    t.div_euclid(10)
}

pub fn read_date(s: &[u16], f: DateFormat) -> Result<XmlDate, LibError> {
    let bad = || {
        LibError::new(msg::DATE_FORMAT).with_suffix(format!(" ('{}')", String::from_utf16_lossy(s)))
    };
    match f {
        DateFormat::Iso => xml_date(s).ok_or_else(bad),
        DateFormat::Microsoft => ms_date(s, "/Date(", ")/").ok_or_else(bad),
        DateFormat::JavaScript => ms_date(s, "new Date(", ")").ok_or_else(bad),
    }
}

fn ms_date(s: &[u16], prefix: &str, suffix: &str) -> Option<XmlDate> {
    let text = String::from_utf16(s).ok()?;
    let digits = text.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let body = digits.strip_prefix('-').unwrap_or(digits);
    if body.is_empty() || !body.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ms: i64 = digits.parse().ok()?;
    let epoch = BslDate::from_parts(1970, 1, 1, 0, 0, 0)?.ticks();
    let ticks = ms
        .checked_mul(BslDate::TICKS_PER_SECOND / 1000)?
        .checked_add(epoch)?;
    Some(XmlDate {
        date: BslDate::from_ticks(ticks)?,
        offset: Some(0),
    })
}
