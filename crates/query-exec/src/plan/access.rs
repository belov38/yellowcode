use std::collections::{HashSet, VecDeque};
use std::ops::Bound;

use ibschema::logical::TypeCode;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, FieldInfo, IndexId, IndexInfo, TableId, TableInfo};
use query_ir::{ExecError, PExpr, QValue};
use query_syntax::ast::CmpOp;

use super::{Force, Plan, Planner, fatal, wrap};
use crate::key::Key;
use crate::ops::access::{fetch_by_id, project_fields};
use crate::ops::{Cx, Op, eval};
use crate::source::{Row, RowIter};

#[derive(Default)]
struct Cons {
    eq: Option<PExpr>,
    list: Option<Vec<PExpr>>,
    lo: Option<(PExpr, bool)>,
    hi: Option<(PExpr, bool)>,
}

pub(super) fn is_null(v: &QValue) -> bool {
    matches!(v, QValue::Null)
}

pub(super) fn eq_const(e: &PExpr) -> bool {
    match e {
        PExpr::Lit(v) => !is_null(v),
        PExpr::Outer { .. } => true,
        _ => false,
    }
}

fn range_const(e: &PExpr, f: &FieldInfo) -> bool {
    let PExpr::Lit(v) = e else { return false };
    let [t] = f.types.as_slice() else {
        return false;
    };
    matches!(
        (t.code, v),
        (TypeCode::N, QValue::Number(_))
            | (TypeCode::S, QValue::String(_))
            | (TypeCode::T, QValue::Date(_))
            | (TypeCode::L, QValue::Bool(_))
            | (TypeCode::R, QValue::Ref { .. })
    )
}

pub(super) fn flip(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Ge => CmpOp::Le,
        o => o,
    }
}

fn list_items(rhs: &[PExpr]) -> Option<Vec<PExpr>> {
    let mut out = Vec::new();
    for e in rhs {
        match e {
            PExpr::Lit(v) if !is_null(v) => out.push(e.clone()),
            PExpr::Lit(_) => {}
            PExpr::Set(_) => out.push(e.clone()),
            _ => return None,
        }
    }
    Some(out)
}

fn constraints(info: &TableInfo, fields: &[FieldId], conj: &[PExpr]) -> Vec<(FieldId, Cons)> {
    let mut out: Vec<(FieldId, Cons)> = Vec::new();
    let field = |e: &PExpr| match e {
        PExpr::Col(c) => fields.get(usize::from(*c)).copied(),
        _ => None,
    };
    fn slot(out: &mut Vec<(FieldId, Cons)>, f: FieldId) -> Option<&mut Cons> {
        if !out.iter().any(|(g, _)| *g == f) {
            out.push((f, Cons::default()));
        }
        out.iter_mut().find(|(g, _)| *g == f).map(|(_, c)| c)
    }
    for c in conj {
        match c {
            PExpr::Compare(op, a, b) => {
                let (f, op, k) = match (field(a), field(b)) {
                    (Some(f), None) => (f, *op, &**b),
                    (None, Some(f)) => (f, flip(*op), &**a),
                    _ => continue,
                };
                let Some(fi) = info.field(f) else { continue };
                let Some(s) = slot(&mut out, f) else { continue };
                match op {
                    CmpOp::Eq if eq_const(k) => {
                        s.eq.get_or_insert_with(|| k.clone());
                    }
                    CmpOp::Gt if range_const(k, fi) => {
                        s.lo.get_or_insert((k.clone(), false));
                    }
                    CmpOp::Ge if range_const(k, fi) => {
                        s.lo.get_or_insert((k.clone(), true));
                    }
                    CmpOp::Lt if range_const(k, fi) => {
                        s.hi.get_or_insert((k.clone(), false));
                    }
                    CmpOp::Le if range_const(k, fi) => {
                        s.hi.get_or_insert((k.clone(), true));
                    }
                    _ => {}
                }
            }
            PExpr::InList { l, not: false, rhs } if l.len() == 1 => {
                if let (Some(f), Some(items)) = (l.first().and_then(field), list_items(rhs))
                    && let Some(s) = slot(&mut out, f)
                {
                    s.list.get_or_insert(items);
                }
            }
            PExpr::Between {
                l,
                not: false,
                lo,
                hi,
            } => {
                if let Some(f) = field(l)
                    && let Some(fi) = info.field(f)
                    && range_const(lo, fi)
                    && range_const(hi, fi)
                {
                    let Some(s) = slot(&mut out, f) else { continue };
                    s.lo.get_or_insert(((**lo).clone(), true));
                    s.hi.get_or_insert(((**hi).clone(), true));
                }
            }
            _ => {}
        }
    }
    out
}

pub(super) fn unique_id_key(info: &TableInfo) -> Option<&IndexInfo> {
    let id = info.field_by_std(StdField::Id)?;
    info.key_index
        .as_ref()
        .filter(|k| k.fields.get(k.prefix..) == Some(&[id][..]))
}

fn with_last(prefix: &[PExpr], last: &PExpr) -> Vec<PExpr> {
    let mut v = prefix.to_vec();
    v.push(last.clone());
    v
}

impl Planner<'_> {
    fn id_field(info: &TableInfo) -> Option<FieldId> {
        info.field_by_std(StdField::Id)
    }

    pub(super) fn access(
        &self,
        table: TableId,
        fields: &[FieldId],
        rep: bool,
        pending: Vec<PExpr>,
    ) -> Result<Plan, ExecError> {
        let info = self
            .map
            .try_table(table)
            .ok_or_else(|| fatal("unknown table"))?;
        let name = self.table_name(table);
        let chosen = if pending.is_empty() || !self.use_index() {
            None
        } else {
            let cons = constraints(info, fields, &pending);
            self.by_id(info, &cons, table, &name, fields)
                .or_else(|| self.by_index(info, &cons, table, &name, fields))
        };
        let plan = match chosen {
            Some(p) => p,
            None => self.plain(info, table, name, fields, rep),
        };
        Ok(wrap(plan, pending))
    }

    fn by_id(
        &self,
        info: &TableInfo,
        cons: &[(FieldId, Cons)],
        table: TableId,
        name: &str,
        fields: &[FieldId],
    ) -> Option<Plan> {
        unique_id_key(info).filter(|k| k.prefix == 0)?;
        let id = Self::id_field(info)?;
        let (_, c) = cons.iter().find(|(f, _)| *f == id)?;
        let ids = match (&c.eq, &c.list) {
            (Some(e), _) => vec![e.clone()],
            (None, Some(items)) => items.clone(),
            _ => return None,
        };
        Some(Plan::IdLookup {
            table,
            name: name.to_string(),
            ids,
            fields: fields.to_vec(),
        })
    }

    fn by_index(
        &self,
        info: &TableInfo,
        cons: &[(FieldId, Cons)],
        table: TableId,
        name: &str,
        fields: &[FieldId],
    ) -> Option<Plan> {
        enum Tail<'c> {
            None,
            List(&'c [PExpr]),
            Range(&'c Cons),
        }
        let mut candidates: Vec<(IndexId, &IndexInfo)> = Vec::with_capacity(info.indexes.len() + 1);
        for (k, ix) in info.indexes.iter().enumerate() {
            candidates.push((IndexId::Named(u16::try_from(k).ok()?), ix));
        }
        if (unique_id_key(info).is_none() || info.key_index.as_ref().is_some_and(|k| k.prefix > 0))
            && let Some(key) = &info.key_index
        {
            candidates.push((IndexId::Id, key));
        }
        debug_assert!(
            {
                let lead = |ix: &IndexInfo| ix.fields.get(..ix.prefix).map(<[FieldId]>::to_vec);
                let first = candidates.first().map(|(_, ix)| lead(ix));
                candidates.iter().all(|(_, ix)| Some(lead(ix)) == first)
            },
            "the indexes of {} start with different separators",
            info.logical
        );
        type Best<'c, 'i> = (
            usize,
            bool,
            IndexId,
            &'i IndexInfo,
            usize,
            Vec<PExpr>,
            Tail<'c>,
        );
        let mut best: Option<Best<'_, '_>> = None;
        for (index, ix) in candidates {
            let separators = ix.fields.get(..ix.prefix).unwrap_or(&[]);
            let given = separators
                .iter()
                .all(|f| cons.iter().any(|(g, c)| g == f && c.eq.is_some()));
            let skip = if given { 0 } else { ix.prefix };
            let mut eqs: Vec<PExpr> = Vec::new();
            let mut tail = Tail::None;
            for f in ix.fields.iter().skip(skip) {
                let Some((_, c)) = cons.iter().find(|(g, _)| g == f) else {
                    break;
                };
                if let Some(e) = &c.eq {
                    eqs.push(e.clone());
                    continue;
                }
                if let Some(items) = &c.list {
                    tail = Tail::List(items);
                } else if c.lo.is_some() || c.hi.is_some() {
                    tail = Tail::Range(c);
                }
                break;
            }
            let covered = eqs.len() + usize::from(!matches!(tail, Tail::None));
            let plain = skip == 0;
            if covered > 0 && best.as_ref().is_none_or(|b| (covered, plain) > (b.0, b.1)) {
                best = Some((covered, plain, index, ix, skip, eqs, tail));
            }
        }
        let (_, _, index, ix, skip, eqs, tail) = best?;
        let index_name = ix.name.clone();
        let name = name.to_string();
        let fields = fields.to_vec();
        Some(match tail {
            Tail::List(items) => Plan::IndexPoints {
                table,
                name,
                index,
                index_name,
                prefix: eqs,
                values: items.to_vec(),
                fields,
                skip,
            },
            Tail::None => Plan::IndexRange {
                table,
                name,
                index,
                index_name,
                lo: Bound::Included(eqs.clone()),
                hi: Bound::Included(eqs),
                fields,
                skip,
            },
            Tail::Range(c) => {
                let edge = |b: &Option<(PExpr, bool)>| match b {
                    Some((e, true)) => Bound::Included(with_last(&eqs, e)),
                    Some((e, false)) => Bound::Excluded(with_last(&eqs, e)),
                    None if eqs.is_empty() => Bound::Unbounded,
                    None => Bound::Included(eqs.clone()),
                };
                Plan::IndexRange {
                    table,
                    name,
                    index,
                    index_name,
                    lo: edge(&c.lo),
                    hi: edge(&c.hi),
                    fields,
                    skip,
                }
            }
        })
    }

    pub(super) fn plain(
        &self,
        info: &TableInfo,
        table: TableId,
        name: String,
        fields: &[FieldId],
        rep: bool,
    ) -> Plan {
        if rep && let Some(code) = info.field_by_std(StdField::Code) {
            for (k, ix) in info.indexes.iter().enumerate() {
                if ix.fields.first() == Some(&code)
                    && let Ok(k16) = u16::try_from(k)
                {
                    return Plan::IndexRange {
                        table,
                        name,
                        index: IndexId::Named(k16),
                        index_name: ix.name.clone(),
                        lo: Bound::Unbounded,
                        hi: Bound::Unbounded,
                        fields: fields.to_vec(),
                        skip: 0,
                    };
                }
            }
        }
        if !rep && self.opts.force == Some(Force::Index) {
            return Plan::IndexRange {
                table,
                name,
                index: IndexId::Id,
                index_name: "ID".to_string(),
                lo: Bound::Unbounded,
                hi: Bound::Unbounded,
                fields: fields.to_vec(),
                skip: 0,
            };
        }
        Plan::Scan {
            table,
            name,
            fields: fields.to_vec(),
        }
    }
}

fn list_values(exprs: &[PExpr], cx: &Cx<'_>) -> Result<Vec<QValue>, ExecError> {
    let mut seen: HashSet<Key> = HashSet::new();
    let mut out = Vec::new();
    let mut add = |v: QValue| -> Result<(), ExecError> {
        if !is_null(&v) && seen.insert(crate::ops::join::key_charged(&v, cx)?) {
            out.push(v);
        }
        Ok(())
    };
    for e in exprs {
        match e {
            PExpr::Set(s) => {
                for v in &s.values {
                    add(v.clone())?;
                }
            }
            other => add(eval(other, &[], cx)?)?,
        }
    }
    Ok(out)
}

pub struct IdLookup {
    table: TableId,
    ids: Vec<PExpr>,
    fields: Vec<FieldId>,
    queue: Option<VecDeque<QValue>>,
}

impl IdLookup {
    pub fn new(table: TableId, ids: Vec<PExpr>, fields: Vec<FieldId>) -> IdLookup {
        IdLookup {
            table,
            ids,
            fields,
            queue: None,
        }
    }
}

impl<'a> Op<'a> for IdLookup {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.queue.is_none() {
            self.queue = Some(list_values(&self.ids, cx)?.into());
        }
        let Some(q) = self.queue.as_mut() else {
            return Ok(None);
        };
        while let Some(v) = q.pop_front() {
            cx.tick()?;
            if let Some(row) = fetch_by_id(cx, self.table, &v, &self.fields)? {
                return Ok(Some(project_fields(row, &self.fields, cx)?));
            }
        }
        Ok(None)
    }
}

pub struct IndexPoints<'a> {
    table: TableId,
    index: IndexId,
    prefix: Vec<PExpr>,
    values: Vec<PExpr>,
    fields: Vec<FieldId>,
    read: Vec<FieldId>,
    point_fields: Vec<FieldId>,
    points: Option<VecDeque<Vec<QValue>>>,
    checks: Vec<(usize, Key)>,
    cur: Option<RowIter<'a>>,
    skip: usize,
    areas: VecDeque<Vec<QValue>>,
    area: Option<(Vec<QValue>, usize)>,
}

impl<'a> IndexPoints<'a> {
    pub fn new(
        table: TableId,
        index: IndexId,
        prefix: Vec<PExpr>,
        values: Vec<PExpr>,
        fields: Vec<FieldId>,
    ) -> IndexPoints<'a> {
        IndexPoints {
            table,
            index,
            prefix,
            values,
            read: fields.clone(),
            fields,
            point_fields: Vec::new(),
            points: None,
            checks: Vec::new(),
            cur: None,
            skip: 0,
            areas: VecDeque::new(),
            area: None,
        }
    }

    pub fn skipping(mut self, skip: usize) -> Self {
        self.skip = skip;
        self
    }

    fn cover(&mut self, cx: &Cx<'_>) -> Result<(), ExecError> {
        let info = cx
            .src
            .map()
            .try_table(self.table)
            .ok_or_else(|| fatal("unknown table"))?;
        let ix = match self.index {
            IndexId::Id => info.key_index.as_ref(),
            IndexId::Named(k) => info.indexes.get(usize::from(k)),
        }
        .ok_or_else(|| fatal("unknown index"))?;
        let n = self.skip + self.prefix.len() + 1;
        self.point_fields = ix.fields.iter().take(n).copied().collect();
        if self.point_fields.len() != n {
            return Err(fatal("a point is longer than its index"));
        }
        for f in &self.point_fields {
            if !self.read.contains(f) {
                self.read.push(*f);
            }
        }
        Ok(())
    }

    fn next_key(&mut self) -> Option<Vec<QValue>> {
        if self.skip == 0 {
            return self.points.as_mut()?.pop_front();
        }
        let points = self.points.as_ref()?;
        loop {
            match &mut self.area {
                Some((area, at)) if *at < points.len() => {
                    let k = area.iter().chain(points.get(*at)?).cloned().collect();
                    *at += 1;
                    return Some(k);
                }
                _ => self.area = Some((self.areas.pop_front()?, 0)),
            }
        }
    }

    fn holds_point(&self, row: &Row, cx: &Cx<'_>) -> Result<bool, ExecError> {
        for (pos, key) in &self.checks {
            let v = row
                .get(*pos)
                .ok_or_else(|| fatal("field out of range of a stored row"))?;
            if crate::key::try_key_of(v, cx.coll)? != *key {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl<'a> Op<'a> for IndexPoints<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.points.is_none() {
            self.cover(cx)?;
            let mut pre = Vec::with_capacity(self.prefix.len());
            for e in &self.prefix {
                pre.push(eval(e, &[], cx)?);
            }
            let mut pts: VecDeque<Vec<QValue>> = if pre.iter().any(is_null) {
                VecDeque::new()
            } else {
                list_values(&self.values, cx)?
                    .into_iter()
                    .map(|v| {
                        let mut k = pre.clone();
                        k.push(v);
                        k
                    })
                    .collect()
            };
            if self.skip > 0 && !pts.is_empty() {
                match crate::ops::access::data_areas(cx, self.table, self.index, self.skip, false)?
                {
                    Some(areas) => self.areas = areas,
                    None => {
                        pts.clear();
                        self.checks.clear();
                        let src = cx.src;
                        self.cur = Some(src.scan_fields(self.table, &self.read)?);
                    }
                }
            }
            self.points = Some(pts);
        }
        loop {
            if let Some(it) = self.cur.as_mut() {
                cx.tick()?;
                match it.next() {
                    Some(r) => {
                        let r = r?;
                        if self.holds_point(&r, cx)? {
                            return Ok(Some(project_fields(r, &self.fields, cx)?));
                        }
                        continue;
                    }
                    None => self.cur = None,
                }
            }
            let Some(k) = self.next_key() else {
                return Ok(None);
            };
            self.checks = k
                .iter()
                .zip(&self.point_fields)
                .filter(|(v, _)| matches!(v, QValue::String(_)))
                .map(|(v, f)| Ok((usize::from(f.0), crate::key::try_key_of(v, cx.coll)?)))
                .collect::<Result<_, ExecError>>()?;
            let src = cx.src;
            self.cur = Some(src.range_fields(
                self.table,
                self.index,
                Bound::Included(&k[..]),
                Bound::Included(&k[..]),
                false,
                &self.read,
            )?);
        }
    }
}
