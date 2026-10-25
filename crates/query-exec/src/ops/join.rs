use std::collections::{HashMap, HashSet};
use std::ops::Bound;

use ibschema::storage::{FieldId, IndexId, TableId};
use query_ir::{ExecError, PExpr, QValue};
use query_syntax::ast::JoinKind;

use super::access::{fetch_by_id, project_fields};
use super::{BoxOp, Cx, Op, concat, eval, fatal, nulls, rows_push_growth};
use crate::eval::{and3, not3, truth};
use crate::key::{Key, key_of};
use crate::model::{
    HashModel, block, charge_key, keys_model, row_cap_model, vec_block, vec_push_growth,
};
use crate::source::{Row, RowIter};

fn keep_row(
    rows: &[Row],
    row: &Row,
    cap: usize,
    cx: &Cx<'_>,
    held: &mut u64,
) -> Result<u64, ExecError> {
    let release = rows_push_growth(rows, cx, cap)?;
    let (grow, _) = vec_push_growth(rows.len(), cap, std::mem::size_of::<Row>());
    *held += grow;
    let n = row_cap_model(row);
    cx.mem.grow(n)?;
    *held += n;
    Ok(release)
}

fn grew(release: u64, cx: &Cx<'_>, held: &mut u64) {
    if release > 0 {
        cx.mem.shrink(release);
        *held = held.saturating_sub(release);
    }
}

fn holds(on: &Option<PExpr>, row: &[QValue], cx: &Cx<'_>) -> Result<bool, ExecError> {
    match on {
        None => Ok(true),
        Some(p) => Ok(truth(&eval(p, row, cx)?) == Some(true)),
    }
}

fn check_kind(kind: JoinKind) -> Result<(), ExecError> {
    match kind {
        JoinKind::Inner | JoinKind::Left | JoinKind::Full => Ok(()),
        JoinKind::Right => Err(fatal("ПРАВОЕ is rewritten to ЛЕВОЕ by lowering")),
    }
}

pub struct NestedLoopJoin<'a> {
    kind: JoinKind,
    l: BoxOp<'a>,
    r: BoxOp<'a>,
    on: Option<PExpr>,
    l_width: usize,
    r_width: usize,
    right: Option<Vec<Row>>,
    held: u64,
    matched: Vec<bool>,
    cur: Option<Row>,
    pos: usize,
    cur_matched: bool,
    tail: Option<usize>,
}

impl<'a> NestedLoopJoin<'a> {
    pub fn new(
        kind: JoinKind,
        l: BoxOp<'a>,
        r: BoxOp<'a>,
        on: Option<PExpr>,
        l_width: usize,
        r_width: usize,
    ) -> NestedLoopJoin<'a> {
        NestedLoopJoin {
            kind,
            l,
            r,
            on,
            l_width,
            r_width,
            right: None,
            held: 0,
            matched: Vec::new(),
            cur: None,
            pos: 0,
            cur_matched: false,
            tail: None,
        }
    }
}

impl<'a> Op<'a> for NestedLoopJoin<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.finish(cx);
        self.l.close(cx);
        self.r.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        check_kind(self.kind)?;
        if self.right.is_none() {
            let mut rows = Vec::new();
            while let Some(row) = self.r.next(cx)? {
                cx.tick()?;
                let release = keep_row(&rows, &row, rows.capacity(), cx, &mut self.held)?;
                rows.push(row);
                grew(release, cx, &mut self.held);
            }
            let flags = block(rows.len() as u64);
            cx.mem.grow(flags)?;
            self.held += flags;
            self.matched = vec![false; rows.len()];
            self.right = Some(rows);
        }
        loop {
            if let Some(p) = self.tail {
                let mut p = p;
                while let Some(rr) = self.right.as_ref().and_then(|r| r.get(p)) {
                    p += 1;
                    if !self.matched.get(p - 1).copied().unwrap_or(true) {
                        self.tail = Some(p);
                        return Ok(Some(concat(&nulls(self.l_width), rr, cx)?));
                    }
                }
                self.tail = Some(p);
                self.finish(cx);
                return Ok(None);
            }
            if self.cur.is_none() {
                match self.l.next(cx)? {
                    None => {
                        if self.kind == JoinKind::Full {
                            self.tail = Some(0);
                            continue;
                        }
                        self.finish(cx);
                        return Ok(None);
                    }
                    Some(row) => {
                        self.cur = Some(row);
                        self.pos = 0;
                        self.cur_matched = false;
                    }
                }
            }
            let Some(lrow) = self.cur.as_ref() else {
                continue;
            };
            while let Some(rr) = self.right.as_ref().and_then(|r| r.get(self.pos)) {
                cx.tick()?;
                self.pos += 1;
                let joined = concat(lrow, rr, cx)?;
                if holds(&self.on, &joined, cx)? {
                    self.cur_matched = true;
                    if let Some(m) = self.matched.get_mut(self.pos - 1) {
                        *m = true;
                    }
                    return Ok(Some(joined));
                }
            }
            let lrow = self.cur.take();
            if !self.cur_matched
                && self.kind != JoinKind::Inner
                && let Some(l) = lrow
            {
                return Ok(Some(concat(&l, &nulls(self.r_width), cx)?));
            }
        }
    }
}

impl NestedLoopJoin<'_> {
    fn finish(&mut self, cx: &mut Cx<'_>) {
        self.right = Some(Vec::new());
        self.matched = Vec::new();
        cx.mem.shrink(std::mem::take(&mut self.held));
    }
}

pub enum IndexLookup {
    Id {
        table: TableId,
        key: PExpr,
    },
    Prefix {
        table: TableId,
        index: IndexId,
        keys: Vec<PExpr>,
        skip: usize,
    },
    Temp {
        table: crate::temp::TempTableId,
        index: usize,
        keys: Vec<PExpr>,
    },
}

pub struct IndexJoin<'a> {
    kind: JoinKind,
    l: BoxOp<'a>,
    lookup: IndexLookup,
    fields: Vec<FieldId>,
    on: Option<PExpr>,
    cur: Option<Row>,
    iter: Option<RowIter<'a>>,
    cur_matched: bool,
}

impl<'a> IndexJoin<'a> {
    pub fn new(
        kind: JoinKind,
        l: BoxOp<'a>,
        lookup: IndexLookup,
        fields: Vec<FieldId>,
        on: Option<PExpr>,
    ) -> IndexJoin<'a> {
        IndexJoin {
            kind,
            l,
            lookup,
            fields,
            on,
            cur: None,
            iter: None,
            cur_matched: false,
        }
    }

    fn open(&self, lrow: &[QValue], cx: &Cx<'a>) -> Result<Option<RowIter<'a>>, ExecError> {
        match &self.lookup {
            IndexLookup::Id { table, key } => {
                let k = eval(key, lrow, cx)?;
                let found = fetch_by_id(cx, *table, &k, &self.fields)?;
                Ok(found.map(|r| -> RowIter<'a> { Box::new(std::iter::once(Ok(r))) }))
            }
            IndexLookup::Temp { table, index, keys } => {
                let t = super::temp::table_of(cx, *table)?;
                let ix = t
                    .indexes()
                    .get(*index)
                    .ok_or_else(|| fatal("temporary table index is gone"))?;
                let mut vals = Vec::with_capacity(keys.len());
                for k in keys {
                    let v = eval(k, lrow, cx)?;
                    if matches!(v, QValue::Null) {
                        return Ok(None);
                    }
                    vals.push(key_charged(&v, cx)?);
                }
                let (start, end) = ix.span(Bound::Included(&vals[..]), Bound::Included(&vals[..]));
                Ok(Some(Box::new(
                    ix.rows_in(start, end)
                        .filter_map(|r| t.rows.get(r))
                        .map(|r| Ok(r.clone())),
                )))
            }
            IndexLookup::Prefix {
                table,
                index,
                keys,
                skip,
            } => {
                let mut vals = Vec::with_capacity(keys.len());
                for k in keys {
                    let v = eval(k, lrow, cx)?;
                    if matches!(v, QValue::Null) {
                        return Ok(None);
                    }
                    vals.push(v);
                }
                let src = cx.src;
                if *skip > 0 {
                    let Some(areas) = super::access::data_areas(cx, *table, *index, *skip, false)?
                    else {
                        return Ok(Some(src.scan_fields(*table, &self.fields)?));
                    };
                    let (table, index, fields) = (*table, *index, self.fields.clone());
                    let rows = areas.into_iter().flat_map(move |area| {
                        let key: Vec<QValue> = area.iter().chain(&vals).cloned().collect();
                        match src.range_fields(
                            table,
                            index,
                            Bound::Included(&key[..]),
                            Bound::Included(&key[..]),
                            false,
                            &fields,
                        ) {
                            Ok(it) => it,
                            Err(e) => Box::new(std::iter::once(Err(e))) as RowIter<'a>,
                        }
                    });
                    return Ok(Some(Box::new(rows)));
                }
                Ok(Some(src.range_fields(
                    *table,
                    *index,
                    Bound::Included(&vals[..]),
                    Bound::Included(&vals[..]),
                    false,
                    &self.fields,
                )?))
            }
        }
    }
}

impl<'a> Op<'a> for IndexJoin<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.l.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if !matches!(self.kind, JoinKind::Inner | JoinKind::Left) {
            return Err(fatal("an index join is inner or left"));
        }
        loop {
            if self.cur.is_none() {
                let Some(row) = self.l.next(cx)? else {
                    return Ok(None);
                };
                self.iter = self.open(&row, cx)?;
                self.cur = Some(row);
                self.cur_matched = false;
            }
            let Some(lrow) = self.cur.as_ref() else {
                continue;
            };
            if let Some(it) = self.iter.as_mut() {
                for r in it.by_ref() {
                    cx.tick()?;
                    let joined = concat(lrow, &project_fields(r?, &self.fields, cx)?, cx)?;
                    if holds(&self.on, &joined, cx)? {
                        self.cur_matched = true;
                        return Ok(Some(joined));
                    }
                }
            }
            self.iter = None;
            let lrow = self.cur.take();
            if !self.cur_matched
                && self.kind == JoinKind::Left
                && let Some(l) = lrow
            {
                return Ok(Some(concat(&l, &nulls(self.fields.len()), cx)?));
            }
        }
    }
}

pub struct Swapped<'a> {
    input: BoxOp<'a>,
    vw: usize,
    on: Option<PExpr>,
}

impl<'a> Swapped<'a> {
    pub fn new(input: BoxOp<'a>, vw: usize, on: PExpr) -> Swapped<'a> {
        Swapped {
            input,
            vw,
            on: Some(on),
        }
    }
}

impl<'a> Op<'a> for Swapped<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        while let Some(row) = self.input.next(cx)? {
            let Some((values, table)) = row.split_at_checked(self.vw) else {
                return Err(fatal("a driven join row is shorter than its values"));
            };
            let out = concat(table, values, cx)?;
            if holds(&self.on, &out, cx)? {
                return Ok(Some(out));
            }
        }
        Ok(None)
    }
}

fn null_key(k: &Key) -> bool {
    k.0.as_slice() == [0]
}

fn keys_of(xs: &[PExpr], row: &[QValue], cx: &Cx<'_>) -> Result<Option<Vec<Key>>, ExecError> {
    cx.mem
        .grow_scratch(vec_block(xs.len(), std::mem::size_of::<Key>()))?;
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        let v = eval(x, row, cx)?;
        let k = key_charged(&v, cx)?;
        if null_key(&k) {
            return Ok(None);
        }
        out.push(k);
    }
    Ok(Some(out))
}

pub(crate) fn key_charged(v: &QValue, cx: &Cx<'_>) -> Result<Key, ExecError> {
    let coll = cx.env.collation;
    charge_key(v, coll, &mut |n| cx.mem.grow_scratch(n))?;
    Ok(key_of(v, coll))
}

pub(crate) const CORRELATED_TREE: u64 = 4096;

const JOIN_ENTRY: u64 =
    (std::mem::size_of::<Vec<Key>>() + std::mem::size_of::<Vec<usize>>()) as u64;

pub struct HashJoin<'a> {
    kind: JoinKind,
    l: BoxOp<'a>,
    r: BoxOp<'a>,
    l_keys: Vec<PExpr>,
    r_keys: Vec<PExpr>,
    on: Option<PExpr>,
    l_width: usize,
    r_width: usize,
    built: bool,
    rows: Vec<Row>,
    table: HashMap<Vec<Key>, Vec<usize>>,
    table_model: HashModel,
    matched: Vec<bool>,
    held: u64,
    cur: Option<Row>,
    cands: Vec<usize>,
    cands_bytes: u64,
    pos: usize,
    cur_matched: bool,
    tail: Option<usize>,
}

impl<'a> HashJoin<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: JoinKind,
        l: BoxOp<'a>,
        r: BoxOp<'a>,
        l_keys: Vec<PExpr>,
        r_keys: Vec<PExpr>,
        on: Option<PExpr>,
        l_width: usize,
        r_width: usize,
    ) -> HashJoin<'a> {
        HashJoin {
            kind,
            l,
            r,
            l_keys,
            r_keys,
            on,
            l_width,
            r_width,
            built: false,
            rows: Vec::new(),
            table: HashMap::new(),
            table_model: HashModel::new(JOIN_ENTRY),
            matched: Vec::new(),
            held: 0,
            cur: None,
            cands: Vec::new(),
            cands_bytes: 0,
            pos: 0,
            cur_matched: false,
            tail: None,
        }
    }

    fn build(&mut self, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        while let Some(row) = self.r.next(cx)? {
            cx.tick()?;
            let release = keep_row(&self.rows, &row, self.rows.capacity(), cx, &mut self.held)?;
            if let Some(k) = keys_of(&self.r_keys, &row, cx)? {
                let pos = self.rows.len();
                match self.table.get_mut(&k) {
                    Some(list) => {
                        let (grow, old) = vec_push_growth(
                            list.len(),
                            list.capacity(),
                            std::mem::size_of::<usize>(),
                        );
                        cx.mem.grow(grow)?;
                        self.held += grow;
                        list.push(pos);
                        grew(old, cx, &mut self.held);
                    }
                    None => {
                        let kb = keys_model(&k);
                        cx.mem.keep_scratch(kb)?;
                        let (list, _) = vec_push_growth(0, 0, std::mem::size_of::<usize>());
                        let table = self.table_model.growth();
                        cx.mem.grow(list + table)?;
                        self.held += kb + list + table;
                        self.table.insert(k, vec![pos]);
                        grew(self.table_model.inserted(), cx, &mut self.held);
                    }
                }
            }
            self.rows.push(row);
            grew(release, cx, &mut self.held);
        }
        let m = block(self.rows.len() as u64);
        cx.mem.grow(m)?;
        self.held += m;
        self.matched = vec![false; self.rows.len()];
        self.built = true;
        Ok(())
    }

    fn drop_cands(&mut self, cx: &mut Cx<'a>) {
        self.cands = Vec::new();
        cx.mem.shrink(std::mem::take(&mut self.cands_bytes));
    }

    fn finish(&mut self, cx: &mut Cx<'a>) {
        self.drop_cands(cx);
        self.rows = Vec::new();
        self.table = HashMap::new();
        self.matched = Vec::new();
        cx.mem.shrink(std::mem::take(&mut self.held));
    }
}

impl<'a> Op<'a> for HashJoin<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.finish(cx);
        self.l.close(cx);
        self.r.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        check_kind(self.kind)?;
        if !self.built {
            self.build(cx)?;
        }
        loop {
            if let Some(mut p) = self.tail {
                while let Some(rr) = self.rows.get(p) {
                    p += 1;
                    if !self.matched.get(p - 1).copied().unwrap_or(true) {
                        self.tail = Some(p);
                        return Ok(Some(concat(&nulls(self.l_width), rr, cx)?));
                    }
                }
                self.tail = Some(p);
                self.finish(cx);
                return Ok(None);
            }
            if self.cur.is_none() {
                match self.l.next(cx)? {
                    None => {
                        if self.kind == JoinKind::Full {
                            self.tail = Some(0);
                            continue;
                        }
                        self.finish(cx);
                        return Ok(None);
                    }
                    Some(row) => {
                        cx.tick()?;
                        self.drop_cands(cx);
                        let found = match keys_of(&self.l_keys, &row, cx)? {
                            Some(k) => self.table.get(&k),
                            None => None,
                        };
                        if let Some(list) = found {
                            let n = vec_block(list.len(), std::mem::size_of::<usize>());
                            cx.mem.grow(n)?;
                            self.cands_bytes = n;
                            self.cands = list.clone();
                        }
                        self.cur = Some(row);
                        self.pos = 0;
                        self.cur_matched = false;
                    }
                }
            }
            let Some(lrow) = self.cur.as_ref() else {
                continue;
            };
            while let Some(&ri) = self.cands.get(self.pos) {
                self.pos += 1;
                cx.tick()?;
                let Some(rr) = self.rows.get(ri) else {
                    continue;
                };
                let joined = concat(lrow, rr, cx)?;
                if holds(&self.on, &joined, cx)? {
                    self.cur_matched = true;
                    if let Some(m) = self.matched.get_mut(ri) {
                        *m = true;
                    }
                    return Ok(Some(joined));
                }
            }
            let lrow = self.cur.take();
            if !self.cur_matched
                && self.kind != JoinKind::Inner
                && let Some(l) = lrow
            {
                return Ok(Some(concat(&l, &nulls(self.r_width), cx)?));
            }
        }
    }
}

pub(crate) fn in3(l: &[Key], q: &[Key]) -> Option<bool> {
    and3(l.iter().zip(q).map(|(a, b)| {
        if null_key(a) || null_key(b) {
            None
        } else {
            Some(a == b)
        }
    }))
}

pub(crate) struct KeySet {
    plain: HashSet<Vec<Key>>,
    nulls: HashSet<Vec<Key>>,
    plain_model: HashModel,
    nulls_model: HashModel,
}

impl Default for KeySet {
    fn default() -> Self {
        let kv = std::mem::size_of::<Vec<Key>>() as u64;
        KeySet {
            plain: HashSet::new(),
            nulls: HashSet::new(),
            plain_model: HashModel::new(kv),
            nulls_model: HashModel::new(kv),
        }
    }
}

impl KeySet {
    pub(crate) fn is_empty(&self) -> bool {
        self.plain.is_empty() && self.nulls.is_empty()
    }

    pub(crate) fn probe(&self, l: &[Key]) -> Option<bool> {
        crate::probe::hit();
        if self.is_empty() {
            return Some(false);
        }
        let l_null = l.iter().any(null_key);
        if l_null && l.len() == 1 {
            return None;
        }
        if !l_null && self.plain.contains(l) {
            return Some(true);
        }
        let mut res = Some(false);
        let candidates: Box<dyn Iterator<Item = &Vec<Key>> + '_> = if l_null {
            Box::new(self.plain.iter().chain(self.nulls.iter()))
        } else {
            Box::new(self.nulls.iter())
        };
        for t in candidates {
            crate::probe::hit();
            match in3(l, t) {
                Some(true) => return Some(true),
                None => res = None,
                Some(false) => {}
            }
        }
        res
    }
}

enum Sub<'a> {
    Hashed { q: Option<BoxOp<'a>>, set: KeySet },
    Correlated(Box<dyn FnMut() -> BoxOp<'a> + 'a>),
}

pub struct SemiJoin<'a> {
    input: BoxOp<'a>,
    l: Vec<PExpr>,
    not: bool,
    sub: Sub<'a>,
    held: u64,
}

impl<'a> SemiJoin<'a> {
    pub fn hashed(input: BoxOp<'a>, l: Vec<PExpr>, q: BoxOp<'a>, not: bool) -> SemiJoin<'a> {
        SemiJoin {
            input,
            l,
            not,
            sub: Sub::Hashed {
                q: Some(q),
                set: KeySet::default(),
            },
            held: 0,
        }
    }

    pub fn correlated(
        input: BoxOp<'a>,
        l: Vec<PExpr>,
        make: Box<dyn FnMut() -> BoxOp<'a> + 'a>,
        not: bool,
    ) -> SemiJoin<'a> {
        SemiJoin {
            input,
            l,
            not,
            sub: Sub::Correlated(make),
            held: 0,
        }
    }
}

pub(crate) fn tuple_keys(row: &[QValue], width: usize, cx: &Cx<'_>) -> Result<Vec<Key>, ExecError> {
    let n = row.len().min(width);
    cx.mem
        .grow_scratch(vec_block(n, std::mem::size_of::<Key>()))?;
    let mut out = Vec::with_capacity(n);
    for v in row.iter().take(width) {
        out.push(key_charged(v, cx)?);
    }
    Ok(out)
}

impl<'a> SemiJoin<'a> {
    fn build(&mut self, width: usize, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        if let Sub::Hashed { q, set } = &mut self.sub
            && let Some(q) = q.take()
        {
            self.held += set.fill(q, width, cx)?;
        }
        Ok(())
    }
}

impl KeySet {
    pub(crate) fn fill<'a>(
        &mut self,
        mut q: BoxOp<'a>,
        width: usize,
        cx: &mut Cx<'a>,
    ) -> Result<u64, ExecError> {
        let mut held = 0;
        while let Some(row) = q.next(cx)? {
            cx.tick()?;
            crate::probe::hit();
            if row.len() < width {
                return Err(fatal("a subquery with fewer columns than the left side"));
            }
            let k = tuple_keys(&row, width, cx)?;
            let (target, model) = if k.iter().any(null_key) {
                (&mut self.nulls, &mut self.nulls_model)
            } else {
                (&mut self.plain, &mut self.plain_model)
            };
            if target.contains(&k) {
                continue;
            }
            let kb = keys_model(&k);
            cx.mem.keep_scratch(kb)?;
            let table = model.growth();
            cx.mem.grow(table)?;
            held += kb + table;
            target.insert(k);
            let old = model.inserted();
            if old > 0 {
                cx.mem.shrink(old);
                held = held.saturating_sub(old);
            }
        }
        Ok(held)
    }
}

impl<'a> Op<'a> for SemiJoin<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
        if let Sub::Hashed { q: Some(q), .. } = &mut self.sub {
            q.close(cx);
        }
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let width = self.l.len();
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            self.build(width, cx)?;
            cx.mem
                .grow_scratch(vec_block(width, std::mem::size_of::<Key>()))?;
            let mut lv = Vec::with_capacity(width);
            for x in &self.l {
                let v = eval(x, &row, cx)?;
                lv.push(key_charged(&v, cx)?);
            }
            let found = match &mut self.sub {
                Sub::Hashed { set, .. } => set.probe(&lv),
                Sub::Correlated(make) => {
                    cx.mem.grow_scratch(CORRELATED_TREE)?;
                    let mut q = make();
                    let copy = row_cap_model(&row);
                    cx.mem.grow(copy)?;
                    cx.outer.push(row.clone());
                    let mark = cx.mem.mark();
                    let r = run_correlated(&mut q, &lv, width, cx);
                    q.close(cx);
                    cx.outer.pop();
                    drop(q);
                    cx.mem.reset_to(mark);
                    cx.mem.shrink(copy);
                    r?
                }
            };
            let keep = if self.not { not3(found) } else { found };
            if keep == Some(true) {
                return Ok(Some(row));
            }
        }
        cx.mem.shrink(std::mem::take(&mut self.held));
        Ok(None)
    }
}

pub(crate) fn run_correlated<'a>(
    q: &mut BoxOp<'a>,
    lv: &[Key],
    width: usize,
    cx: &mut Cx<'a>,
) -> Result<Option<bool>, ExecError> {
    let mut res = Some(false);
    while let Some(r) = q.next(cx)? {
        cx.tick()?;
        let t = tuple_keys(&r, width, cx)?;
        match in3(lv, &t) {
            Some(true) => return Ok(Some(true)),
            None => res = None,
            Some(false) => {}
        }
    }
    Ok(res)
}
