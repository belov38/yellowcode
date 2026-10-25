use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bsl_prim::{Collation, Strength};
use query_ir::{ExecError, QValue};

use crate::model::{block, hash_inserts_peak, vec_block, vec_pushes_peak};

use super::{code_points, push_units};

const SHARP_S: u32 = 0xDF;
const MASK_CACHE: usize = 4096;

enum Item {
    Any,
    Lit(Vec<u8>),
    Set(CharSet),
}

struct CharSet {
    negated: bool,
    singles: HashSet<Vec<u8>>,
    ranges: Vec<(Vec<u8>, Vec<u8>)>,
}

impl CharSet {
    fn contains(&self, key: &[u8]) -> bool {
        let hit = self.singles.contains(key)
            || self
                .ranges
                .iter()
                .any(|(lo, hi)| lo.as_slice() <= key && key <= hi.as_slice());
        hit != self.negated
    }
}

pub struct LikePattern {
    items: Vec<Item>,
    lits: HashMap<Vec<u8>, Vec<usize>>,
    any: Vec<usize>,
    sets: Vec<usize>,
    star: Vec<u64>,
    words: usize,
    cache: Mutex<HashMap<u32, Arc<[u64]>>>,
}

fn key_of_cp(cp: u32, c: &dyn Collation) -> Vec<u8> {
    let mut u = Vec::with_capacity(2);
    push_units(cp, &mut u);
    c.sort_key(Strength::Secondary, &u)
}

fn is_special(ch: u32) -> bool {
    ch == u32::from(b'%') || ch == u32::from(b'_') || ch == u32::from(b'[')
}

trait Sink {
    fn star(&mut self);
    fn any(&mut self);
    fn lit(&mut self, cp: u32);
    fn set_begin(&mut self, negated: bool);
    fn single(&mut self, cp: u32);
    fn range(&mut self, a: u32, b: u32);
    fn set_end(&mut self);
}

fn parse(p: &[u32], esc: Option<u32>, sink: &mut impl Sink) {
    let mut i = 0;
    while i < p.len() {
        let ch = p.get(i).copied().unwrap_or(0);
        let next = p.get(i + 1).copied();
        if Some(ch) == esc
            && let Some(nx) = next
            && (is_special(nx) || Some(nx) == esc)
        {
            sink.lit(nx);
            i += 2;
            continue;
        }
        i += 1;
        match ch {
            0x25 => sink.star(),
            0x5F => sink.any(),
            0x5B => i += parse_set(p.get(i..).unwrap_or(&[]), sink),
            _ => sink.lit(ch),
        }
    }
}

fn parse_set(p: &[u32], sink: &mut impl Sink) -> usize {
    let negated = p.first() == Some(&0x5E);
    let mut j = usize::from(negated);
    sink.set_begin(negated);
    while let Some(&ch) = p.get(j) {
        j += 1;
        if ch == 0x5D {
            break;
        }
        match (p.get(j), p.get(j + 1)) {
            (Some(&0x2D), Some(&hi)) if hi != 0x5D => {
                sink.range(ch, hi);
                j += 2;
            }
            _ => sink.single(ch),
        }
    }
    sink.set_end();
    j
}

struct Build<'c> {
    c: &'c dyn Collation,
    items: Vec<Item>,
    stars: Vec<usize>,
    set: Option<CharSet>,
}

impl Sink for Build<'_> {
    fn star(&mut self) {
        self.stars.push(self.items.len());
    }

    fn any(&mut self) {
        self.items.push(Item::Any);
    }

    fn lit(&mut self, cp: u32) {
        let key = key_of_cp(cp, self.c);
        if cp == SHARP_S {
            self.items.push(Item::Lit(key.clone()));
        }
        self.items.push(Item::Lit(key));
    }

    fn set_begin(&mut self, negated: bool) {
        self.set = Some(CharSet {
            negated,
            singles: HashSet::new(),
            ranges: Vec::new(),
        });
    }

    fn single(&mut self, cp: u32) {
        if let Some(set) = &mut self.set {
            set.singles.insert(key_of_cp(cp, self.c));
        }
    }

    fn range(&mut self, a: u32, b: u32) {
        if let Some(set) = &mut self.set {
            let (a, b) = (key_of_cp(a, self.c), key_of_cp(b, self.c));
            set.ranges.push(if a <= b { (a, b) } else { (b, a) });
        }
    }

    fn set_end(&mut self) {
        if let Some(set) = self.set.take() {
            self.items.push(Item::Set(set));
        }
    }
}

#[derive(Default)]
struct Shape {
    items: usize,
    lits: usize,
    any: usize,
    sets: usize,
    stars: usize,
    set_keys: usize,
    set_bytes: u64,
    singles: usize,
    ranges: usize,
}

impl Sink for Shape {
    fn star(&mut self) {
        self.stars += 1;
    }

    fn any(&mut self) {
        self.items += 1;
        self.any += 1;
    }

    fn lit(&mut self, cp: u32) {
        let n = if cp == SHARP_S { 2 } else { 1 };
        self.items += n;
        self.lits += n;
    }

    fn set_begin(&mut self, _negated: bool) {
        self.singles = 0;
        self.ranges = 0;
    }

    fn single(&mut self, _cp: u32) {
        self.singles += 1;
        self.set_keys += 1;
    }

    fn range(&mut self, _a: u32, _b: u32) {
        self.ranges += 1;
        self.set_keys += 2;
    }

    fn set_end(&mut self) {
        self.items += 1;
        self.sets += 1;
        self.set_bytes += hash_inserts_peak(self.singles, size_of::<Vec<u8>>() as u64)
            + vec_pushes_peak(self.ranges, size_of::<(Vec<u8>, Vec<u8>)>());
    }
}

impl LikePattern {
    pub fn compile(pat: &[u16], esc: Option<char>, c: &dyn Collation) -> LikePattern {
        let mut p: Vec<u32> = Vec::with_capacity(pat.len());
        p.extend(code_points(pat));
        let mut build = Build {
            c,
            items: Vec::new(),
            stars: Vec::new(),
            set: None,
        };
        parse(&p, esc.map(u32::from), &mut build);
        let Build { items, stars, .. } = build;
        let k = items.len();
        let words = (k + 1).div_ceil(64);
        let mut star = vec![0u64; words];
        for s in stars {
            if let Some(w) = star.get_mut(s / 64) {
                *w |= 1 << (s % 64);
            }
        }
        let mut lits: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        let (mut any, mut sets) = (Vec::new(), Vec::new());
        for (j, it) in items.iter().enumerate() {
            match it {
                Item::Any => any.push(j),
                Item::Lit(key) => lits.entry(key.clone()).or_default().push(j),
                Item::Set(_) => sets.push(j),
            }
        }
        LikePattern {
            items,
            lits,
            any,
            sets,
            star,
            words,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn mask(&self, cp: u32, c: &dyn Collation) -> Arc<[u64]> {
        if let Ok(cache) = self.cache.lock()
            && let Some(m) = cache.get(&cp)
        {
            return m.clone();
        }
        let key = key_of_cp(cp, c);
        let mut m = vec![0u64; self.words];
        let mut set_bit = |j: usize| {
            if let Some(w) = m.get_mut(j / 64) {
                *w |= 1 << (j % 64);
            }
        };
        for &j in &self.any {
            set_bit(j);
        }
        if let Some(js) = self.lits.get(&key) {
            for &j in js {
                set_bit(j);
            }
        }
        for &j in &self.sets {
            if let Some(Item::Set(s)) = self.items.get(j)
                && s.contains(&key)
            {
                set_bit(j);
            }
        }
        let m: Arc<[u64]> = m.into();
        if let Ok(mut cache) = self.cache.lock()
            && cache.len() < MASK_CACHE
        {
            cache.insert(cp, m.clone());
        }
        m
    }

    pub fn matches(&self, text: &[u16], c: &dyn Collation) -> bool {
        let k = self.items.len();
        let mut cur = vec![0u64; self.words];
        let mut next = vec![0u64; self.words];
        if let Some(w) = cur.first_mut() {
            *w = 1;
        }
        let mut local: HashMap<u32, Arc<[u64]>> = HashMap::new();
        for cp in code_points(text) {
            let reps = if cp == SHARP_S { 2 } else { 1 };
            let m = match local.get(&cp) {
                Some(m) => m.clone(),
                None => {
                    let m = self.mask(cp, c);
                    local.insert(cp, m.clone());
                    m
                }
            };
            for _ in 0..reps {
                let mut carry = 0u64;
                let mut live = 0u64;
                for (((n, &s), &mk), &st) in next.iter_mut().zip(&cur).zip(m.iter()).zip(&self.star)
                {
                    crate::probe::hit();
                    let a = s & mk;
                    *n = (a << 1) | carry | (s & st);
                    carry = a >> 63;
                    live |= *n;
                }
                if live == 0 {
                    return false;
                }
                std::mem::swap(&mut cur, &mut next);
            }
        }
        cur.get(k / 64).is_some_and(|w| w >> (k % 64) & 1 == 1)
    }
}

pub const LIKE_CP_KEY: u64 = 40;
const LIKE_COUNT_UNITS: usize = 4096;
const LIKE_FIXED: u64 = 1024;

fn pattern_bytes(units: usize, sh: &Shape, words: usize) -> u64 {
    let usize_ = size_of::<usize>();
    let key = block(LIKE_CP_KEY);
    let keys = (2 * sh.lits + sh.set_keys + 2) as u64 * key;
    let positions = sh.lits as u64 * vec_block(4, usize_) + vec_pushes_peak(sh.lits, usize_);
    vec_block(units, size_of::<u32>())
        + vec_pushes_peak(sh.items, size_of::<Item>())
        + vec_pushes_peak(sh.stars, usize_)
        + vec_block(words, size_of::<u64>())
        + keys
        + sh.set_bytes
        + hash_inserts_peak(sh.lits, size_of::<(Vec<u8>, Vec<usize>)>() as u64)
        + positions
        + vec_pushes_peak(sh.any, usize_)
        + vec_pushes_peak(sh.sets, usize_)
}

fn match_bytes(distinct: usize, words: usize) -> u64 {
    let slot = size_of::<(u32, Arc<[u64]>)>() as u64;
    let state = vec_block(words, size_of::<u64>());
    3 * state
        + (distinct as u64).saturating_mul(block(16 + 8 * words as u64))
        + hash_inserts_peak(distinct, slot)
        + hash_inserts_peak(distinct.min(MASK_CACHE), slot)
}

pub fn like_cost(
    s: &QValue,
    pat: &QValue,
    esc: Option<char>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    let (QValue::String(s), QValue::String(p)) = (s, pat) else {
        return Ok(0);
    };
    let mut total = 0u64;
    let distinct = if s.0.len() > LIKE_COUNT_UNITS {
        let bits = block((0x11_0000 / 64) * 8);
        charge(bits)?;
        total += bits;
        let mut seen = vec![0u64; 0x11_0000 / 64];
        let mut d = 0usize;
        for cp in code_points(&s.0) {
            let (w, b) = ((cp / 64) as usize, cp % 64);
            if let Some(word) = seen.get_mut(w)
                && *word & (1 << b) == 0
            {
                *word |= 1 << b;
                d += 1;
            }
        }
        d
    } else {
        s.0.len()
    };
    let cps = vec_block(p.0.len(), size_of::<u32>());
    charge(cps)?;
    total += cps;
    let mut cp: Vec<u32> = Vec::with_capacity(p.0.len());
    cp.extend(code_points(&p.0));
    let mut sh = Shape::default();
    parse(&cp, esc.map(u32::from), &mut sh);
    drop(cp);
    let words = (sh.items + 1).div_ceil(64);
    let rest = LIKE_FIXED + pattern_bytes(p.0.len(), &sh, words) + match_bytes(distinct, words);
    charge(rest)?;
    total += rest;
    Ok(total)
}

pub fn like(s: &QValue, pat: &QValue, esc: Option<char>, c: &dyn Collation) -> Option<bool> {
    match (s, pat) {
        (QValue::String(s), QValue::String(p)) => {
            Some(LikePattern::compile(&p.0, esc, c).matches(&s.0, c))
        }
        _ => None,
    }
}
