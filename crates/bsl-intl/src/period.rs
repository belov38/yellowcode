use crate::calendar::{Period, begin_of, end_of};
use crate::format::{FALLBACK_LANGUAGE, words};
use crate::locale::{Locale, locale};
use crate::pattern::render_date_pattern;
use crate::present::{date_to_string, short_date};
use crate::words::{language_of, locale_key};
use bsl_prim::BslDate;

const DAY: usize = 0;
const YEAR: usize = 1;
const MONTH: usize = 2;
const QUARTER: usize = 3;

fn is_whole(begin: BslDate, end: BslDate, p: Period) -> bool {
    begin_of(begin, p) == begin && end_of(begin, p) == end
}

fn bound(d: BslDate, day_edge: (u32, u32, u32), loc: &Locale) -> String {
    let p = d.parts();
    if (p.hour, p.minute, p.second) == day_edge {
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
) -> String {
    if begin == BslDate::EMPTY && end == BslDate::EMPTY {
        return String::new();
    }
    let key = fmt.and_then(locale_key);
    let loc = key.as_deref().and_then(locale).unwrap_or(session);
    let language = language_of(key.as_deref(), session);
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
    if let Some(s) = found {
        return s;
    }
    if let Some(p) = patterns.filter(|_| {
        begin != BslDate::EMPTY
            && begin_of(begin, Period::Month) == begin
            && end_of(end, Period::Month) == end
            && begin <= end
    }) {
        return format!(
            "{} - {}",
            render_date_pattern(&p[MONTH], begin, loc),
            render_date_pattern(&p[MONTH], end, loc)
        );
    }
    format!(
        "{} - {}",
        bound(begin, (0, 0, 0), loc),
        bound(end, (23, 59, 59), loc)
    )
}
