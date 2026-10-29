use super::spec::FormatSpec;
use crate::locale::Locale;
use crate::present::{group_digits, int_tail};
use bsl_prim::Number;

fn shift(int: &mut Vec<u8>, frac: &mut Vec<u8>, shift: i32) {
    let k = shift.unsigned_abs() as usize;
    if shift > 0 {
        let mut moved = vec![b'0'; k.saturating_sub(int.len())];
        moved.extend(int.drain(int.len().saturating_sub(k)..));
        moved.append(frac);
        *frac = moved;
    } else if shift < 0 {
        let take = k.min(frac.len());
        int.extend(frac.drain(..take));
        int.resize(int.len() + (k - take), b'0');
    }
    let zeros = int.iter().take_while(|&&c| c == b'0').count();
    int.drain(..zeros);
    frac.truncate(frac.len() - frac.iter().rev().take_while(|&&c| c == b'0').count());
}

pub(crate) fn round_to(int: &mut Vec<u8>, frac: &mut Vec<u8>, d: usize) {
    if frac.len() > d {
        let up = frac[d] >= b'5';
        frac.truncate(d);
        if up && !increment(frac) && !increment(int) {
            int.insert(0, b'1');
        }
    }
    frac.resize(d, b'0');
}

fn increment(digits: &mut [u8]) -> bool {
    for c in digits.iter_mut().rev() {
        if *c == b'9' {
            *c = b'0';
        } else {
            *c += 1;
            return true;
        }
    }
    false
}

pub(crate) fn apply_template(num: &str, template: Option<&str>) -> String {
    let Some(t) = template else {
        return num.to_string();
    };
    let mut out = String::with_capacity(t.len());
    let mut quote = None;
    for c in t.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => out.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == 'Ч' || c == 'N' => out.push_str(num),
            None => out.push(c),
        }
    }
    out
}

pub(crate) fn format_number(n: &Number, spec: &FormatSpec, loc: &Locale) -> String {
    if n.is_zero() {
        match spec.nz.as_deref() {
            None => return String::new(),
            Some(t) if !t.is_empty() => return t.to_string(),
            Some(_) => {}
        }
    }
    let mut int = n.int_digits().into_bytes();
    let mut frac = n.frac_digits().into_bytes();
    shift(&mut int, &mut frac, spec.ns.unwrap_or(0));
    let width = spec.nd.filter(|&w| w > 0).map(|w| w as usize);
    let decimals = spec
        .nfd
        .map(|d| d as usize)
        .or(spec.nd.map(|_| 0))
        .map(|d| width.map_or(d, |w| d.min(w)));
    if let Some(d) = decimals {
        round_to(&mut int, &mut frac, d);
    }
    if decimals == Some(0)
        && width.is_none()
        && spec.nz.is_none()
        && spec.ns.unwrap_or(0) == 0
        && spec.nf.is_none()
        && spec.nn.is_none()
        && int.iter().chain(&frac).all(|&c| c == b'0')
    {
        return String::new();
    }
    if let Some(w) = width {
        let d = decimals.unwrap_or(0);
        let room = w - d;
        if int.len() > room {
            int = vec![b'9'; room];
            frac = vec![b'9'; d];
        } else if spec.nlz {
            int.splice(0..0, std::iter::repeat_n(b'0', room - int.len()));
        }
    }
    let negative = n.is_negative() && int.iter().chain(&frac).any(|&c| c != b'0');
    if int.is_empty() && !matches!((width, frac.is_empty()), (Some(w), false) if w == frac.len()) {
        int.push(b'0');
    }
    let data = loc.data();
    let (first, then) = match spec.ng {
        None => (3, Some(3)),
        Some((p, s)) => (p, s.map(|s| if s == 0 { p } else { s })),
    };
    let int = String::from_utf8(int).unwrap_or_default();
    let mut body = group_digits(
        int_tail(&int),
        first as usize,
        then.map(|s| s as usize),
        spec.ngs.as_deref().unwrap_or(&data.group),
    );
    if !frac.is_empty() {
        body.push_str(spec.nds.as_deref().unwrap_or(&data.decimal));
        body.push_str(&String::from_utf8(frac).unwrap_or_default());
    }
    let m = &data.minus;
    let signed = match (negative, spec.nn.unwrap_or(1)) {
        (false, _) => body,
        (true, 0) => format!("({body})"),
        (true, 2) => format!("{m} {body}"),
        (true, 3) => format!("{body}{m}"),
        (true, 4) => format!("{body} {m}"),
        (true, _) => format!("{m}{body}"),
    };
    apply_template(&signed, spec.nf.as_deref())
}
