use crate::error::{LibError, msg};
use crate::locale::Locale;
use bsl_prim::Number;

const MAX_EXP: usize = 100_000;

fn bad() -> LibError {
    LibError::new(msg::CONVERT_2NUMERIC)
}

fn is_space(u: u16) -> bool {
    matches!(u, 0x20 | 0x09 | 0x0A | 0x0D | 0xA0)
}

fn starts_with(s: &[u16], i: usize, pat: &[u16]) -> bool {
    !pat.is_empty() && s.len() >= i + pat.len() && s[i..i + pat.len()] == *pat
}

pub fn parse_number(s: &[u16], loc: &Locale) -> Result<Number, LibError> {
    let start = s.iter().position(|&u| !is_space(u)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|&u| !is_space(u))
        .map_or(start, |p| p + 1);
    let s = &s[start..end];
    let group: Vec<u16> = loc.data().group.encode_utf16().collect();
    let decimal: Vec<u16> = loc.data().decimal.encode_utf16().collect();
    let mut i = 0;
    let mut negative = false;
    match s.first() {
        Some(&0x2B) => i = 1,
        Some(&0x2D) => {
            negative = true;
            i = 1;
        }
        _ => {}
    }
    let (mut int, mut frac) = (String::new(), String::new());
    let mut in_frac = false;
    let mut exp: Option<(bool, String)> = None;
    while i < s.len() {
        let u = s[i];
        if (0x30..=0x39).contains(&u) {
            let c = char::from(u as u8);
            if in_frac {
                frac.push(c)
            } else {
                int.push(c)
            }
            i += 1;
        } else if starts_with(s, i, &group) {
            i += group.len();
        } else if u == 0x2E || starts_with(s, i, &decimal) {
            if in_frac {
                return Err(bad());
            }
            in_frac = true;
            i += if u == 0x2E { 1 } else { decimal.len() };
        } else if u == 0x45 {
            let mut j = i + 1;
            let neg = s.get(j) == Some(&0x2D);
            j += usize::from(neg);
            let digits: Option<String> = s[j..]
                .iter()
                .map(|&d| (0x30..=0x39).contains(&d).then(|| char::from(d as u8)))
                .collect();
            match digits {
                Some(d) if !d.is_empty() => exp = Some((neg, d)),
                _ => return Err(bad()),
            }
            break;
        } else {
            return Err(bad());
        }
    }
    if int.is_empty() && frac.is_empty() {
        return Err(bad());
    }
    if let Some((neg, digits)) = exp {
        let n = digits.trim_start_matches('0');
        if n.len() > 6 || n.parse::<usize>().unwrap_or(0) > MAX_EXP {
            return Err(bad());
        }
        let n = n.parse::<usize>().unwrap_or(0);
        if neg {
            let split = int.len().saturating_sub(n);
            let moved = int.split_off(split);
            frac = format!("{}{}{}", "0".repeat(n - moved.len()), moved, frac);
        } else {
            let take = n.min(frac.len());
            let rest = frac.split_off(take);
            int.push_str(&frac);
            int.push_str(&"0".repeat(n - take));
            frac = rest;
        }
    }
    if int.is_empty() {
        int.push('0');
    }
    let text = format!("{}{int}.{frac}", if negative { "-" } else { "" });
    Number::parse(&text).ok_or_else(bad)
}
