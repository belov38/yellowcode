use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;

use brace::Node;

use super::query::py_strip;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Raised;

pub(crate) type Py<T> = std::result::Result<T, Raised>;

pub(crate) const LIST_REPR: &str = "[…]";

pub(crate) fn py_str(n: &Node) -> Cow<'_, str> {
    Cow::Borrowed(n.text().unwrap_or(LIST_REPR))
}

pub(crate) fn py_len(n: &Node) -> usize {
    match n {
        Node::List(l) => l.len(),
        Node::Str(s) | Node::Atom(s) => s.chars().count(),
    }
}

pub(crate) fn str_at(n: &Node, i: usize) -> Py<Cow<'_, str>> {
    match n {
        Node::List(l) => l.get(i).map(py_str).ok_or(Raised),
        Node::Str(s) | Node::Atom(s) => s
            .chars()
            .nth(i)
            .map(|c| Cow::Owned(c.to_string()))
            .ok_or(Raised),
    }
}

pub(crate) fn py_int(s: &str) -> Py<i64> {
    let t = py_strip(s);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return Err(Raised);
    }
    let mut v: i64 = 0;
    for d in digits.chars().filter_map(|c| c.to_digit(10)) {
        v = v.saturating_mul(10).saturating_add(i64::from(d));
    }
    Ok(if neg { v.saturating_neg() } else { v })
}

pub(crate) fn py_index(v: &[Node], i: i64) -> Py<&Node> {
    let k = if i < 0 {
        i64::try_from(v.len())
            .ok()
            .and_then(|len| len.checked_add(i))
    } else {
        Some(i)
    };
    k.and_then(|k| usize::try_from(k).ok())
        .and_then(|k| v.get(k))
        .ok_or(Raised)
}

pub(crate) fn py_item(n: &Node, i: i64) -> Py<Cow<'_, Node>> {
    match n {
        Node::List(l) => py_index(l, i).map(Cow::Borrowed),
        Node::Str(s) | Node::Atom(s) => {
            let k = if i < 0 {
                i64::try_from(s.chars().count())
                    .ok()
                    .and_then(|len| len.checked_add(i))
            } else {
                Some(i)
            };
            k.and_then(|k| usize::try_from(k).ok())
                .and_then(|k| s.chars().nth(k))
                .map(|c| Cow::Owned(Node::atom(c.to_string())))
                .ok_or(Raised)
        }
    }
}

pub(crate) fn py_seq(n: &Node) -> Cow<'_, [Node]> {
    match n {
        Node::List(l) => Cow::Borrowed(l),
        Node::Str(s) | Node::Atom(s) => {
            Cow::Owned(s.chars().map(|c| Node::atom(c.to_string())).collect())
        }
    }
}

pub(crate) fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn py_slice(v: &[Node], start: i64, end: Option<i64>) -> &[Node] {
    let len = i64::try_from(v.len()).unwrap_or(i64::MAX);
    let norm = |x: i64| {
        let x = if x < 0 {
            x.saturating_add(len).max(0)
        } else {
            x.min(len)
        };
        usize::try_from(x).unwrap_or(0)
    };
    let (s, e) = (norm(start), end.map_or(v.len(), norm));
    if s >= e {
        return &[];
    }
    v.get(s..e).unwrap_or_default()
}

fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn b64_value(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

pub(crate) fn b64_decode(s: &str) -> Py<Vec<u8>> {
    b64_decode_raw(s.strip_prefix("#base64:").unwrap_or(s))
}

pub(crate) fn b64_decode_raw(s: &str) -> Py<Vec<u8>> {
    let s: String = s.chars().filter(|c| !py_isspace(*c)).collect();
    if !s.is_ascii() {
        return Err(Raised);
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let (mut quad, mut left, mut pads) = (0u8, 0u8, 0u8);
    for c in s.bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(v) = b64_value(c) else {
            continue;
        };
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push((left << 2) | (v >> 4));
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (v >> 2));
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | v);
                left = 0;
            }
        }
    }
    if quad == 0 { Ok(out) } else { Err(Raised) }
}

pub(crate) fn tenth_g(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let len = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let mut exp = len.saturating_sub(2);
    let mut sig: Vec<u8> = digits.bytes().map(|b| b - b'0').collect();
    if sig.len() > 6 {
        let tail = sig.split_off(6);
        let up = match tail.split_first() {
            Some((&first, rest)) if first > 5 || (first == 5 && rest.iter().any(|&d| d > 0)) => {
                true
            }
            Some((&5, _)) => sig.last().is_some_and(|d| d % 2 == 1),
            _ => false,
        };
        if up {
            let mut i = sig.len();
            loop {
                let Some(k) = i.checked_sub(1) else {
                    sig.insert(0, 1);
                    sig.truncate(6);
                    exp = exp.saturating_add(1);
                    break;
                };
                i = k;
                match sig.get_mut(k) {
                    Some(d) if *d == 9 => *d = 0,
                    Some(d) => {
                        *d += 1;
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    while sig.len() > 1 && sig.last() == Some(&0) {
        sig.pop();
    }
    let s: String = sig.iter().map(|d| char::from(b'0' + d)).collect();
    let sign = if n < 0 { "-" } else { "" };
    if (-4..6).contains(&exp) {
        let int_len = usize::try_from(exp.saturating_add(1)).unwrap_or(0);
        let body = if exp < 0 {
            let zeros = usize::try_from(exp.saturating_neg().saturating_sub(1)).unwrap_or(0);
            format!("0.{}{s}", "0".repeat(zeros))
        } else if s.len() <= int_len {
            format!("{s}{}", "0".repeat(int_len - s.len()))
        } else {
            let (a, b) = s.split_at(int_len);
            format!("{a}.{b}")
        };
        return format!("{sign}{body}");
    }
    let (first, rest) = s.split_at(1);
    let mant = if rest.is_empty() {
        first.to_string()
    } else {
        format!("{first}.{rest}")
    };
    let esign = if exp < 0 { '-' } else { '+' };
    format!("{sign}{mant}e{esign}{:02}", exp.unsigned_abs())
}

#[derive(Debug, Clone)]
pub(crate) struct Dict<K, V> {
    keys: Vec<K>,
    map: HashMap<K, V>,
}

impl<K, V> Default for Dict<K, V> {
    fn default() -> Self {
        Dict {
            keys: Vec::new(),
            map: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq + Clone, V> Dict<K, V> {
    pub(crate) fn insert(&mut self, k: K, v: V) {
        if !self.map.contains_key(&k) {
            self.keys.push(k.clone());
        }
        self.map.insert(k, v);
    }

    pub(crate) fn get<Q>(&self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(k)
    }

    pub(crate) fn contains_key<Q>(&self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(k)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.keys.iter()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.keys
            .iter()
            .filter_map(|k| self.map.get(k).map(|v| (k, v)))
    }
}
