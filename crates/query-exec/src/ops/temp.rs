use std::ops::Bound;

use query_ir::{ExecError, PExpr, QValue};

use super::{Cx, Op, eval, fatal};
use crate::key::Key;
use crate::model::{row_model, rows_model};
use crate::source::Row;
use crate::temp::{TempTable, TempTableId};

pub(crate) fn table_of<'a>(cx: &Cx<'a>, id: TempTableId) -> Result<&'a TempTable, ExecError> {
    cx.temps
        .and_then(|t| t.table_by_id(id))
        .ok_or_else(|| fatal("temporary table is gone"))
}

fn copy_row(row: &Row, cx: &Cx<'_>) -> Result<Row, ExecError> {
    cx.mem.grow_scratch(row_model(row))?;
    Ok(row.clone())
}

pub struct TempScan {
    table: TempTableId,
    pos: usize,
}

impl TempScan {
    pub fn new(table: TempTableId) -> TempScan {
        TempScan { table, pos: 0 }
    }
}

impl<'a> Op<'a> for TempScan {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        cx.tick()?;
        let t = table_of(cx, self.table)?;
        let Some(row) = t.rows.get(self.pos) else {
            return Ok(None);
        };
        self.pos += 1;
        Ok(Some(copy_row(row, cx)?))
    }
}

pub struct ValuesScan {
    rows: std::sync::Arc<query_ir::ValueRows>,
    pos: usize,
    started: bool,
}

impl ValuesScan {
    pub fn new(rows: std::sync::Arc<query_ir::ValueRows>) -> ValuesScan {
        ValuesScan {
            rows,
            pos: 0,
            started: false,
        }
    }
}

impl<'a> Op<'a> for ValuesScan {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if !self.started {
            self.started = true;
            cx.mem.grow(rows_model(&self.rows.rows))?;
        }
        cx.tick()?;
        let Some(row) = self.rows.rows.get(self.pos) else {
            return Ok(None);
        };
        self.pos += 1;
        Ok(Some(copy_row(row, cx)?))
    }
}

pub(crate) fn bound_keys(
    b: &Bound<Vec<PExpr>>,
    cx: &Cx<'_>,
) -> Result<Option<Bound<Vec<Key>>>, ExecError> {
    let ev = |xs: &Vec<PExpr>| -> Result<Option<Vec<Key>>, ExecError> {
        let mut out = Vec::with_capacity(xs.len());
        for x in xs {
            let v = eval(x, &[], cx)?;
            if matches!(v, QValue::Null) {
                return Ok(None);
            }
            out.push(super::join::key_charged(&v, cx)?);
        }
        Ok(Some(out))
    };
    Ok(match b {
        Bound::Unbounded => Some(Bound::Unbounded),
        Bound::Included(x) => ev(x)?.map(Bound::Included),
        Bound::Excluded(x) => ev(x)?.map(Bound::Excluded),
    })
}

fn as_slice(b: &Bound<Vec<Key>>) -> Bound<&[Key]> {
    match b {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
    }
}

pub struct TempRange {
    table: TempTableId,
    index: usize,
    lo: Bound<Vec<PExpr>>,
    hi: Bound<Vec<PExpr>>,
    span: Option<(usize, usize)>,
}

impl TempRange {
    pub fn new(
        table: TempTableId,
        index: usize,
        lo: Bound<Vec<PExpr>>,
        hi: Bound<Vec<PExpr>>,
    ) -> TempRange {
        TempRange {
            table,
            index,
            lo,
            hi,
            span: None,
        }
    }
}

impl<'a> Op<'a> for TempRange {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let t = table_of(cx, self.table)?;
        if self.span.is_none() {
            let span = match (bound_keys(&self.lo, cx)?, bound_keys(&self.hi, cx)?) {
                (Some(lo), Some(hi)) => {
                    let ix = t
                        .indexes()
                        .get(self.index)
                        .ok_or_else(|| fatal("temporary table index is gone"))?;
                    ix.span(as_slice(&lo), as_slice(&hi))
                }
                _ => (0, 0),
            };
            self.span = Some(span);
        }
        let Some((pos, end)) = self.span.as_mut() else {
            return Ok(None);
        };
        cx.tick()?;
        if *pos >= *end {
            return Ok(None);
        }
        let ix = t
            .indexes()
            .get(self.index)
            .ok_or_else(|| fatal("temporary table index is gone"))?;
        let row = ix
            .rows_in(*pos, *pos + 1)
            .next()
            .and_then(|r| t.rows.get(r));
        *pos += 1;
        match row {
            Some(r) => Ok(Some(copy_row(r, cx)?)),
            None => Err(fatal("temporary table index entry without a row")),
        }
    }
}
