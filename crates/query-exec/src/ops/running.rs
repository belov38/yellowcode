use super::{BoxOp, Cx, Op, eval};
use crate::model::{block, row_cap_model, value_model};
use crate::source::Row;
use bsl_prim::{BslDate, Number};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::RunningBalanceL;
use query_ir::{ExecError, QValue};

type Rec = (u32, [u8; 16]);
#[derive(Clone, Copy)]
struct Boundary {
    date: BslDate,
    recorder: Option<Rec>,
}
fn boundary(v: &QValue) -> Option<Boundary> {
    match v {
        QValue::Date(date) if *date != BslDate::EMPTY => Some(Boundary {
            date: *date,
            recorder: None,
        }),
        QValue::PointInTime(m) if m.date != BslDate::EMPTY => Some(Boundary {
            date: m.date,
            recorder: m.reference,
        }),
        QValue::Boundary(b) => boundary(&b.point),
        _ => None,
    }
}
fn fatal(s: &str) -> ExecError {
    ExecError::Fatal(s.into())
}
fn number(row: &Row, i: usize) -> Result<&Number, ExecError> {
    match row.get(i) {
        Some(QValue::Number(n)) => Ok(n),
        _ => Err(fatal("running balance component is not numeric")),
    }
}
fn set(row: &mut Row, i: usize, v: QValue) -> Result<(), ExecError> {
    let slot = row
        .get_mut(i)
        .ok_or_else(|| fatal("running balance column"))?;
    *slot = v;
    Ok(())
}
struct Group {
    state: Row,
    bytes: u64,
    started: bool,
    last: Option<BslDate>,
}

pub struct RunningBalance<'a> {
    input: BoxOp<'a>,
    step: RunningBalanceL,
    pending: Option<(Row, u64)>,
    group: Option<Group>,
    initialized: bool,
    start: Option<Boundary>,
    end: Option<Boundary>,
    held: u64,
}
impl<'a> RunningBalance<'a> {
    pub fn new(input: BoxOp<'a>, step: RunningBalanceL) -> Self {
        Self {
            input,
            step,
            pending: None,
            group: None,
            initialized: false,
            start: None,
            end: None,
            held: 0,
        }
    }
    fn pull(&mut self, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        if self.pending.is_none()
            && let Some(row) = self.input.next(cx)?
        {
            let bytes = row_cap_model(&row);
            cx.mem.grow(bytes)?;
            self.held += bytes;
            self.pending = Some((row, bytes));
        }
        Ok(())
    }
    fn equal_columns(
        &self,
        a: &Row,
        b: &Row,
        columns: impl Iterator<Item = usize>,
        cx: &Cx<'_>,
    ) -> Result<bool, ExecError> {
        for i in columns {
            let (Some(a), Some(b)) = (a.get(i), b.get(i)) else {
                return Ok(false);
            };
            let equal = super::expr::keys_charged(a, b, cx, || {
                crate::eval::cmp(a, b, cx.env.collation).is_eq()
            })?;
            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn same_group(&self, row: &Row, cx: &Cx<'_>) -> Result<bool, ExecError> {
        let Some(g) = &self.group else {
            return Ok(false);
        };
        self.equal_columns(
            &g.state,
            row,
            (0..self.step.dims).map(|i| raw::DIMS + i),
            cx,
        )
    }
    fn release(&mut self, bytes: u64, cx: &Cx<'_>) {
        cx.mem.shrink(bytes);
        self.held = self.held.saturating_sub(bytes);
    }
    fn start_group(&mut self, cx: &mut Cx<'a>) -> Result<bool, ExecError> {
        self.pull(cx)?;
        let Some((first, _)) = &self.pending else {
            return Ok(false);
        };
        let bytes = row_cap_model(first);
        cx.mem.grow(bytes)?;
        self.held += bytes;
        let mut state = first.clone();
        let opening = matches!(state.get(raw::PERIOD), Some(QValue::Null));
        for r in 0..self.step.resources {
            for c in 0..raw::PER_RESOURCE {
                if !opening || c != raw::BALANCE {
                    set(
                        &mut state,
                        raw::component(self.step.dims, r, c),
                        QValue::Number(Number::zero()),
                    )?;
                }
            }
        }
        self.group = Some(Group {
            state,
            bytes,
            started: false,
            last: None,
        });
        if opening && let Some((_, n)) = self.pending.take() {
            self.release(n, cx);
        }
        Ok(true)
    }
    fn period(&self, date: BslDate, cx: &Cx<'_>) -> Result<BslDate, ExecError> {
        let v = eval(&self.step.period, &[QValue::Date(date)], cx)?;
        let QValue::Date(d) = v else {
            return Err(fatal("running balance period is not a date"));
        };
        Ok(self.start.map_or(d, |s| {
            if d.ticks() < s.date.ticks() {
                s.date
            } else {
                d
            }
        }))
    }
    fn fields(&self, row: &mut Row, cx: &Cx<'_>) -> Result<(), ExecError> {
        let offset = raw::width(self.step.dims, self.step.resources);
        for (i, e) in self.step.period_fields.iter().enumerate() {
            let v = eval(e, row, cx)?;
            set(row, offset + i, v)?;
        }
        Ok(())
    }
    fn boundary_row(&self, b: Boundary, date: BslDate, cx: &Cx<'_>) -> Result<Row, ExecError> {
        let g = self
            .group
            .as_ref()
            .ok_or_else(|| fatal("missing running balance key"))?;
        cx.mem.grow_scratch(row_cap_model(&g.state))?;
        let mut row = g.state.clone();
        set(&mut row, raw::PERIOD, QValue::Date(date))?;
        set(
            &mut row,
            raw::RECORDER,
            b.recorder
                .map_or(QValue::Undefined, |(table, id)| QValue::Ref { table, id }),
        )?;
        set(&mut row, raw::LINE, QValue::Number(Number::zero()))?;
        for r in 0..self.step.resources {
            let at = raw::component(self.step.dims, r, raw::BALANCE);
            cx.mem.grow_scratch(value_model(
                row.get(at).ok_or_else(|| fatal("running balance column"))?,
            ))?;
            let b = number(&row, at)?.clone();
            set(
                &mut row,
                raw::component(self.step.dims, r, raw::CLOSING),
                QValue::Number(b),
            )?;
        }
        self.fields(&mut row, cx)?;
        Ok(row)
    }
    fn keep(&self, row: &Row, movement: bool) -> Result<bool, ExecError> {
        let nonzero =
            |r, c| number(row, raw::component(self.step.dims, r, c)).map(|n| !n.is_zero());
        if self.step.wanted.is_empty() {
            return Err(fatal(
                "periodic balance without selected resource components is not qualified",
            ));
        }
        for (r, c) in &self.step.wanted {
            let components: &[usize] = match (movement, *c) {
                (true, raw::BALANCE | raw::CLOSING) => &[raw::RECEIPT, raw::EXPENSE],
                (true, raw::RECEIPT) => &[raw::RECEIPT],
                (true, raw::EXPENSE) => &[raw::EXPENSE],
                (true, raw::TURNOVER) => &[raw::TURNOVER],
                (false, raw::BALANCE | raw::CLOSING) => &[raw::BALANCE],
                _ => &[],
            };
            for c in components {
                if nonzero(*r, *c)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    fn full_key(&self, a: &Row, b: &Row, cx: &Cx<'_>) -> Result<bool, ExecError> {
        self.equal_columns(
            a,
            b,
            (0..3).chain(raw::width(self.step.dims, self.step.resources)..self.step.width()),
            cx,
        )
    }
    fn movement(&mut self, mut row: Row, bytes: u64, cx: &Cx<'_>) -> Result<Row, ExecError> {
        cx.mem.grow_scratch(bytes)?;
        let g = self
            .group
            .as_mut()
            .ok_or_else(|| fatal("missing running balance key"))?;
        let Some(QValue::Date(date)) = row.get(raw::PERIOD) else {
            return Err(fatal("movement period is not a date"));
        };
        g.last = Some(*date);
        for r in 0..self.step.resources {
            let at = |c| raw::component(self.step.dims, r, c);
            let b = number(&g.state, at(raw::BALANCE))?;
            let p = number(&row, at(raw::RECEIPT))?;
            let e = number(&row, at(raw::EXPENSE))?;
            let limbs = b
                .size_hint_limbs()
                .max(p.size_hint_limbs())
                .max(e.size_hint_limbs());
            cx.mem.grow_scratch(block(
                64u64
                    .saturating_mul(limbs.saturating_add(1))
                    .saturating_add(256),
            ))?;
            let closing = b.add(p).sub(e);
            crate::num::guard(cx.env, &closing)?;
            let opening = b.clone();
            let old = value_model(
                g.state
                    .get(at(raw::BALANCE))
                    .ok_or_else(|| fatal("running state column"))?,
            );
            let next = QValue::Number(closing.clone());
            let new = value_model(&next);
            if new > old {
                cx.mem.grow(new - old)?;
                self.held += new - old;
                g.bytes += new - old;
            }
            set(&mut g.state, at(raw::BALANCE), next)?;
            if old > new {
                cx.mem.shrink(old - new);
                self.held -= old - new;
                g.bytes -= old - new;
            }
            set(&mut row, at(raw::BALANCE), QValue::Number(opening))?;
            set(&mut row, at(raw::CLOSING), QValue::Number(closing))?;
        }
        self.release(bytes, cx);
        Ok(row)
    }
}
impl<'a> Op<'a> for RunningBalance<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if !self.initialized {
            self.start = match &self.step.bounds[0] {
                Some(e) => boundary(&eval(e, &[], cx)?),
                None => None,
            };
            self.end = match &self.step.bounds[1] {
                Some(e) => boundary(&eval(e, &[], cx)?),
                None => None,
            };
            self.initialized = true;
        }
        loop {
            cx.tick()?;
            if self.group.is_none() && !self.start_group(cx)? {
                self.close(cx);
                return Ok(None);
            }
            self.pull(cx)?;
            if self.group.as_ref().is_some_and(|g| !g.started) {
                if let Some(g) = self.group.as_mut() {
                    g.started = true;
                }
                if self.step.boundaries
                    && let Some(start) = self.start
                {
                    let row = self.boundary_row(start, start.date, cx)?;
                    let merge = match &self.pending {
                        Some((p, _)) => self.same_group(p, cx)? && self.full_key(&row, p, cx)?,
                        None => false,
                    };
                    if !merge && self.keep(&row, false)? {
                        if let Some(g) = self.group.as_mut() {
                            g.last = Some(start.date);
                        }
                        return Ok(Some(row));
                    }
                }
            }
            let same = match &self.pending {
                Some((row, _)) => self.same_group(row, cx)?,
                None => false,
            };
            if same {
                let (row, bytes) = self
                    .pending
                    .take()
                    .ok_or_else(|| fatal("missing running lookahead"))?;
                let row = self.movement(row, bytes, cx)?;
                if self.keep(&row, true)? {
                    return Ok(Some(row));
                }
                continue;
            }
            let end = if self.step.boundaries { self.end } else { None };
            let row = if let Some(end) = end {
                let date = self.period(end.date, cx)?;
                let last = self.group.as_ref().and_then(|g| g.last);
                if last.is_none_or(|last| last.ticks() < date.ticks()) {
                    let row = self.boundary_row(end, date, cx)?;
                    self.keep(&row, false)?.then_some(row)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(g) = self.group.take() {
                self.release(g.bytes, cx);
            }
            if row.is_some() {
                return Ok(row);
            }
        }
    }
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.pending = None;
        self.group = None;
        cx.mem.shrink(self.held);
        self.held = 0;
        self.input.close(cx);
    }
}
