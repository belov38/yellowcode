use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Bound;
use std::sync::Arc;

use ibschema::storage::IndexId;
use query_ir::{ExecError, HierTable, PExpr, QValue};

use super::join::key_charged;
use super::sort::{SKey, desc_flags};
use super::{BoxOp, Cx, Op, fatal};
use crate::key::Key;
use crate::model::{
    HashModel, block, hash_buckets_for, hash_table_model, key_model, row_cap_model, value_model,
    vec_block, vec_push_growth,
};
use crate::source::Row;

const NODE_WALK: u64 = 256;

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

pub struct HierarchyOrder<'a> {
    input: BoxOp<'a>,
    id: u16,
    parent: u16,
    keys: Vec<(PExpr, bool)>,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
}

impl<'a> HierarchyOrder<'a> {
    pub fn new(
        input: BoxOp<'a>,
        id: u16,
        parent: u16,
        keys: Vec<(PExpr, bool)>,
    ) -> HierarchyOrder<'a> {
        HierarchyOrder {
            input,
            id,
            parent,
            keys,
            out: None,
            held: 0,
        }
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let desc: Arc<[bool]> = desc_flags(&self.keys);
        let mut rows: Vec<Option<Row>> = Vec::new();
        let mut skeys: Vec<SKey> = Vec::new();
        let mut ids: Vec<Key> = Vec::new();
        let mut parents: Vec<Option<Key>> = Vec::new();
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            let idv = row
                .get(usize::from(self.id))
                .ok_or_else(|| fatal("hierarchy id column out of range"))?;
            let idk = key_charged(idv, cx)?;
            let pv = row
                .get(usize::from(self.parent))
                .ok_or_else(|| fatal("hierarchy parent column out of range"))?;
            let pk = if matches!(pv, query_ir::QValue::Null) {
                None
            } else {
                Some(key_charged(pv, cx)?)
            };
            let sk = SKey::of(&self.keys, &desc, &row, cx)?;
            let kb = key_model(&idk) + pk.as_ref().map_or(0, key_model) + sk.bytes();
            cx.mem.keep_scratch(kb)?;
            let rb = row_cap_model(&row);
            cx.mem.grow(rb)?;
            self.held += kb + rb;
            let mut old = 0;
            old += push_growth(
                ids.len(),
                ids.capacity(),
                std::mem::size_of::<Key>(),
                cx,
                &mut self.held,
            )?;
            ids.push(idk);
            old += push_growth(
                parents.len(),
                parents.capacity(),
                std::mem::size_of::<Option<Key>>(),
                cx,
                &mut self.held,
            )?;
            parents.push(pk);
            old += push_growth(
                skeys.len(),
                skeys.capacity(),
                std::mem::size_of::<SKey>(),
                cx,
                &mut self.held,
            )?;
            skeys.push(sk);
            old += push_growth(
                rows.len(),
                rows.capacity(),
                std::mem::size_of::<Option<Row>>(),
                cx,
                &mut self.held,
            )?;
            rows.push(Some(row));
            release(old, cx, &mut self.held);
        }
        let n = rows.len();
        let aux = hash_table_model(hash_buckets_for(n), 16)
            + vec_block(n, std::mem::size_of::<Vec<usize>>())
            + block(48 * n as u64)
            + 3 * vec_block(n, std::mem::size_of::<usize>())
            + vec_block(n, std::mem::size_of::<Row>());
        cx.mem.grow(aux)?;
        self.held += aux;
        let mut by_id: HashMap<&Key, usize> = HashMap::with_capacity(n);
        for (i, k) in ids.iter().enumerate() {
            by_id.entry(k).or_insert(i);
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut roots: Vec<usize> = Vec::with_capacity(n);
        for (i, p) in parents.iter().enumerate() {
            match p.as_ref().and_then(|k| by_id.get(k)) {
                Some(&pi) if pi != i => {
                    if let Some(c) = children.get_mut(pi) {
                        c.push(i);
                    }
                }
                _ => roots.push(i),
            }
        }
        let by_key = |a: &usize, b: &usize| skeys.get(*a).cmp(&skeys.get(*b));
        roots.sort_by(by_key);
        for c in &mut children {
            c.sort_by(by_key);
        }
        let mut out = Vec::with_capacity(n);
        let mut stack: Vec<usize> = Vec::with_capacity(n);
        stack.extend(roots.into_iter().rev());
        while let Some(i) = stack.pop() {
            cx.tick()?;
            if let Some(r) = rows.get_mut(i).and_then(Option::take) {
                out.push(r);
                if let Some(c) = children.get(i) {
                    stack.extend(c.iter().rev());
                }
            }
        }
        out.extend(rows.into_iter().flatten());
        drop(by_id);
        drop((children, stack, ids, parents, skeys));
        let rows_bytes: u64 = out.iter().map(row_cap_model).sum::<u64>()
            + vec_block(out.capacity(), std::mem::size_of::<Row>());
        let gone = self.held.saturating_sub(rows_bytes);
        release(gone, cx, &mut self.held);
        Ok(out)
    }
}

impl<'a> Op<'a> for HierarchyOrder<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let v = self.run(cx)?;
            self.out = Some(v.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(r) => {
                let n = row_cap_model(&r);
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

pub enum Seeds<'a> {
    Values(Vec<PExpr>),
    Rel(Option<BoxOp<'a>>),
}

pub struct HierarchyClosure<'a> {
    tables: Vec<HierTable>,
    seeds: Seeds<'a>,
    use_index: bool,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
}

enum Children {
    Index(IndexId),
    Map(HashMap<Key, Vec<QValue>>),
}

impl<'a> HierarchyClosure<'a> {
    pub fn new(tables: Vec<HierTable>, use_index: bool, seeds: Seeds<'a>) -> HierarchyClosure<'a> {
        HierarchyClosure {
            tables,
            seeds,
            use_index,
            out: None,
            held: 0,
        }
    }

    fn seed_values(&mut self, cx: &mut Cx<'a>, held: &mut u64) -> Result<Vec<QValue>, ExecError> {
        let mut out = Vec::new();
        match &mut self.seeds {
            Seeds::Values(xs) => {
                for x in xs.iter() {
                    match x {
                        PExpr::Set(s) => {
                            let n = vec_block(s.values.len(), crate::model::VALUE_SIZE)
                                + s.values.iter().map(value_model).sum::<u64>();
                            cx.mem.grow(n)?;
                            *held += n;
                            out.extend(s.values.iter().cloned());
                        }
                        e => {
                            let v = super::eval(e, &[], cx)?;
                            let old = push_growth(
                                out.len(),
                                out.capacity(),
                                crate::model::VALUE_SIZE,
                                cx,
                                held,
                            )?;
                            out.push(v);
                            release(old, cx, held);
                        }
                    }
                }
            }
            Seeds::Rel(q) => {
                if let Some(mut q) = q.take() {
                    while let Some(row) = q.next(cx)? {
                        cx.tick()?;
                        let v = row
                            .into_iter()
                            .next()
                            .ok_or_else(|| fatal("a hierarchy seed without a column"))?;
                        let n = value_model(&v);
                        cx.mem.grow(n)?;
                        *held += n;
                        let old = push_growth(
                            out.len(),
                            out.capacity(),
                            crate::model::VALUE_SIZE,
                            cx,
                            held,
                        )?;
                        out.push(v);
                        release(old, cx, held);
                    }
                }
            }
        }
        Ok(out)
    }

    fn index_of(&self, t: &HierTable, cx: &Cx<'_>) -> Option<IndexId> {
        if !self.use_index {
            return None;
        }
        let info = cx.src.map().try_table(t.table)?;
        let k = info
            .indexes
            .iter()
            .position(|ix| ix.prefix == 0 && ix.fields.first() == Some(&t.parent))?;
        Some(IndexId::Named(u16::try_from(k).ok()?))
    }

    fn children_map(
        &mut self,
        t: &HierTable,
        cx: &mut Cx<'a>,
        held: &mut u64,
    ) -> Result<HashMap<Key, Vec<QValue>>, ExecError> {
        let mut map: HashMap<Key, Vec<QValue>> = HashMap::new();
        let mut model = HashModel::new(
            (std::mem::size_of::<Key>() + std::mem::size_of::<Vec<QValue>>()) as u64,
        );
        let src = cx.src;
        let fields = [t.id, t.parent];
        for row in src.scan_fields(t.table, &fields)? {
            cx.tick()?;
            let row = row?;
            let (Some(id), Some(parent)) = (
                row.get(usize::from(t.id.0)),
                row.get(usize::from(t.parent.0)),
            ) else {
                return Err(fatal("hierarchy column out of range"));
            };
            let pk = key_charged(parent, cx)?;
            let n = value_model(id);
            cx.mem.grow(n)?;
            *held += n;
            match map.get_mut(&pk) {
                Some(list) => {
                    let old = push_growth(
                        list.len(),
                        list.capacity(),
                        crate::model::VALUE_SIZE,
                        cx,
                        held,
                    )?;
                    list.push(id.clone());
                    release(old, cx, held);
                }
                None => {
                    let kb = key_model(&pk);
                    cx.mem.keep_scratch(kb)?;
                    let (list, _) = vec_push_growth(0, 0, crate::model::VALUE_SIZE);
                    let table = model.growth();
                    cx.mem.grow(list + table)?;
                    *held += kb + list + table;
                    map.insert(pk, vec![id.clone()]);
                    release(model.inserted(), cx, held);
                }
            }
        }
        Ok(map)
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let mut held = 0u64;
        let r = self.run_inner(cx, &mut held);
        self.held += held;
        r
    }

    fn run_inner(&mut self, cx: &mut Cx<'a>, held: &mut u64) -> Result<Vec<Row>, ExecError> {
        let seeds = self.seed_values(cx, held)?;
        let tables = self.tables.clone();
        let mut children: Vec<Option<Children>> = tables.iter().map(|_| None).collect();
        let mut seen: HashSet<Key> = HashSet::new();
        let mut seen_model = HashModel::new(std::mem::size_of::<Key>() as u64);
        let mut out: Vec<Row> = Vec::new();
        let mut add = |v: &QValue, cx: &mut Cx<'a>, held: &mut u64| -> Result<bool, ExecError> {
            let k = key_charged(v, cx)?;
            if seen.contains(&k) {
                return Ok(false);
            }
            let kb = key_model(&k);
            cx.mem.keep_scratch(kb)?;
            let n = value_model(v)
                + vec_block(1, crate::model::VALUE_SIZE)
                + seen_model.growth()
                + NODE_WALK;
            cx.mem.grow(n)?;
            *held += kb + n;
            seen.insert(k);
            release(seen_model.inserted(), cx, held);
            let old = push_growth(
                out.len(),
                out.capacity(),
                std::mem::size_of::<Row>(),
                cx,
                held,
            )?;
            out.push(vec![v.clone()]);
            release(old, cx, held);
            Ok(true)
        };
        for seed in &seeds {
            cx.tick()?;
            let slot = match seed {
                QValue::Null => continue,
                QValue::Ref { table, id } => tables
                    .iter()
                    .position(|t| t.number == *table)
                    .map(|i| (i, id)),
                _ => None,
            };
            let Some((ti, id)) = slot else {
                add(seed, cx, held)?;
                continue;
            };
            let Some(t) = tables.get(ti) else { continue };
            let src = cx.src;
            if id.iter().all(|b| *b == 0) {
                for row in src.scan_fields(t.table, &[t.id])? {
                    cx.tick()?;
                    let row = row?;
                    let v = row
                        .get(usize::from(t.id.0))
                        .ok_or_else(|| fatal("hierarchy id column out of range"))?;
                    add(v, cx, held)?;
                }
                continue;
            }
            if src.get_by_id_fields(t.table, seed, &[t.id])?.is_none() {
                continue;
            }
            if !add(seed, cx, held)? {
                continue;
            }
            if children.get(ti).is_some_and(Option::is_none) {
                let c = match self.index_of(t, cx) {
                    Some(i) => Children::Index(i),
                    None => Children::Map(self.children_map(t, cx, held)?),
                };
                if let Some(slot) = children.get_mut(ti) {
                    *slot = Some(c);
                }
            }
            let mut queue: VecDeque<QValue> = VecDeque::new();
            queue.push_back(seed.clone());
            while let Some(node) = queue.pop_front() {
                cx.tick()?;
                let mut kids: Vec<QValue> = Vec::new();
                match children.get(ti).and_then(Option::as_ref) {
                    Some(Children::Index(ix)) => {
                        let key = [node.clone()];
                        let it = src.range_fields(
                            t.table,
                            *ix,
                            Bound::Included(&key[..]),
                            Bound::Included(&key[..]),
                            false,
                            &[t.id],
                        )?;
                        for row in it {
                            cx.tick()?;
                            if let Some(v) = row?.get(usize::from(t.id.0)) {
                                let old = push_growth(
                                    kids.len(),
                                    kids.capacity(),
                                    crate::model::VALUE_SIZE,
                                    cx,
                                    held,
                                )?;
                                kids.push(v.clone());
                                release(old, cx, held);
                            }
                        }
                    }
                    Some(Children::Map(m)) => {
                        if let Some(v) = m.get(&key_charged(&node, cx)?) {
                            let n = vec_block(v.len(), crate::model::VALUE_SIZE);
                            cx.mem.grow(n)?;
                            *held += n;
                            kids.extend(v.iter().cloned());
                        }
                    }
                    None => {}
                }
                let kids_bytes = vec_block(kids.capacity(), crate::model::VALUE_SIZE);
                for k in kids {
                    if add(&k, cx, held)? {
                        let old = push_growth(
                            queue.len(),
                            queue.capacity(),
                            crate::model::VALUE_SIZE,
                            cx,
                            held,
                        )?;
                        queue.push_back(k);
                        release(old, cx, held);
                    }
                }
                release(kids_bytes, cx, held);
            }
            let q = vec_block(queue.capacity(), crate::model::VALUE_SIZE);
            drop(queue);
            release(q, cx, held);
        }
        Ok(out)
    }
}

impl<'a> Op<'a> for HierarchyClosure<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        if let Seeds::Rel(Some(q)) = &mut self.seeds {
            q.close(cx);
        }
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let v = self.run(cx)?;
            self.out = Some(v.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(r) => Ok(Some(r)),
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}
