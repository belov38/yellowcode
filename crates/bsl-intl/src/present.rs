use crate::locale::Locale;
use crate::pattern::render_date_pattern;
use bsl_prim::{BslDate, Number};

pub(crate) const MAX_INT_DIGITS: usize = 309;

pub(crate) fn int_tail(int: &str) -> &str {
    &int[int.len().saturating_sub(MAX_INT_DIGITS)..]
}

pub(crate) fn group_digits(int: &str, first: usize, then: Option<usize>, sep: &str) -> String {
    if first == 0 || int.len() <= first || !int.is_ascii() {
        return int.to_string();
    }
    let mut cuts = vec![int.len() - first];
    if let Some(size) = then.filter(|&s| s > 0) {
        let mut end = int.len() - first;
        while end > size {
            end -= size;
            cuts.push(end);
        }
    }
    let mut out = String::with_capacity(int.len() + cuts.len() * sep.len());
    let mut start = 0;
    for &cut in cuts.iter().rev() {
        out.push_str(&int[start..cut]);
        out.push_str(sep);
        start = cut;
    }
    out.push_str(&int[start..]);
    out
}

pub fn number_to_string(n: &Number, loc: &Locale) -> String {
    let d = loc.data();
    let int = n.int_digits();
    let grouped = group_digits(int_tail(&int), 3, Some(3), &d.group);
    let mut out = String::new();
    if n.is_negative() && !n.is_zero() {
        out.push_str(&d.minus);
    }
    out.push_str(&grouped);
    let frac = n.frac_digits();
    if !frac.is_empty() {
        out.push_str(&d.decimal);
        out.push_str(&frac);
    }
    out
}

fn full_year(pattern: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut prev_y = false;
    for c in pattern.chars() {
        if c == '\'' {
            quoted = !quoted;
        }
        if c == 'y' && !quoted {
            if !prev_y {
                out.push_str("yyyy");
            }
            prev_y = true;
        } else {
            prev_y = false;
            out.push(c);
        }
    }
    out
}

pub(crate) fn short_date(d: BslDate, loc: &Locale) -> String {
    render_date_pattern(&full_year(&loc.data().date_patterns[3]), d, loc)
}

pub(crate) fn medium_time(d: BslDate, loc: &Locale) -> String {
    render_date_pattern(&loc.data().time_patterns[2], d, loc)
}

pub(crate) fn glue_date_time(date: &str, time: &str, loc: &Locale) -> String {
    loc.data()
        .date_time
        .replace("{1}", date)
        .replace("{0}", time)
}

pub fn date_to_string(d: BslDate, loc: &Locale) -> String {
    glue_date_time(&short_date(d, loc), &medium_time(d, loc), loc)
}

pub fn bool_to_string(b: bool) -> &'static str {
    if b { "Да" } else { "Нет" }
}
