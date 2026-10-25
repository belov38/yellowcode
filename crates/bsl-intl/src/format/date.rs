use super::spec::FormatSpec;
use crate::locale::Locale;
use crate::pattern::render_date_pattern;
use crate::present::{date_to_string, glue_date_time, medium_time, short_date};
use bsl_prim::BslDate;

#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Lit(String),
    Field(char, usize),
}

fn tokenize(p: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut it = p.chars().peekable();
    while let Some(c) = it.next() {
        if c == '"' {
            for q in it.by_ref() {
                if q == '"' {
                    break;
                }
                lit.push(q);
            }
        } else if matches!(c, 'd' | 'M' | 'y' | 'h' | 'H' | 'm' | 's' | 't' | 'к') {
            let mut n = 1;
            while it.peek() == Some(&c) {
                it.next();
                n += 1;
            }
            if c == 't' && n < 2 {
                lit.push(c);
                continue;
            }
            if !lit.is_empty() {
                out.push(Tok::Lit(std::mem::take(&mut lit)));
            }
            out.push(Tok::Field(c, n));
        } else {
            lit.push(c);
        }
    }
    if !lit.is_empty() {
        out.push(Tok::Lit(lit));
    }
    out
}

fn num(out: &mut String, v: u32, width: usize) {
    out.push_str(&format!("{v:0width$}"));
}

fn render_df(pattern: &str, d: BslDate, loc: &Locale) -> String {
    let data = loc.data();
    let p = d.parts();
    let toks = tokenize(pattern);
    let with_day = toks
        .iter()
        .any(|t| matches!(t, Tok::Field('d', n) if *n <= 2));
    let mi = (p.month.clamp(1, 12) - 1) as usize;
    let wi = (d.day_of_week() % 7) as usize;
    let mut out = String::new();
    for t in &toks {
        match t {
            Tok::Lit(s) => out.push_str(s),
            Tok::Field(c, n) => match (c, *n) {
                ('d', 1 | 2) => num(&mut out, p.day, *n),
                ('d', 3) => out.push_str(&data.days[1][wi]),
                ('d', _) => out.push_str(&data.days[0][wi]),
                ('M', 1 | 2) => num(&mut out, p.month, *n),
                ('M', 3) => out.push_str(&data.months[if with_day { 1 } else { 3 }][mi]),
                ('M', _) => out.push_str(&data.months[if with_day { 0 } else { 2 }][mi]),
                ('y', 1 | 2) => num(&mut out, p.year % 100, 2),
                ('y', n) => num(&mut out, p.year, n.min(4)),
                ('h', n) => num(
                    &mut out,
                    if p.hour.is_multiple_of(12) {
                        12
                    } else {
                        p.hour % 12
                    },
                    n.min(2),
                ),
                ('H', n) => num(&mut out, p.hour, n.min(2)),
                ('m', n) => num(&mut out, p.minute, n.min(2)),
                ('s', n) => num(&mut out, p.second, n.min(2)),
                ('t', _) => out.push_str(&data.am_pm[usize::from(p.hour >= 12)]),
                _ => num(&mut out, (p.month.clamp(1, 12) - 1) / 3 + 1, 1),
            },
        }
    }
    out
}

fn render_dlf(code: &str, d: BslDate, loc: &Locale) -> Option<String> {
    let data = loc.data();
    let long = || render_date_pattern(&data.date_patterns[1], d, loc);
    Some(match code {
        "D" => short_date(d, loc),
        "DD" => long(),
        "T" => medium_time(d, loc),
        "DT" => date_to_string(d, loc),
        "DDT" => glue_date_time(&long(), &medium_time(d, loc), loc),
        _ => return None,
    })
}

pub(super) fn format_date(d: BslDate, spec: &FormatSpec, loc: &Locale) -> String {
    if d == BslDate::EMPTY {
        return spec.de.clone().unwrap_or_default();
    }
    if let Some(df) = spec.df.as_deref().filter(|s| !s.is_empty()) {
        return render_df(df, d, loc);
    }
    spec.dlf
        .as_deref()
        .and_then(|c| render_dlf(c.trim(), d, loc))
        .unwrap_or_else(|| date_to_string(d, loc))
}
