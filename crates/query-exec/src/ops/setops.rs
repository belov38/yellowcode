use std::collections::HashSet;

use query_ir::ExecError;

use super::join::tuple_keys;
use super::{BoxOp, Cx, Op};
use crate::key::Key;
use crate::model::{HashModel, keys_model};
use crate::source::Row;

pub struct Distinct<'a> {
    input: BoxOp<'a>,
    seen: HashSet<Vec<Key>>,
    model: HashModel,
    held: u64,
}

impl<'a> Distinct<'a> {
    pub fn new(input: BoxOp<'a>) -> Distinct<'a> {
        Distinct {
            input,
            seen: HashSet::new(),
            model: HashModel::new(std::mem::size_of::<Vec<Key>>() as u64),
            held: 0,
        }
    }

    fn finish(&mut self, cx: &mut Cx<'a>) {
        self.seen = HashSet::new();
        self.model = HashModel::new(std::mem::size_of::<Vec<Key>>() as u64);
        cx.mem.shrink(std::mem::take(&mut self.held));
    }
}

impl<'a> Op<'a> for Distinct<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.finish(cx);
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            let k = tuple_keys(&row, row.len(), cx)?;
            if self.seen.contains(&k) {
                continue;
            }
            let kb = keys_model(&k);
            cx.mem.keep_scratch(kb)?;
            let table = self.model.growth();
            cx.mem.grow(table)?;
            self.held += kb + table;
            self.seen.insert(k);
            let old = self.model.inserted();
            if old > 0 {
                cx.mem.shrink(old);
                self.held = self.held.saturating_sub(old);
            }
            return Ok(Some(row));
        }
        self.finish(cx);
        Ok(None)
    }
}

pub struct Concat<'a> {
    parts: Vec<BoxOp<'a>>,
    cur: usize,
}

impl<'a> Concat<'a> {
    pub fn new(parts: Vec<BoxOp<'a>>) -> Concat<'a> {
        Concat { parts, cur: 0 }
    }
}

impl<'a> Op<'a> for Concat<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        for p in &mut self.parts {
            p.close(cx);
        }
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        while let Some(p) = self.parts.get_mut(self.cur) {
            if let Some(r) = p.next(cx)? {
                return Ok(Some(r));
            }
            self.cur += 1;
        }
        Ok(None)
    }
}

pub fn union<'a>(parts: Vec<BoxOp<'a>>, all: &[bool]) -> BoxOp<'a> {
    let last_distinct = all.iter().rposition(|a| !*a);
    let mut parts = parts;
    let Some(k) = last_distinct else {
        return Box::new(Concat::new(parts));
    };
    let rest = parts.split_off((k + 2).min(parts.len()));
    let head: BoxOp<'a> = Box::new(Distinct::new(Box::new(Concat::new(parts))));
    if rest.is_empty() {
        return head;
    }
    let mut flat = Vec::with_capacity(rest.len() + 1);
    flat.push(head);
    flat.extend(rest);
    Box::new(Concat::new(flat))
}
