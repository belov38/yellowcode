pub(crate) use super::scalar::py_space;
use crate::templates::context::Lookup;
use brace::{Guid, Node};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphError(pub String);

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GraphError {}

pub(crate) type R<T> = Result<T, GraphError>;

pub(crate) fn bad<T>(msg: impl Into<String>) -> R<T> {
    Err(GraphError(msg.into()))
}

pub(crate) const DECLARED: &[&str] = &["web", "win", "style", "pal"];

pub(crate) fn is(n: &Node, lit: &str) -> bool {
    n.text() == Some(lit)
}

pub(crate) fn list(n: &Node) -> R<&[Node]> {
    n.as_list()
        .ok_or_else(|| GraphError("scalar where a list is expected".into()))
}

pub(crate) fn at<T>(v: &[T], i: i64) -> R<&T> {
    let k = if i < 0 {
        i64::try_from(v.len()).ok().and_then(|n| n.checked_add(i))
    } else {
        Some(i)
    };
    k.and_then(|k| usize::try_from(k).ok())
        .and_then(|k| v.get(k))
        .ok_or_else(|| GraphError(format!("index {i} out of range")))
}

pub(crate) fn item(n: &Node, i: i64) -> R<&Node> {
    at(list(n)?, i)
}

pub(crate) fn slice<T>(v: &[T], start: i64, stop: i64) -> &[T] {
    let n = i64::try_from(v.len()).unwrap_or(i64::MAX);
    let norm = |x: i64| -> usize {
        let x = if x < 0 { x.saturating_add(n) } else { x };
        usize::try_from(x.clamp(0, n)).unwrap_or(0)
    };
    v.get(norm(start)..norm(stop)).unwrap_or_default()
}

pub(crate) fn count(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

#[derive(Clone, Copy)]
pub(crate) struct Names<'a> {
    lookup: &'a dyn Lookup,
}

impl<'a> Names<'a> {
    pub(crate) fn from_lookup(lookup: &'a dyn Lookup) -> Self {
        Self { lookup }
    }

    pub(crate) fn name(&self, u: &Node) -> Option<&'a str> {
        self.name_of(u.text()?)
    }

    pub(crate) fn name_of(&self, u: &str) -> Option<&'a str> {
        self.lookup.name(&Guid::parse(u)?)
    }
}

fn sci(x: f64, prec: usize) -> R<(String, i64)> {
    if !x.is_finite() {
        return bad(format!("not a finite number: {x}"));
    }
    let t = format!("{:.*e}", prec, x.abs());
    let (mant, exp) = t
        .split_once('e')
        .ok_or_else(|| GraphError(format!("number format {t}")))?;
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0".to_string(),
        d => d.to_string(),
    };
    let e = exp
        .parse()
        .map_err(|_| GraphError(format!("number format {t}")))?;
    Ok((digits, e))
}

fn split_digits(digits: &str, p: usize) -> (&str, &str) {
    (
        digits.get(..p).unwrap_or(digits),
        digits.get(p..).unwrap_or_default(),
    )
}

#[allow(dead_code)]
pub(crate) fn fmt_double(x: f64) -> R<String> {
    if x == 0.0 {
        return Ok("0".into());
    }
    let (digits, e) = sci(x, 15)?;
    let point = e.saturating_add(1);
    let n = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let out = if (1..=n).contains(&point) {
        let (a, b) = split_digits(&digits, count(point));
        if b.is_empty() {
            a.to_string()
        } else {
            format!("{a}.{b}")
        }
    } else {
        let (a, b) = split_digits(&digits, 1);
        if b.is_empty() {
            format!("{a}e{e}")
        } else {
            format!("{a}.{b}e{e}")
        }
    };
    Ok(if x < 0.0 { format!("-{out}") } else { out })
}

#[allow(dead_code)]
pub(crate) fn text_b(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
