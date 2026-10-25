use crate::locale::Locale;
use bsl_prim::BslDate;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Lit(String),
    Field { letter: char, count: usize },
}

pub fn parse_pattern(pattern: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut it = pattern.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\'' {
            if it.peek() == Some(&'\'') {
                it.next();
                lit.push('\'');
                continue;
            }
            while let Some(q) = it.next() {
                if q == '\'' {
                    if it.peek() == Some(&'\'') {
                        it.next();
                        lit.push('\'');
                        continue;
                    }
                    break;
                }
                lit.push(q);
            }
        } else if c.is_ascii_alphabetic() {
            let mut count = 1;
            while it.peek() == Some(&c) {
                it.next();
                count += 1;
            }
            if !lit.is_empty() {
                parts.push(Part::Lit(std::mem::take(&mut lit)));
            }
            parts.push(Part::Field { letter: c, count });
        } else {
            lit.push(c);
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    parts
}

pub fn render_date_pattern(pattern: &str, d: BslDate, loc: &Locale) -> String {
    render_parts(&parse_pattern(pattern), d, loc)
}

pub fn render_parts(parts: &[Part], d: BslDate, loc: &Locale) -> String {
    let mut out = String::new();
    for p in parts {
        match p {
            Part::Lit(s) => out.push_str(s),
            Part::Field { letter, count } => field(&mut out, *letter, *count, d, loc),
        }
    }
    out
}

fn num(out: &mut String, v: u32, width: usize) {
    out.push_str(&format!("{v:0width$}"));
}

fn first_char(s: &str) -> &str {
    s.chars().next().map_or("", |c| &s[..c.len_utf8()])
}

fn field(out: &mut String, letter: char, count: usize, d: BslDate, loc: &Locale) {
    let data = loc.data();
    let p = d.parts();
    match letter {
        'G' => out.push_str("AD"),
        'y' | 'u' | 'Y' => {
            if count == 2 {
                num(out, p.year % 100, 2);
            } else {
                num(out, p.year, count);
            }
        }
        'M' | 'L' => {
            let stand = letter == 'L';
            let i = usize::try_from(p.month).unwrap_or(1).clamp(1, 12) - 1;
            match count {
                1 | 2 => num(out, p.month, count),
                3 => out.push_str(&data.months[if stand { 3 } else { 1 }][i]),
                4 => out.push_str(&data.months[if stand { 2 } else { 0 }][i]),
                _ => out.push_str(first_char(&data.months[if stand { 2 } else { 0 }][i])),
            }
        }
        'd' => num(out, p.day, count),
        'D' => num(out, d.day_of_year(), count),
        'E' | 'e' | 'c' => {
            let i = (d.day_of_week() % 7) as usize;
            let stand = letter == 'c';
            if letter != 'E' && count <= 2 {
                num(out, i as u32 + 1, count);
                return;
            }
            match count {
                1..=3 => out.push_str(&data.days[if stand { 3 } else { 1 }][i]),
                4 => out.push_str(&data.days[if stand { 2 } else { 0 }][i]),
                _ => out.push_str(first_char(&data.days[if stand { 2 } else { 0 }][i])),
            }
        }
        'a' => out.push_str(&data.am_pm[usize::from(p.hour >= 12)]),
        'H' => num(out, p.hour, count),
        'h' => num(
            out,
            if p.hour.is_multiple_of(12) {
                12
            } else {
                p.hour % 12
            },
            count,
        ),
        'k' => num(out, if p.hour == 0 { 24 } else { p.hour }, count),
        'K' => num(out, p.hour % 12, count),
        'm' => num(out, p.minute, count),
        's' => num(out, p.second, count),
        'S' => {
            let digits = format!("{:04}", d.frac_ticks());
            out.extend(digits.chars().chain(std::iter::repeat('0')).take(count));
        }
        'q' | 'Q' => {
            let q = (p.month.clamp(1, 12) - 1) / 3 + 1;
            if count <= 2 {
                num(out, q, count);
            } else {
                out.push_str(&format!("Q{q}"));
            }
        }
        _ => out.extend(std::iter::repeat_n(letter, count)),
    }
}
