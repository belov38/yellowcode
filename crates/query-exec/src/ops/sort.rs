use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use query_ir::{ExecError, PExpr};

use super::join::key_charged;
use super::{BoxOp, Cx, Op, eval};
use crate::eval::rank;
use crate::key::Key;
use crate::model::{key_model, row_cap_model, vec_block, vec_push_growth};
use crate::source::Row;

#[derive(Clone, Debug)]
pub struct SKey {
    items: Vec<(u8, Key)>,
    desc: Arc<[bool]>,
}

impl SKey {
    pub(crate) fn of(
        keys: &[(PExpr, bool)],
        desc: &Arc<[bool]>,
        row: &[query_ir::QValue],
        cx: &Cx<'_>,
    ) -> Result<SKey, ExecError> {
        cx.mem
            .grow_scratch(vec_block(keys.len(), std::mem::size_of::<(u8, Key)>()))?;
        let mut items = Vec::with_capacity(keys.len());
        for (e, _) in keys {
            let v = eval(e, row, cx)?;
            let k = key_charged(&v, cx)?;
            items.push((rank(&v), k));
        }
        Ok(SKey {
            items,
            desc: desc.clone(),
        })
    }

    pub(crate) fn bytes(&self) -> u64 {
        vec_block(self.items.capacity(), std::mem::size_of::<(u8, Key)>())
            + self.items.iter().map(|(_, k)| key_model(k)).sum::<u64>()
    }
}

impl PartialEq for SKey {
    fn eq(&self, o: &SKey) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for SKey {}
impl PartialOrd for SKey {
    fn partial_cmp(&self, o: &SKey) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for SKey {
    fn cmp(&self, o: &SKey) -> Ordering {
        for (i, (a, b)) in self.items.iter().zip(&o.items).enumerate() {
            let c = a.0.cmp(&b.0).then_with(|| {
                if a.0 <= 1 {
                    Ordering::Equal
                } else {
                    a.1.cmp(&b.1)
                }
            });
            if c != Ordering::Equal {
                return if self.desc.get(i).copied().unwrap_or(false) {
                    c.reverse()
                } else {
                    c
                };
            }
        }
        Ordering::Equal
    }
}

pub(crate) fn desc_flags(keys: &[(PExpr, bool)]) -> Arc<[bool]> {
    keys.iter().map(|k| k.1).collect()
}

fn push_growth(
    len: usize,
    cap: usize,
    elem: usize,
    cx: &Cx<'_>,
    held: &mut u64,
) -> Result<u64, ExecError> {
    let (grow, release) = vec_push_growth(len, cap, elem);
    if grow > 0 {
        cx.mem.grow(grow)?;
        *held += grow;
    }
    Ok(release)
}

fn release(n: u64, cx: &Cx<'_>, held: &mut u64) {
    if n > 0 {
        cx.mem.shrink(n);
        *held = held.saturating_sub(n);
    }
}

fn entry_bytes(row: &Row, key: &SKey, cx: &Cx<'_>) -> Result<u64, ExecError> {
    let kb = key.bytes();
    cx.mem.keep_scratch(kb)?;
    let rb = row_cap_model(row);
    cx.mem.grow(rb)?;
    Ok(rb + kb)
}

pub struct Sort<'a> {
    input: BoxOp<'a>,
    keys: Vec<(PExpr, bool)>,
    out: Option<std::vec::IntoIter<(Row, u64)>>,
    held: u64,
}

impl<'a> Sort<'a> {
    pub fn new(input: BoxOp<'a>, keys: Vec<(PExpr, bool)>) -> Sort<'a> {
        Sort {
            input,
            keys,
            out: None,
            held: 0,
        }
    }

    fn fill(&mut self, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        let desc = desc_flags(&self.keys);
        let entry = std::mem::size_of::<(SKey, Row, u64)>();
        let mut buf: Vec<(SKey, Row, u64)> = Vec::new();
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            let k = SKey::of(&self.keys, &desc, &row, cx)?;
            let n = entry_bytes(&row, &k, cx)?;
            self.held += n;
            let old = push_growth(buf.len(), buf.capacity(), entry, cx, &mut self.held)?;
            buf.push((k, row, n));
            release(old, cx, &mut self.held);
        }
        let scratch = vec_block(buf.len(), entry);
        cx.mem.grow(scratch)?;
        buf.sort_by(|a, b| a.0.cmp(&b.0));
        cx.mem.shrink(scratch);
        let out_elem = std::mem::size_of::<(Row, u64)>();
        let out_bytes = vec_block(buf.len(), out_elem);
        cx.mem.grow(out_bytes)?;
        self.held += out_bytes;
        let entries = vec_block(buf.capacity(), entry);
        let v: Vec<(Row, u64)> = buf
            .into_iter()
            .map(|(k, r, n)| (r, n - k.bytes()))
            .collect();
        release(entries, cx, &mut self.held);
        let keys_gone = self
            .held
            .saturating_sub(out_bytes + v.iter().map(|e| e.1).sum::<u64>());
        release(keys_gone, cx, &mut self.held);
        self.out = Some(v.into_iter());
        Ok(())
    }
}

impl<'a> Op<'a> for Sort<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            self.fill(cx)?;
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some((r, n)) => {
                cx.mem.shrink(n);
                self.held = self.held.saturating_sub(n);
                Ok(Some(r))
            }
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}

struct Entry {
    key: SKey,
    seq: u64,
    row: Row,
    bytes: u64,
}
impl PartialEq for Entry {
    fn eq(&self, o: &Entry) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Entry) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Entry {
    fn cmp(&self, o: &Entry) -> Ordering {
        self.key.cmp(&o.key).then(self.seq.cmp(&o.seq))
    }
}

pub struct TopN<'a> {
    input: BoxOp<'a>,
    keys: Vec<(PExpr, bool)>,
    n: u64,
    out: Option<std::vec::IntoIter<Entry>>,
    held: u64,
}

impl<'a> TopN<'a> {
    pub fn new(input: BoxOp<'a>, keys: Vec<(PExpr, bool)>, n: u64) -> TopN<'a> {
        TopN {
            input,
            keys,
            n,
            out: None,
            held: 0,
        }
    }

    fn fill(&mut self, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        let desc = desc_flags(&self.keys);
        let cap = usize::try_from(self.n).unwrap_or(usize::MAX);
        let entry = std::mem::size_of::<Entry>();
        let mut heap: BinaryHeap<Entry> = BinaryHeap::new();
        let mut seq = 0u64;
        while let Some(row) = if cap > 0 { self.input.next(cx)? } else { None } {
            cx.tick()?;
            let key = SKey::of(&self.keys, &desc, &row, cx)?;
            seq += 1;
            if heap.len() >= cap {
                if heap.peek().is_some_and(|w| key >= w.key) {
                    continue;
                }
                if let Some(w) = heap.pop() {
                    let gone = w.bytes;
                    drop(w);
                    release(gone, cx, &mut self.held);
                }
            }
            let bytes = entry_bytes(&row, &key, cx)?;
            self.held += bytes;
            let old = push_growth(heap.len(), heap.capacity(), entry, cx, &mut self.held)?;
            heap.push(Entry {
                key,
                seq,
                row,
                bytes,
            });
            release(old, cx, &mut self.held);
        }
        self.out = Some(heap.into_sorted_vec().into_iter());
        Ok(())
    }
}

impl<'a> Op<'a> for TopN<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            self.fill(cx)?;
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(e) => {
                let Entry {
                    key, row, bytes, ..
                } = e;
                drop(key);
                cx.mem.shrink(bytes);
                self.held = self.held.saturating_sub(bytes);
                Ok(Some(row))
            }
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}
