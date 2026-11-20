use crate::model::ObjId;
use ibschema::values::RefId;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub(crate) const MAX_RUNS: usize = 4096;

static MOMENTS: AtomicU64 = AtomicU64::new(1);

fn moment() -> u64 {
    MOMENTS.fetch_add(1, AtomicOrdering::Relaxed)
}

const DIED_AT_END: u8 = 0;
const DIED_IN_BODY: u8 = 1;
const DIED_UNSEEN: u8 = 2;

#[derive(Debug)]
pub(crate) struct HolderMark {
    died: Arc<AtomicU64>,
    wrote: Arc<AtomicBool>,
    death: Arc<AtomicU8>,
}

thread_local! {
    static LAST_ASSIGNED: std::cell::RefCell<Option<(u64, Arc<AtomicU8>)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static ASSIGNED_IN_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl HolderMark {
    pub(crate) fn new() -> Arc<HolderMark> {
        Arc::new(HolderMark {
            died: Arc::new(AtomicU64::new(0)),
            wrote: Arc::new(AtomicBool::new(false)),
            death: Arc::new(AtomicU8::new(DIED_AT_END)),
        })
    }

    pub(crate) fn holder(self: &Arc<Self>) -> Holder {
        Holder {
            live: Arc::downgrade(self),
            died: self.died.clone(),
            wrote: self.wrote.clone(),
            death: self.death.clone(),
            seen: Arc::new(AtomicBool::new(false)),
            found_at: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn note_write(&self) {
        self.wrote.store(true, AtomicOrdering::Relaxed);
    }
}

impl Drop for HolderMark {
    fn drop(&mut self) {
        let death = bsl_vm::death();
        let how = match death {
            bsl_vm::Death::End => DIED_AT_END,
            bsl_vm::Death::Body => DIED_IN_BODY,
            bsl_vm::Death::Unobserved => DIED_UNSEEN,
            bsl_vm::Death::Assigned(epoch) => {
                LAST_ASSIGNED.with(|last| {
                    let mut last = last.borrow_mut();
                    if let Some((e, before)) = last.as_ref()
                        && *e == epoch
                    {
                        before.store(DIED_UNSEEN, AtomicOrdering::Relaxed);
                    }
                    *last = Some((epoch, self.death.clone()));
                });
                DIED_IN_BODY
            }
        };
        self.death.store(how, AtomicOrdering::Relaxed);
        self.died.store(moment(), AtomicOrdering::Relaxed);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Holder {
    live: Weak<HolderMark>,
    died: Arc<AtomicU64>,
    wrote: Arc<AtomicBool>,
    death: Arc<AtomicU8>,
    seen: Arc<AtomicBool>,
    found_at: Arc<AtomicU64>,
}

impl Holder {
    fn alive(&self) -> bool {
        self.live.strong_count() > 0
    }

    fn same(&self, other: &Holder) -> bool {
        Weak::ptr_eq(&self.live, &other.live)
    }

    fn died(&self) -> u64 {
        self.died.load(AtomicOrdering::Relaxed)
    }

    fn wrote(&self) -> bool {
        self.wrote.load(AtomicOrdering::Relaxed)
    }

    pub(crate) fn note_seen(&self) {
        self.seen.store(true, AtomicOrdering::Relaxed);
    }

    pub(crate) fn seen(&self) -> bool {
        self.seen.load(AtomicOrdering::Relaxed)
    }

    pub(crate) fn note_found(&self, deletions: u64) {
        self.found_at
            .store(deletions.wrapping_add(1), AtomicOrdering::Relaxed);
    }

    pub(crate) fn found(&self, deletions: u64) -> bool {
        self.found_at.load(AtomicOrdering::Relaxed) == deletions.wrapping_add(1)
    }

    fn assigned(&self) -> bool {
        self.death.load(AtomicOrdering::Relaxed) != DIED_AT_END
    }

    fn unseen(&self) -> bool {
        self.death.load(AtomicOrdering::Relaxed) == DIED_UNSEEN
    }
}

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
    head: String,
    head_key: String,
    width: usize,
    first: u128,
    last: u128,
    order: u64,
    prefixed: bool,
}

impl Token {
    pub(crate) fn number(&self) -> u128 {
        self.first
    }
}

#[derive(Default)]
pub(crate) struct Ledger {
    tokens: Mutex<Vec<(Token, Holder)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Ledger {
    pub(crate) fn push(&self, token: Token, holder: Holder) {
        let mut tokens = lock(&self.tokens);
        if let Some((held, _)) = tokens.iter_mut().rev().find(|(t, h)| {
            h.same(&holder)
                && t.key == token.key
                && t.head_key == token.head_key
                && t.width == token.width
                && t.prefixed == token.prefixed
                && t.last.checked_add(1) == Some(token.first)
        }) {
            held.last = token.last;
            return;
        }
        tokens.push((token, holder));
    }

    fn take(
        &self,
        key: &SeriesKey,
        head_key: &str,
        width: usize,
        n: u128,
        holder: &Holder,
    ) -> bool {
        let mut tokens = lock(&self.tokens);
        let Some(i) = tokens.iter().position(|(t, h)| {
            h.same(holder)
                && t.key == *key
                && t.head_key == head_key
                && t.width == width
                && t.first <= n
                && n <= t.last
        }) else {
            return false;
        };
        let (t, h) = tokens.remove(i);
        if t.first < n {
            tokens.insert(
                i,
                (
                    Token {
                        last: n - 1,
                        ..t.clone()
                    },
                    h.clone(),
                ),
            );
        }
        if t.last > n {
            tokens.push((Token { first: n + 1, ..t }, h));
        }
        true
    }
}

pub(crate) struct Issued {
    pub code: String,
    pub token: Option<Token>,
    pub ended: bool,
}

#[derive(Debug)]
pub(crate) enum IssueError<E> {
    Read(E),
    Full,
    Unobserved,
    Stale,
}

#[derive(Clone, Debug)]
struct Pooled {
    head: String,
    head_key: String,
    width: usize,
    n: u128,
    call: Option<u64>,
    unobserved: bool,
    ended: bool,
    unseen: bool,
    txn: Option<u64>,
    skipped: bool,
}

impl Pooled {
    fn code(&self) -> String {
        if self.width == 0 {
            self.n.to_string()
        } else {
            format!("{}{}", self.head, digits(self.n, self.width))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Given {
    InCall,
    InTxn(u64),
    Unobserved,
    Ended,
    EndedUnseen,
}

type Kept = (Token, Holder, Option<u64>, u64);

const MAX_POOLED: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) struct IssueCtx {
    pub call: Option<u64>,
    pub pool: bool,
}

impl IssueCtx {}

#[derive(Default)]
struct Inner {
    series: HashMap<SeriesKey, Series>,
    runs: usize,
    freed: std::collections::HashSet<SeriesKey>,
    pool: HashMap<SeriesKey, VecDeque<Pooled>>,
    pooled: usize,
    closed: std::collections::HashSet<SeriesKey>,
}

impl Inner {
    fn note_freed(&mut self, key: SeriesKey) {
        if key.owner.is_nil() && key.parent.is_nil() {
            self.freed.insert(key);
        }
    }

    fn give_back(
        &mut self,
        t: &Token,
        numbers: impl Iterator<Item = u128>,
        call: Option<u64>,
        how: Given,
    ) {
        for n in numbers {
            self.remove(&t.key, &t.head_key, t.width, n, n);
            if self.closed.contains(&t.key) {
                continue;
            }
            if self.pooled >= MAX_POOLED {
                let freed = self.pool.remove(&t.key).map_or(0, |q| q.len());
                self.pooled = self.pooled.saturating_sub(freed);
                self.closed.insert(t.key);
                continue;
            }
            self.pool.entry(t.key).or_default().push_back(Pooled {
                head: t.head.clone(),
                head_key: t.head_key.clone(),
                width: t.width,
                n,
                call,
                unobserved: how == Given::Unobserved,
                ended: matches!(how, Given::Ended | Given::EndedUnseen),
                unseen: how == Given::EndedUnseen,
                txn: match how {
                    Given::InTxn(txn) => Some(txn),
                    _ => None,
                },
                skipped: false,
            });
            self.pooled += 1;
        }
    }

    fn pooled_largest(
        &self,
        key: &SeriesKey,
        prefix: &str,
        compare: &dyn Fn(&str, &str) -> Ordering,
    ) -> Option<String> {
        self.pool.get(key).and_then(|q| {
            q.iter()
                .map(Pooled::code)
                .filter(|c| fold(c).starts_with(prefix))
                .max_by(|a, b| compare(a, b))
        })
    }

    fn purge(&mut self, key: &SeriesKey, call: Option<u64>) {
        let Some(q) = self.pool.get_mut(key) else {
            return;
        };
        let before = q.len();
        q.retain(|p| p.ended || (p.call.is_some() && p.call == call));
        let (after, empty) = (q.len(), q.is_empty());
        self.pooled = self.pooled.saturating_sub(before - after);
        if empty {
            self.pool.remove(key);
        }
        if after != before {
            self.note_freed(*key);
        }
    }

    fn unobserved(&self, key: &SeriesKey, prefix: &str) -> bool {
        self.pool.get(key).is_some_and(|q| {
            q.iter()
                .any(|p| p.unobserved && fold(&p.code()).starts_with(prefix))
        })
    }

    fn pooled_first(&self, key: &SeriesKey, prefix: &str, head_key: &str) -> Option<usize> {
        self.pool.get(key).and_then(|q| {
            q.iter()
                .position(|p| p.head_key == head_key && fold(&p.code()).starts_with(prefix))
        })
    }

    fn take_pooled(&mut self, key: &SeriesKey, at: usize) -> Option<Pooled> {
        let q = self.pool.get_mut(key)?;
        let p = q.remove(at)?;
        self.pooled = self.pooled.saturating_sub(1);
        if q.is_empty() {
            self.pool.remove(key);
        }
        Some(p)
    }
}

impl Inner {
    fn add(&mut self, key: SeriesKey, head: &str, width: usize, n: u128) -> Result<bool, ()> {
        let head_key = fold(head);
        let Inner { series, runs, .. } = self;
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
        let Inner { series, runs, .. } = self;
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
    kept: Mutex<Vec<Kept>>,
    deletions: AtomicU64,
}

impl Numerator {
    fn state(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    pub(crate) fn runs(&self) -> usize {
        self.sweep(None);
        self.state().runs
    }

    pub(crate) fn series(&self) -> usize {
        self.sweep(None);
        self.state().series.len()
    }

    fn sweep(&self, call: Option<u64>) {
        let mut kept = lock(&self.kept);
        if kept.is_empty() {
            return;
        }
        let mut dead: Vec<Kept> = Vec::new();
        kept.retain(|e| {
            if e.1.alive() {
                return true;
            }
            dead.push(e.clone());
            false
        });
        drop(kept);
        if dead.is_empty() {
            return;
        }
        let (mut here, ended): (Vec<_>, Vec<_>) = dead
            .into_iter()
            .partition(|(_, _, c, _)| call.is_some() && *c == call);
        here.sort_by_key(|(t, h, _, at)| {
            let moment = if *at > 0 { *at } else { h.died() };
            (moment, std::cmp::Reverse(t.order))
        });
        let (gone, session): (Vec<_>, Vec<_>) =
            ended.into_iter().partition(|(_, _, _, at)| *at > 0);
        let mut state = self.state();
        for (t, _, _, _) in &gone {
            state.remove(&t.key, &t.head_key, t.width, t.first, t.last);
            state.note_freed(t.key);
        }
        Self::session_end(&mut state, session);
        for (t, _, _, at) in &here {
            let how = if *at > 0 {
                Given::Unobserved
            } else {
                Given::InCall
            };
            state.give_back(t, (t.first..=t.last).rev(), call, how);
        }
    }

    fn session_end(state: &mut Inner, mut session: Vec<Kept>) {
        session.sort_by(|(a, ha, ca, _), (b, hb, cb, _)| {
            ca.cmp(cb)
                .then_with(|| a.prefixed.cmp(&b.prefixed))
                .then_with(|| hb.assigned().cmp(&ha.assigned()))
                .then_with(|| {
                    if ha.assigned() && hb.assigned() {
                        ha.died().cmp(&hb.died())
                    } else {
                        b.order.cmp(&a.order)
                    }
                })
        });
        for (t, h, _, _) in &session {
            let how = if h.unseen() {
                Given::EndedUnseen
            } else {
                Given::Ended
            };
            state.give_back(t, (t.first..=t.last).rev(), None, how);
        }
    }

    pub(crate) fn number_unobserved(
        &self,
        key: SeriesKey,
        call: Option<u64>,
        txn: Option<u64>,
    ) -> bool {
        self.sweep(call);
        let state = self.state();
        if state.freed.contains(&key) {
            return true;
        }
        let below = |p: &Pooled| {
            state.series.get(&key).is_some_and(|s| {
                s.runs
                    .iter()
                    .any(|r| r.head_key == p.head_key && r.width == p.width && r.last > p.n)
            })
        };
        let around = |p: &Pooled| {
            state.series.get(&key).map_or((0u128, 0u128), |s| {
                s.runs
                    .iter()
                    .filter(|r| r.head_key == p.head_key && r.width == p.width)
                    .fold((0, 0), |(above, under), r| {
                        let a = if r.last > p.n {
                            r.last - r.first.max(p.n + 1) + 1
                        } else {
                            0
                        };
                        let b = if r.first < p.n {
                            r.last.min(p.n - 1) - r.first + 1
                        } else {
                            0
                        };
                        (above.saturating_add(a), under.saturating_add(b))
                    })
            })
        };
        let in_txn = |p: &Pooled| p.txn.is_some() && p.txn == txn && around(p) == (1, 0);
        state.pool.get(&key).is_some_and(|q| {
            (txn.is_none() && !q.is_empty())
                || q.len() >= 2
                || q.iter().any(|p| {
                    p.ended
                        || p.unobserved
                        || (below(p) && !in_txn(p))
                        || !(p.call.is_some() && p.call == call)
                })
        })
    }

    pub(crate) fn dead_writers(
        &self,
        ledger: &Ledger,
        key: SeriesKey,
    ) -> Option<Vec<(String, Holder)>> {
        let mut found = Vec::new();
        for (t, h) in lock(&ledger.tokens).iter() {
            if t.key != key || h.alive() || !h.wrote() {
                continue;
            }
            if t.first != t.last {
                return None;
            }
            let code = if t.width == 0 {
                t.first.to_string()
            } else {
                format!("{}{}", t.head, digits(t.first, t.width))
            };
            found.push((code, h.clone()));
        }
        Some(found)
    }

    pub(crate) fn note_deletion(&self) {
        self.deletions.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn deletions(&self) -> u64 {
        self.deletions.load(AtomicOrdering::Relaxed)
    }

    pub(crate) fn pooled(&self, key: SeriesKey) -> bool {
        self.state().pool.get(&key).is_some_and(|q| !q.is_empty())
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

    pub(crate) fn release(&self, ledger: &Ledger, call: Option<u64>) {
        let tokens = std::mem::take(&mut *lock(&ledger.tokens));
        let mut kept = lock(&self.kept);
        let mut tokens: Vec<_> = tokens.into_iter().collect();
        tokens.sort_by_key(|(t, _)| std::cmp::Reverse(t.order));
        for (t, h) in tokens {
            let at = if h.alive() { 0 } else { moment() };
            kept.push((t, h, call, at));
        }
    }

    pub(crate) fn let_go(
        &self,
        ledger: &Ledger,
        token: &Token,
        n: u128,
        holder: &Holder,
        call: Option<u64>,
    ) -> bool {
        if !ledger.take(&token.key, &token.head_key, token.width, n, holder) {
            return false;
        }
        self.state()
            .give_back(token, std::iter::once(n), call, Given::InCall);
        true
    }

    pub(crate) fn release_dead(
        &self,
        ledger: &Ledger,
        key: SeriesKey,
        call: Option<u64>,
        txn: u64,
    ) {
        self.sweep(call);
        let mut dead: Vec<(Token, Holder)> = Vec::new();
        {
            let mut tokens = lock(&ledger.tokens);
            tokens.retain(|(t, h)| {
                if t.key != key || h.alive() || h.wrote() {
                    return true;
                }
                dead.push((t.clone(), h.clone()));
                false
            });
        }
        if dead.is_empty() {
            return;
        }
        dead.sort_by_key(|(t, h)| (h.died(), std::cmp::Reverse(t.order)));
        let mut state = self.state();
        for (t, _) in &dead {
            state.give_back(t, (t.first..=t.last).rev(), call, Given::InTxn(txn));
        }
    }

    pub(crate) fn release_token(&self, t: &Token) {
        self.state()
            .remove(&t.key, &t.head_key, t.width, t.first, t.last);
    }

    pub(crate) fn issue_string_in<E>(
        &self,
        ctx: IssueCtx,
        key: SeriesKey,
        len: usize,
        prefix: &str,
        in_base: impl FnOnce() -> Result<Option<String>, E>,
        compare: impl Fn(&str, &str) -> Ordering,
    ) -> Result<Issued, IssueError<E>> {
        let folded = fold(prefix);
        self.sweep(ctx.call);
        let mut state = self.state();
        if ctx.pool && state.closed.contains(&key) {
            return Err(IssueError::Full);
        }
        if ctx.pool {
            state.purge(&key, ctx.call);
            if state.unobserved(&key, &folded) {
                return Err(IssueError::Unobserved);
            }
        }
        let base = in_base().map_err(IssueError::Read)?;
        let reserved = state.series.get(&key).and_then(|s| {
            s.runs
                .iter()
                .filter_map(|r| r.best(&folded))
                .max_by(|a, b| compare(a, b))
        });
        let pooled = if ctx.pool {
            state.pooled_largest(&key, &folded, &compare)
        } else {
            None
        };
        let max = [base, reserved, pooled]
            .into_iter()
            .flatten()
            .reduce(|a, b| {
                if compare(&a, &b) == Ordering::Less {
                    b
                } else {
                    a
                }
            });
        if ctx.pool
            && let Some(m) = &max
        {
            let family = fold(split(m).head);
            if let Some(q) = state.pool.get_mut(&key) {
                for p in q.iter_mut() {
                    if p.ended && p.head_key != family && fold(&p.code()).starts_with(&folded) {
                        p.skipped = true;
                    }
                }
            }
            if let Some(at) = state.pooled_first(&key, &folded, &family) {
                let first = state.pool.get(&key).and_then(|q| q.get(at)).cloned();
                if let Some(p) = first {
                    if p.ended && p.skipped {
                        return Err(IssueError::Stale);
                    }
                    if p.unseen {
                        return Err(IssueError::Unobserved);
                    }
                    let _ = state.take_pooled(&key, at);
                    let code = p.code();
                    let mut token = None;
                    if state
                        .add(key, &p.head, p.width, p.n)
                        .map_err(|()| IssueError::Full)?
                    {
                        token = Some(Token {
                            key,
                            head: p.head.clone(),
                            head_key: p.head_key.clone(),
                            width: p.width,
                            first: p.n,
                            last: p.n,
                            order: moment(),
                            prefixed: !folded.is_empty(),
                        });
                    }
                    return Ok(Issued {
                        code: padded(&code, len),
                        token,
                        ended: p.ended,
                    });
                }
            }
        }
        let code = match &max {
            Some(max) => next_after(max, len),
            None => first_code(prefix, len),
        };
        if let Some(max) = max.filter(|m| trimmed(m) == code) {
            return Ok(Issued {
                code: max,
                token: None,
                ended: false,
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
                head: s.head.to_string(),
                head_key: fold(s.head),
                width,
                first: n,
                last: n,
                order: moment(),
                prefixed: !folded.is_empty(),
            });
        }
        Ok(Issued {
            code: padded(&code, len),
            token,
            ended: false,
        })
    }

    pub(crate) fn issue_number_in<E>(
        &self,
        ctx: IssueCtx,
        key: SeriesKey,
        digits_of_code: usize,
        in_base: impl FnOnce() -> Result<Option<u128>, E>,
    ) -> Result<Issued, IssueError<E>> {
        self.sweep(ctx.call);
        let mut state = self.state();
        if ctx.pool && state.closed.contains(&key) {
            return Err(IssueError::Full);
        }
        if ctx.pool {
            state.purge(&key, ctx.call);
            if state.unobserved(&key, "") {
                return Err(IssueError::Unobserved);
            }
        }
        let base = in_base().map_err(IssueError::Read)?;
        if ctx.pool
            && let Some(at) = state.pooled_first(&key, "", "")
            && let Some(p) = state.pool.get(&key).and_then(|q| q.get(at)).cloned()
        {
            if p.unseen {
                return Err(IssueError::Unobserved);
            }
            let _ = state.take_pooled(&key, at);
            let mut token = None;
            if state.add(key, "", 0, p.n).map_err(|()| IssueError::Full)? {
                token = Some(Token {
                    key,
                    head: String::new(),
                    head_key: String::new(),
                    width: 0,
                    first: p.n,
                    last: p.n,
                    order: moment(),
                    prefixed: false,
                });
            }
            return Ok(Issued {
                code: p.n.to_string(),
                token,
                ended: p.ended,
            });
        }
        let reserved = state
            .series
            .get(&key)
            .and_then(|s| s.runs.iter().map(|r| r.last).max());
        let pooled = state
            .pool
            .get(&key)
            .and_then(|q| q.iter().map(|p| p.n).max());
        let max = base.into_iter().chain(reserved).chain(pooled).max();
        let top = pow10(digits_of_code).map_or(u128::MAX, |p| p - 1);
        let next = max.map_or(1, |m| m.saturating_add(1)).min(top);
        let mut token = None;
        if !max.is_some_and(|m| m >= next)
            && state.add(key, "", 0, next).map_err(|()| IssueError::Full)?
        {
            token = Some(Token {
                key,
                head: String::new(),
                head_key: String::new(),
                width: 0,
                first: next,
                last: next,
                order: moment(),
                prefixed: false,
            });
        }
        Ok(Issued {
            code: next.to_string(),
            token,
            ended: false,
        })
    }
}
