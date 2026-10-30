use std::collections::VecDeque;
use std::ops::Bound;

use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, TableId};
use query_ir::{ExecError, PExpr, QValue};

use super::{Cx, Op, eval, fatal};
use crate::key::try_key_of;
use crate::model::{VALUE_SIZE, vec_block};
use crate::source::{IndexPrefixes, Row, RowIter, SKIP_MAX_AREAS};

pub(crate) fn project_fields(row: Row, fields: &[FieldId], cx: &Cx<'_>) -> Result<Row, ExecError> {
    cx.mem.grow_scratch(vec_block(fields.len(), VALUE_SIZE))?;
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        out.push(
            row.get(usize::from(f.0))
                .cloned()
                .ok_or_else(|| fatal("field out of range of a stored row"))?,
        );
    }
    Ok(out)
}

pub(crate) fn id_field(cx: &Cx<'_>, table: TableId) -> Result<Option<FieldId>, ExecError> {
    let info = cx
        .src
        .map()
        .try_table(table)
        .ok_or_else(|| fatal("unknown table"))?;
    Ok(info
        .field_by_std(StdField::Id)
        .or_else(|| info.field_by_name("ID")))
}

pub(crate) fn with_id(
    cx: &Cx<'_>,
    table: TableId,
    fields: &[FieldId],
) -> Result<Vec<FieldId>, ExecError> {
    let mut out = fields.to_vec();
    if let Some(id) = id_field(cx, table)?
        && !out.contains(&id)
    {
        out.push(id);
    }
    Ok(out)
}

pub(crate) fn fetch_by_id(
    cx: &Cx<'_>,
    table: TableId,
    id: &QValue,
    fields: &[FieldId],
) -> Result<Option<Row>, ExecError> {
    if matches!(id, QValue::Null) {
        return Ok(None);
    }
    let wanted = with_id(cx, table, fields)?;
    let Some(row) = cx.src.get_by_id_fields(table, id, &wanted)? else {
        return Ok(None);
    };
    if let Some(f) = id_field(cx, table)? {
        let same = match row.get(usize::from(f.0)) {
            Some(v) => try_key_of(v, cx.coll)? == try_key_of(id, cx.coll)?,
            None => false,
        };
        if !same {
            return Ok(None);
        }
    }
    Ok(Some(row))
}

pub struct Scan<'a> {
    table: TableId,
    fields: Vec<FieldId>,
    iter: Option<RowIter<'a>>,
}

impl<'a> Scan<'a> {
    pub fn new(table: TableId, fields: Vec<FieldId>) -> Scan<'a> {
        Scan {
            table,
            fields,
            iter: None,
        }
    }
}

impl<'a> Op<'a> for Scan<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.iter.is_none() {
            let src = cx.src;
            self.iter = Some(src.scan_fields(self.table, &self.fields)?);
        }
        let Some(it) = self.iter.as_mut() else {
            return Ok(None);
        };
        cx.tick()?;
        match it.next() {
            None => Ok(None),
            Some(r) => Ok(Some(project_fields(r?, &self.fields, cx)?)),
        }
    }
}

pub struct GetById {
    table: TableId,
    id: PExpr,
    fields: Vec<FieldId>,
    done: bool,
}

impl GetById {
    pub fn new(table: TableId, id: PExpr, fields: Vec<FieldId>) -> GetById {
        GetById {
            table,
            id,
            fields,
            done: false,
        }
    }
}

impl<'a> Op<'a> for GetById {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(None);
        }
        cx.tick()?;
        let id = eval(&self.id, &[], cx)?;
        match fetch_by_id(cx, self.table, &id, &self.fields)? {
            Some(r) => Ok(Some(project_fields(r, &self.fields, cx)?)),
            None => Ok(None),
        }
    }
}

pub struct IndexRange<'a> {
    table: TableId,
    index: IndexId,
    lo: Bound<Vec<PExpr>>,
    hi: Bound<Vec<PExpr>>,
    desc: bool,
    fields: Vec<FieldId>,
    iter: Option<RowIter<'a>>,
    skip: usize,
    areas: Option<Areas>,
}

struct Areas {
    left: VecDeque<Vec<QValue>>,
    lo: Bound<Vec<QValue>>,
    hi: Bound<Vec<QValue>>,
}

pub(crate) fn data_areas(
    cx: &Cx<'_>,
    table: TableId,
    index: IndexId,
    skip: usize,
    desc: bool,
) -> Result<Option<VecDeque<Vec<QValue>>>, ExecError> {
    let Some(IndexPrefixes { tuples, fresh }) =
        cx.src.index_prefixes(table, index, skip, SKIP_MAX_AREAS)?
    else {
        return Ok(None);
    };
    if fresh {
        cx.mem
            .grow(tuples.iter().map(|t| super::row_bytes(t)).sum::<u64>())?;
    }
    let mut left: VecDeque<Vec<QValue>> = tuples.iter().cloned().collect();
    if desc {
        left = left.into_iter().rev().collect();
    }
    Ok(Some(left))
}

pub(crate) fn under_area(area: &[QValue], b: &Bound<Vec<QValue>>) -> Bound<Vec<QValue>> {
    let join = |v: &[QValue]| -> Vec<QValue> { area.iter().chain(v).cloned().collect() };
    match b {
        Bound::Unbounded => Bound::Included(area.to_vec()),
        Bound::Included(v) => Bound::Included(join(v)),
        Bound::Excluded(v) => Bound::Excluded(join(v)),
    }
}

impl<'a> IndexRange<'a> {
    pub fn new(
        table: TableId,
        index: IndexId,
        lo: Bound<Vec<PExpr>>,
        hi: Bound<Vec<PExpr>>,
        desc: bool,
        fields: Vec<FieldId>,
    ) -> IndexRange<'a> {
        IndexRange {
            table,
            index,
            lo,
            hi,
            desc,
            fields,
            iter: None,
            skip: 0,
            areas: None,
        }
    }

    pub fn skipping(mut self, skip: usize) -> Self {
        self.skip = skip;
        self
    }

    fn next_skipping(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.areas.is_none() {
            let (lo, hi) = (bound_values(&self.lo, cx)?, bound_values(&self.hi, cx)?);
            let left = match data_areas(cx, self.table, self.index, self.skip, self.desc)? {
                Some(left) => left,
                None => {
                    let src = cx.src;
                    self.iter = Some(src.scan_fields(self.table, &self.fields)?);
                    VecDeque::new()
                }
            };
            self.areas = Some(Areas { left, lo, hi });
        }
        loop {
            if let Some(it) = self.iter.as_mut() {
                cx.tick()?;
                match it.next() {
                    Some(r) => return Ok(Some(project_fields(r?, &self.fields, cx)?)),
                    None => self.iter = None,
                }
            }
            let Some(areas) = self.areas.as_mut() else {
                return Ok(None);
            };
            let Some(area) = areas.left.pop_front() else {
                return Ok(None);
            };
            let (lo, hi) = (under_area(&area, &areas.lo), under_area(&area, &areas.hi));
            let src = cx.src;
            self.iter = Some(src.range_fields(
                self.table,
                self.index,
                as_slice(&lo),
                as_slice(&hi),
                self.desc,
                &self.fields,
            )?);
        }
    }
}

fn bound_values(b: &Bound<Vec<PExpr>>, cx: &Cx<'_>) -> Result<Bound<Vec<QValue>>, ExecError> {
    let ev = |xs: &Vec<PExpr>| -> Result<Vec<QValue>, ExecError> {
        xs.iter().map(|x| eval(x, &[], cx)).collect()
    };
    Ok(match b {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(x) => Bound::Included(ev(x)?),
        Bound::Excluded(x) => Bound::Excluded(ev(x)?),
    })
}

fn as_slice(b: &Bound<Vec<QValue>>) -> Bound<&[QValue]> {
    match b {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
    }
}

impl<'a> Op<'a> for IndexRange<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.skip > 0 {
            return self.next_skipping(cx);
        }
        if self.iter.is_none() {
            let (lo, hi) = (bound_values(&self.lo, cx)?, bound_values(&self.hi, cx)?);
            let src = cx.src;
            self.iter = Some(src.range_fields(
                self.table,
                self.index,
                as_slice(&lo),
                as_slice(&hi),
                self.desc,
                &self.fields,
            )?);
        }
        let Some(it) = self.iter.as_mut() else {
            return Ok(None);
        };
        cx.tick()?;
        match it.next() {
            None => Ok(None),
            Some(r) => Ok(Some(project_fields(r?, &self.fields, cx)?)),
        }
    }
}
