use crate::calendar::{Period, begin_of, end_of};
use crate::error::{LibError, msg};
use crate::format::{FALLBACK_LANGUAGE, words};
use crate::locale::{Locale, locale};
use crate::locale_key::{KeyCase, language_of_key, locale_key};
use crate::pattern::render_date_pattern;
use crate::present::{date_to_string, short_date};
use bsl_prim::BslDate;

const DAY: usize = 0;
const YEAR: usize = 1;
const MONTH: usize = 2;
const QUARTER: usize = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum PeriodError {
    Unobserved,
    Platform(LibError),
}

fn is_whole(begin: BslDate, end: BslDate, p: Period) -> bool {
    begin_of(begin, p) == begin && end_of(begin, p) == end
}

fn clock(d: BslDate) -> (u32, u32, u32) {
    let p = d.parts();
    (p.hour, p.minute, p.second)
}

fn bound(d: BslDate, bare: bool, loc: &Locale) -> String {
    if bare {
        short_date(d, loc)
    } else {
        date_to_string(d, loc)
    }
}

pub fn period_presentation(
    begin: BslDate,
    end: BslDate,
    fmt: Option<&[u16]>,
    session: &Locale,
) -> Result<String, PeriodError> {
    if begin == BslDate::EMPTY && end == BslDate::EMPTY {
        return Ok(String::new());
    }
    let key = fmt.and_then(|fmt| locale_key(fmt, KeyCase::AnyCase));
    let loc = key.as_deref().and_then(locale).unwrap_or(session);
    let language = language_of_key(key.as_deref(), session);
    let name = loc.name();
    if begin != BslDate::EMPTY && end != BslDate::EMPTY && end < begin {
        let suffix = format!(
            " ({} < {})",
            date_to_string(end, loc),
            date_to_string(begin, loc)
        );
        return Err(if name == "ru_RU" {
            PeriodError::Platform(
                LibError::new(msg::PARAM_VALUE)
                    .with_arg(2)
                    .with_suffix(suffix),
            )
        } else {
            PeriodError::Unobserved
        });
    }
    if begin == BslDate::EMPTY
        && [Period::Day, Period::Month, Period::Quarter, Period::Year]
            .into_iter()
            .any(|p| is_whole(begin, end, p))
    {
        return Err(PeriodError::Unobserved);
    }
    let all = &words().period;
    let patterns = all
        .get(&language)
        .or_else(|| all.get(FALLBACK_LANGUAGE))
        .filter(|p| p.len() > QUARTER);
    let pick = |i: usize| patterns.map(|p| render_date_pattern(&p[i], begin, loc));
    let whole = |p| is_whole(begin, end, p);
    let found = if whole(Period::Day) {
        pick(DAY)
    } else if whole(Period::Month) {
        pick(MONTH)
    } else if whole(Period::Quarter) {
        pick(QUARTER)
    } else if whole(Period::Year) {
        pick(YEAR)
    } else {
        None
    };
    if let Some(text) = found {
        return Ok(text);
    }
    if let Some(p) = patterns.filter(|_| {
        begin != BslDate::EMPTY
            && begin_of(begin, Period::Month) == begin
            && end_of(end, Period::Month) == end
            && begin <= end
    }) {
        return Ok(format!(
            "{} - {}",
            render_date_pattern(&p[MONTH], begin, loc),
            render_date_pattern(&p[MONTH], end, loc)
        ));
    }
    let closed = |d: BslDate, from: bool| {
        let (year, month, edge) = if from {
            (
                begin_of(d, Period::Year) == d,
                begin_of(d, Period::Month) == d,
                clock(d) == (0, 0, 0),
            )
        } else {
            (
                end_of(d, Period::Year) == d,
                end_of(d, Period::Month) == d,
                clock(d) == (23, 59, 59),
            )
        };
        let ru = name == "ru_RU" && language == "ru";
        if year {
            patterns
                .filter(|_| ru)
                .map(|p| render_date_pattern(&p[YEAR], d, loc))
        } else if month {
            None
        } else if edge {
            patterns
                .filter(|_| {
                    matches!(
                        (name, language.as_str()),
                        ("ru_RU", "ru") | ("en_US", "en") | ("de_DE", "de")
                    )
                })
                .map(|p| render_date_pattern(&p[DAY], d, loc))
        } else {
            ru.then(|| date_to_string(d, loc))
        }
    };
    if begin == BslDate::EMPTY {
        return closed(end, false)
            .map(|text| format!("... - {text}"))
            .ok_or(PeriodError::Unobserved);
    }
    if end == BslDate::EMPTY {
        return closed(begin, true)
            .map(|text| format!("{text} - ..."))
            .ok_or(PeriodError::Unobserved);
    }
    let bare = clock(begin) == (0, 0, 0) && clock(end) == (23, 59, 59);
    Ok(format!(
        "{} - {}",
        bound(begin, bare, loc),
        bound(end, bare, loc)
    ))
}
