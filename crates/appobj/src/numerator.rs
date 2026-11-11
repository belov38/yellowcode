use crate::model::ObjId;
use ibschema::values::RefId;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub(crate) const MAX_RUNS: usize = 4096;

const MAX_DIGITS: usize = 38;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SeriesKey {
    pub obj: ObjId,
    pub owner: RefId,
    pub parent: RefId,
}

pub(crate) fn trimmed(code: &str) -> &str {
    code.trim_end_matches(' ')
}

pub(crate) fn fold(code: &str) -> String {
    trimmed(code).chars().flat_map(char::to_uppercase).collect()
}

pub(crate) fn blank(code: &str) -> bool {
    trimmed(code).is_empty()
}

pub(crate) fn padded(code: &str, len: usize) -> String {
    let mut out: String = code.chars().take(len).collect();
    let have = out.chars().count();
    out.extend(std::iter::repeat_n(' ', len.saturating_sub(have)));
    out
}

struct Split<'a> {
    head: &'a str,
    number: Option<(u128, usize)>,
}

fn split(code: &str) -> Split<'_> {
    let code = trimmed(code);
    let digits = code
        .bytes()
        .rev()
        .take_while(u8::is_ascii_digit)
        .count()
        .min(code.len());
    let at = code.len().saturating_sub(digits);
    let (head, tail) = (code.get(..at).unwrap_or(""), code.get(at..).unwrap_or(""));
    let number = if tail.is_empty() || tail.len() > MAX_DIGITS {
        None
    } else {
        tail.parse::<u128>().ok().map(|n| (n, tail.len()))
    };
    match number {
        Some(_) => Split { head, number },
        None => Split {
            head: code,
            number: None,
        },
    }
}

fn pow10(width: usize) -> Option<u128> {
    10u128.checked_pow(u32::try_from(width).ok()?)
}

fn digits(n: u128, width: usize) -> String {
    format!("{n:0width$}")
}

pub(crate) fn next_after(max: &str, len: usize) -> String {
    let s = split(max);
    match s.number {
        Some((n, width)) => {
            let next = n.saturating_add(1);
            match pow10(width) {
                Some(limit) if next < limit => format!("{}{}", s.head, digits(next, width)),
                _ => trimmed(max).to_string(),
            }
        }
        None => {
            let have = s.head.chars().count();
            if have >= len {
                return trimmed(max).to_string();
            }
            format!("{}{}1", s.head, "0".repeat(len - have - 1))
        }
    }
}

pub(crate) fn first_code(prefix: &str, len: usize) -> String {
    let have = prefix.chars().count();
    if have >= len {
        return prefix.chars().take(len).collect();
    }
    format!("{prefix}{}1", "0".repeat(len - have - 1))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    head: String,
    head_key: String,
    width: usize,
    first: u128,
    last: u128,
}

impl Run {
    fn of(&self, head_key: &str, width: usize) -> bool {
        self.head_key == head_key && self.width == width
    }

    fn touches(&self, n: u128) -> bool {
        self.first <= n.saturating_add(1) && n <= self.last.saturating_add(1)
    }

    fn best(&self, prefix: &str) -> Option<String> {
        let head_chars = self.head_key.chars().count();
        let n = if prefix.chars().count() <= head_chars {
            self.head_key.starts_with(prefix).then_some(self.last)?
        } else {
            let rest = prefix.strip_prefix(self.head_key.as_str())?;
            if !rest.bytes().all(|b| b.is_ascii_digit()) || rest.len() > self.width {
                return None;
            }
            let span = pow10(self.width - rest.len())?;
            let lo = rest.parse::<u128>().ok()?.checked_mul(span)?;
            let hi = lo.checked_add(span - 1)?;
            let (lo, hi) = (lo.max(self.first), hi.min(self.last));
            (lo <= hi).then_some(hi)?
        };
        Some(format!("{}{}", self.head, digits(n, self.width)))
    }
}

#[derive(Default)]
struct Series {
    runs: Vec<Run>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    key: SeriesKey,
    head_key: String,
    width: usize,
    first: u128,
    last: u128,
}

#[derive(Default)]
pub(crate) struct Ledger {
    tokens: Mutex<Vec<Token>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ledger {
    pub(crate) fn push(&self, token: Token) {
        let mut tokens = lock(&self.tokens);
        if let Some(held) = tokens.iter_mut().rev().find(|t| {
            t.key == token.key
                && t.head_key == token.head_key
                && t.width == token.width
                && t.last.checked_add(1) == Some(token.first)
        }) {
            held.last = token.last;
            return;
        }
        tokens.push(token);
    }
}

pub(crate) struct Issued {
    pub code: String,
    pub token: Option<Token>,
}

#[derive(Debug)]
pub(crate) enum IssueError<E> {
    Read(E),
    Full,
}

#[derive(Default)]
struct Inner {
    series: HashMap<SeriesKey, Series>,
    runs: usize,
}

impl Inner {
    fn add(&mut self, key: SeriesKey, head: &str, width: usize, n: u128) -> Result<bool, ()> {
        let head_key = fold(head);
        let Inner { series, runs } = self;
        let held = series.get(&key).and_then(|s| {
            s.runs
                .iter()
                .find(|r| r.of(&head_key, width) && r.touches(n))
        });
        if let Some(r) = held
            && r.first <= n
            && n <= r.last
        {
            return Ok(false);
        }
        if held.is_none() && *runs >= MAX_RUNS {
            return Err(());
        }
        let s = series.entry(key).or_default();
        let before = s.runs.len();
        match s
            .runs
            .iter_mut()
            .find(|r| r.of(&head_key, width) && r.touches(n))
        {
            Some(r) => {
                r.first = r.first.min(n);
                r.last = r.last.max(n);
            }
            None => s.runs.push(Run {
                head: head.to_string(),
                head_key: head_key.clone(),
                width,
                first: n,
                last: n,
            }),
        }
        join(&mut s.runs, &head_key, width);
        *runs = runs.saturating_sub(before).saturating_add(s.runs.len());
        Ok(true)
    }

    fn remove(&mut self, key: &SeriesKey, head_key: &str, width: usize, first: u128, last: u128) {
        let Inner { series, runs } = self;
        let Some(s) = series.get_mut(key) else {
            return;
        };
        let before = s.runs.len();
        let mut kept: Vec<Run> = Vec::with_capacity(before + 1);
        for run in std::mem::take(&mut s.runs) {
            if !run.of(head_key, width) || run.last < first || run.first > last {
                kept.push(run);
                continue;
            }
            if run.first < first {
                kept.push(Run {
                    last: first - 1,
                    ..run.clone()
                });
            }
            if run.last > last {
                kept.push(Run {
                    first: last + 1,
                    ..run
                });
            }
        }
        s.runs = kept;
        let after = s.runs.len();
        *runs = runs.saturating_sub(before).saturating_add(after);
        if after == 0 {
            series.remove(key);
        }
    }
}

fn join(runs: &mut Vec<Run>, head_key: &str, width: usize) {
    let (mut family, mut others): (Vec<Run>, Vec<Run>) = std::mem::take(runs)
        .into_iter()
        .partition(|r| r.of(head_key, width));
    family.sort_by_key(|r| r.first);
    let mut joined: Vec<Run> = Vec::with_capacity(family.len());
    for run in family {
        match joined.last_mut() {
            Some(prev) if prev.last.saturating_add(1) >= run.first => {
                prev.last = prev.last.max(run.last);
            }
            _ => joined.push(run),
        }
    }
    others.extend(joined);
    *runs = others;
}

#[derive(Default)]
pub(crate) struct Numerator {
    inner: Mutex<Inner>,
    open: Mutex<HashMap<(usize, u64), Weak<Ledger>>>,
}

impl Numerator {
    fn state(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    pub(crate) fn runs(&self) -> usize {
        self.state().runs
    }

    pub(crate) fn series(&self) -> usize {
        self.state().series.len()
    }

    pub(crate) fn ledger(
        &self,
        call: usize,
        txn: u64,
        make: impl FnOnce(Arc<Ledger>) -> bool,
    ) -> Option<Arc<Ledger>> {
        let mut open = lock(&self.open);
        if let Some(live) = open.get(&(call, txn)).and_then(Weak::upgrade) {
            return Some(live);
        }
        if open.len() >= 64 {
            open.retain(|_, w| w.strong_count() > 0);
        }
        let ledger = Arc::new(Ledger::default());
        if !make(ledger.clone()) {
            return None;
        }
        open.insert((call, txn), Arc::downgrade(&ledger));
        Some(ledger)
    }

    pub(crate) fn release(&self, ledger: &Ledger) {
        let tokens = std::mem::take(&mut *lock(&ledger.tokens));
        let mut state = self.state();
        for t in tokens.into_iter().rev() {
            state.remove(&t.key, &t.head_key, t.width, t.first, t.last);
        }
    }

    pub(crate) fn issue_string<E>(
        &self,
        key: SeriesKey,
        len: usize,
        prefix: &str,
        in_base: impl FnOnce() -> Result<Option<String>, E>,
        compare: impl Fn(&str, &str) -> Ordering,
    ) -> Result<Issued, IssueError<E>> {
        let folded = fold(prefix);
        let mut state = self.state();
        let base = in_base().map_err(IssueError::Read)?;
        let reserved = state.series.get(&key).and_then(|s| {
            s.runs
                .iter()
                .filter_map(|r| r.best(&folded))
                .max_by(|a, b| compare(a, b))
        });
        let max = match (base, reserved) {
            (Some(a), Some(b)) => Some(if compare(&a, &b) == Ordering::Less {
                b
            } else {
                a
            }),
            (a, b) => a.or(b),
        };
        let code = match &max {
            Some(max) => next_after(max, len),
            None => first_code(prefix, len),
        };
        if let Some(max) = max.filter(|m| trimmed(m) == code) {
            return Ok(Issued {
                code: max,
                token: None,
            });
        }
        let mut token = None;
        let s = split(&code);
        if let Some((n, width)) = s.number
            && state
                .add(key, s.head, width, n)
                .map_err(|()| IssueError::Full)?
        {
            token = Some(Token {
                key,
                head_key: fold(s.head),
                width,
                first: n,
                last: n,
            });
        }
        Ok(Issued {
            code: padded(&code, len),
            token,
        })
    }

    pub(crate) fn issue_number<E>(
        &self,
        key: SeriesKey,
        digits_of_code: usize,
        in_base: impl FnOnce() -> Result<Option<u128>, E>,
    ) -> Result<Issued, IssueError<E>> {
        let mut state = self.state();
        let base = in_base().map_err(IssueError::Read)?;
        let reserved = state
            .series
            .get(&key)
            .and_then(|s| s.runs.iter().map(|r| r.last).max());
        let max = base.into_iter().chain(reserved).max();
        let top = pow10(digits_of_code).map_or(u128::MAX, |p| p - 1);
        let next = max.map_or(1, |m| m.saturating_add(1)).min(top);
        let mut token = None;
        if !max.is_some_and(|m| m >= next)
            && state.add(key, "", 0, next).map_err(|()| IssueError::Full)?
        {
            token = Some(Token {
                key,
                head_key: String::new(),
                width: 0,
                first: next,
                last: next,
            });
        }
        Ok(Issued {
            code: next.to_string(),
            token,
        })
    }
}
