use crate::error::{LibError, msg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    FromStart,
    FromEnd,
}

fn param_err(n: u32) -> LibError {
    LibError::new(msg::PARAM_VALUE).with_arg(n)
}

fn len_i64(s: &[u16]) -> i64 {
    i64::try_from(s.len()).unwrap_or(i64::MAX)
}

pub fn left(s: &[u16], n: i64) -> &[u16] {
    if n <= 0 {
        return &s[..0];
    }
    let n = usize::try_from(n).unwrap_or(usize::MAX).min(s.len());
    &s[..n]
}

pub fn right(s: &[u16], n: i64) -> &[u16] {
    if n <= 0 {
        return &s[s.len()..];
    }
    let n = usize::try_from(n).unwrap_or(usize::MAX).min(s.len());
    &s[s.len() - n..]
}

pub fn mid(s: &[u16], start: i64, len: Option<i64>) -> &[u16] {
    let start = start.max(1);
    if start > len_i64(s) {
        return &s[..0];
    }
    let from = usize::try_from(start - 1)
        .unwrap_or(usize::MAX)
        .min(s.len());
    let rest = &s[from..];
    match len {
        Some(l) if l >= 0 => {
            let l = usize::try_from(l).unwrap_or(usize::MAX).min(rest.len());
            &rest[..l]
        }
        _ => rest,
    }
}

fn failure(m: usize, at: impl Fn(usize) -> u16) -> Vec<usize> {
    let mut fail = vec![0usize; m];
    let mut k = 0;
    for i in 1..m {
        while k > 0 && at(i) != at(k) {
            k = fail[k - 1];
        }
        if at(i) == at(k) {
            k += 1;
        }
        fail[i] = k;
    }
    fail
}

struct Kmp<'p> {
    pat: &'p [u16],
    fail: Vec<usize>,
}

impl<'p> Kmp<'p> {
    fn new(pat: &'p [u16]) -> Kmp<'p> {
        Kmp {
            pat,
            fail: failure(pat.len(), |i| pat[i]),
        }
    }

    fn find_from(&self, s: &[u16], from: usize) -> Option<usize> {
        let m = self.pat.len();
        let mut k = 0;
        for (i, &u) in s.iter().enumerate().skip(from) {
            while k > 0 && u != self.pat[k] {
                k = self.fail[k - 1];
            }
            if u == self.pat[k] {
                k += 1;
            }
            if k == m {
                return Some(i + 1 - m);
            }
        }
        None
    }
}

struct KmpBack<'p> {
    pat: &'p [u16],
    fail: Vec<usize>,
}

impl<'p> KmpBack<'p> {
    fn new(pat: &'p [u16]) -> KmpBack<'p> {
        let m = pat.len();
        KmpBack {
            pat,
            fail: failure(m, |i| pat[m - 1 - i]),
        }
    }

    fn rfind_before(&self, s: &[u16], end: usize) -> Option<usize> {
        let m = self.pat.len();
        let at = |k: usize| self.pat[m - 1 - k];
        let mut k = 0;
        for (j, &u) in s.get(..end)?.iter().enumerate().rev() {
            while k > 0 && u != at(k) {
                k = self.fail[k - 1];
            }
            if u == at(k) {
                k += 1;
            }
            if k == m {
                return Some(j);
            }
        }
        None
    }
}

fn index_from(s: &[u16], sub: &[u16], from: usize) -> Option<usize> {
    if sub.is_empty() {
        return None;
    }
    Kmp::new(sub).find_from(s, from)
}

pub fn find(
    s: &[u16],
    sub: &[u16],
    dir: Direction,
    start: Option<i64>,
    occurrence: Option<i64>,
) -> Result<i64, LibError> {
    let len = len_i64(s);
    let pos = match start {
        None => match dir {
            Direction::FromStart => 1,
            Direction::FromEnd => len.max(1),
        },
        Some(p) if p >= 1 && p <= len => p,
        Some(_) => return Err(param_err(4)),
    };
    let occ = occurrence.unwrap_or(1);
    if occ < 1 {
        return Err(param_err(5));
    }
    if sub.is_empty() {
        return Ok(pos);
    }
    let pos0 = usize::try_from(pos - 1).unwrap_or(0);
    let m = sub.len();
    match dir {
        Direction::FromStart => {
            let kmp = Kmp::new(sub);
            let mut from = pos0;
            let mut found = 0usize;
            for _ in 0..occ {
                match kmp.find_from(s, from) {
                    Some(i) => {
                        found = i;
                        from = i + m;
                    }
                    None => return Ok(0),
                }
            }
            Ok(found as i64 + 1)
        }
        Direction::FromEnd => {
            let Some(max_start) = s.len().checked_sub(m) else {
                return Ok(0);
            };
            let kmp = KmpBack::new(sub);
            let mut end = pos0.min(max_start) + m;
            for _ in 0..occ {
                match kmp.rfind_before(s, end) {
                    Some(j) => end = j,
                    None => return Ok(0),
                }
            }
            Ok(end as i64 + 1)
        }
    }
}

pub fn find_legacy(s: &[u16], sub: &[u16]) -> i64 {
    if sub.is_empty() {
        return 1;
    }
    index_from(s, sub, 0).map_or(0, |i| i as i64 + 1)
}

pub fn count(s: &[u16], sub: &[u16]) -> i64 {
    if sub.is_empty() {
        return 0;
    }
    let mut n = 0i64;
    let mut from = 0;
    while let Some(i) = index_from(s, sub, from) {
        n += 1;
        from = i + sub.len();
    }
    n
}

pub fn replace(s: &[u16], from: &[u16], to: &[u16]) -> Vec<u16> {
    if from.is_empty() {
        return s.to_vec();
    }
    let mut out = Vec::with_capacity(s.len());
    let mut pos = 0;
    while let Some(i) = index_from(s, from, pos) {
        out.extend_from_slice(&s[pos..i]);
        out.extend_from_slice(to);
        pos = i + from.len();
    }
    out.extend_from_slice(&s[pos..]);
    out
}

const SHORT_DELIMS: usize = 16;

pub fn split(s: &[u16], delims: &[u16], keep_empty: bool) -> Vec<Vec<u16>> {
    let bits = (delims.len() > SHORT_DELIMS).then(|| {
        let mut b = vec![0u64; 1024];
        for &d in delims {
            if let Some(w) = b.get_mut(usize::from(d >> 6)) {
                *w |= 1 << (d & 63);
            }
        }
        b
    });
    let is_delim = |u: &u16| match &bits {
        Some(b) => b
            .get(usize::from(u >> 6))
            .is_some_and(|w| w >> (u & 63) & 1 == 1),
        None => delims.contains(u),
    };
    let mut out = Vec::new();
    for piece in s.split(is_delim) {
        if keep_empty || !piece.is_empty() {
            out.push(piece.to_vec());
        }
    }
    out
}

pub fn starts_with(s: &[u16], prefix: &[u16]) -> Result<bool, LibError> {
    if prefix.is_empty() {
        return Err(param_err(2));
    }
    Ok(s.starts_with(prefix))
}

pub fn ends_with(s: &[u16], suffix: &[u16]) -> Result<bool, LibError> {
    if suffix.is_empty() {
        return Err(param_err(2));
    }
    Ok(s.ends_with(suffix))
}

pub fn char_from_code(code: i64) -> Vec<u16> {
    match u16::try_from(code) {
        Ok(u) => vec![u],
        Err(_) => Vec::new(),
    }
}

pub fn char_code(s: &[u16], pos: i64) -> i64 {
    if pos < 1 {
        return -1;
    }
    usize::try_from(pos - 1)
        .ok()
        .and_then(|i| s.get(i))
        .map_or(-1, |&u| i64::from(u))
}
