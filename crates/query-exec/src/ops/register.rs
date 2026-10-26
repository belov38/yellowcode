use std::cmp::Ordering;
use std::rc::Rc;

use bsl_prim::{BslDate, Number};
use ibschema::storage::{FieldId, TableId};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{RegisterRowsL, StoredTotals};
use query_ir::schema::VtKind;
use query_ir::{ExecError, QValue};

use super::{Cx, Op};
use crate::register::{RegisterKind, RegisterSettings};
use crate::source::{Row, RowIter};

fn fatal(s: impl Into<String>) -> ExecError {
    ExecError::Fatal(s.into())
}

type Rec = (u32, [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Point {
    Date(BslDate),
    Moment(BslDate, Rec),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Bnd {
    None,
    Date(BslDate),
    Moment(BslDate, Rec),
    Incl(Point),
    Excl(Point),
}

fn point_of(v: &QValue) -> Option<Point> {
    match v {
        QValue::Date(d) if *d != BslDate::EMPTY => Some(Point::Date(*d)),
        QValue::PointInTime(m) if m.date != BslDate::EMPTY => Some(match m.reference {
            Some(r) => Point::Moment(m.date, r),
            None => Point::Date(m.date),
        }),
        _ => None,
    }
}

fn bound_of(v: Option<&QValue>) -> Bnd {
    match v {
        None => Bnd::None,
        Some(QValue::Boundary(b)) => match point_of(&b.point) {
            Some(p) if b.include => Bnd::Incl(p),
            Some(p) => Bnd::Excl(p),
            None => Bnd::None,
        },
        Some(v) => match point_of(v) {
            Some(Point::Date(d)) => Bnd::Date(d),
            Some(Point::Moment(d, r)) => Bnd::Moment(d, r),
            None => Bnd::None,
        },
    }
}

fn cmp_pos(t: BslDate, r: Rec, p: Point, date_side: Ordering) -> Ordering {
    match p {
        Point::Date(d) => t.ticks().cmp(&d.ticks()).then(date_side.reverse()),
        Point::Moment(d, pr) => t.ticks().cmp(&d.ticks()).then_with(|| r.cmp(&pr)),
    }
}

fn in_balance(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() < d.ticks(),
        Bnd::Moment(d, pr) => cmp_pos(t, r, Point::Moment(d, pr), Ordering::Less) == Ordering::Less,
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Greater) != Ordering::Greater,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Less) == Ordering::Less,
    }
}

fn after_start(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() >= d.ticks(),
        Bnd::Moment(d, pr) => cmp_pos(t, r, Point::Moment(d, pr), Ordering::Less) != Ordering::Less,
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Less) != Ordering::Less,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Greater) == Ordering::Greater,
    }
}

fn before_end(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() <= d.ticks(),
        Bnd::Moment(d, pr) => {
            cmp_pos(t, r, Point::Moment(d, pr), Ordering::Greater) != Ordering::Greater
        }
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Greater) != Ordering::Greater,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Less) == Ordering::Less,
    }
}

fn date_of(b: Bnd) -> Option<BslDate> {
    match b {
        Bnd::None => None,
        Bnd::Date(d) | Bnd::Moment(d, _) => Some(d),
        Bnd::Incl(Point::Date(d) | Point::Moment(d, _))
        | Bnd::Excl(Point::Date(d) | Point::Moment(d, _)) => Some(d),
    }
}

fn month_start(d: BslDate) -> Option<BslDate> {
    let p = d.parts();
    BslDate::from_parts(p.year, p.month, 1, 0, 0, 0)
}

fn next_month(d: BslDate) -> Option<BslDate> {
    let p = d.parts();
    if p.month >= 12 {
        BslDate::from_parts(p.year + 1, 1, 1, 0, 0, 0)
    } else {
        BslDate::from_parts(p.year, p.month + 1, 1, 0, 0, 0)
    }
}

fn plus_second(d: BslDate) -> Option<BslDate> {
    let p = d.parts();
    let base = BslDate::from_parts(p.year, p.month, p.day, 0, 0, 0)?;
    let one = BslDate::from_parts(p.year, p.month, p.day, 0, 0, 1)?;
    BslDate::from_ticks(d.ticks() + (one.ticks() - base.ticks()))
}

fn current_point() -> Option<BslDate> {
    BslDate::from_parts(3999, 11, 1, 0, 0, 0)
}

fn first_point_after(b: Bnd) -> Option<Option<BslDate>> {
    match b {
        Bnd::None => Some(None),
        Bnd::Date(d) | Bnd::Excl(Point::Date(d)) => Some(Some(d)),
        Bnd::Moment(d, _)
        | Bnd::Incl(Point::Date(d) | Point::Moment(d, _))
        | Bnd::Excl(Point::Moment(d, _)) => plus_second(d).map(Some),
    }
}

fn prev_month(d: BslDate) -> Option<BslDate> {
    let p = d.parts();
    if p.month <= 1 {
        BslDate::from_parts(p.year - 1, 12, 1, 0, 0, 0)
    } else {
        BslDate::from_parts(p.year, p.month - 1, 1, 0, 0, 0)
    }
}

#[derive(Clone, Debug)]
enum Part {
    Movements {
        keep: Keep,
        balance: bool,
        neg: bool,
    },
    Totals {
        at: BslDate,
    },
    Turnovers {
        lo: Option<BslDate>,
        hi: Option<BslDate>,
    },
}

#[derive(Clone, Copy, Debug)]
enum Keep {
    InBalance(Bnd),
    InBalanceFrom(Bnd, BslDate),
    OutBalanceBefore(Bnd, BslDate),
    Window {
        start: Bnd,
        end: Bnd,
        months: Option<(Option<BslDate>, Option<BslDate>)>,
    },
}

#[derive(Debug)]
pub(crate) struct SelectedRegister {
    pub(crate) bounds: [Option<QValue>; 2],
    pub(crate) settings: RegisterSettings,
    parts: Vec<Part>,
}

impl SelectedRegister {
    pub(crate) fn metadata_bytes(&self) -> usize {
        let values = self
            .bounds
            .iter()
            .flatten()
            .map(crate::model::value_model)
            .sum::<u64>();
        std::mem::size_of::<Self>()
            .saturating_add(16)
            .saturating_add(
                self.parts
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Part>()),
            )
            .saturating_add(usize::try_from(values).unwrap_or(usize::MAX))
    }

    pub(crate) fn tables(&self, step: &RegisterRowsL) -> impl Iterator<Item = TableId> + '_ {
        let movements = step.movements;
        let totals = step.totals.as_ref().map(|t| t.table);
        let turnovers = step.turnovers.as_ref().map(|t| t.table);
        self.parts.iter().filter_map(move |part| match part {
            Part::Movements { .. } => Some(movements),
            Part::Totals { .. } => totals,
            Part::Turnovers { .. } => turnovers,
        })
    }
}

pub struct RegisterRows<'a> {
    step: RegisterRowsL,
    selected: Option<Rc<SelectedRegister>>,
    cursor: usize,
    observation: Option<crate::observe::RegisterObservation>,
    cur: Option<(Part, RowIter<'a>)>,
}

impl<'a> RegisterRows<'a> {
    pub fn new(step: RegisterRowsL) -> RegisterRows<'a> {
        Self::observed(step, None)
    }

    pub(crate) fn observed(
        step: RegisterRowsL,
        observation: Option<crate::observe::RegisterObservation>,
    ) -> RegisterRows<'a> {
        RegisterRows {
            step,
            selected: None,
            cursor: 0,
            observation,
            cur: None,
        }
    }
}

fn select_parts(
    st: &RegisterRowsL,
    bounds: &[Option<QValue>; 2],
    s: &RegisterSettings,
) -> Result<Vec<Part>, ExecError> {
    let b0 = bound_of(bounds[0].as_ref());
    let b1 = bound_of(bounds[1].as_ref());
    let mut parts = Vec::new();
    let balance_at =
        |b: Bnd, parts: &mut Vec<Part>| -> Result<(), ExecError> { balance_parts(st, s, b, parts) };
    match st.kind {
        VtKind::Balance => balance_at(b0, &mut parts)?,
        VtKind::BalanceAndTurnovers => {
            if b0 != Bnd::None {
                balance_at(opening_bound(b0), &mut parts)?;
            }
            parts.push(Part::Movements {
                keep: Keep::Window {
                    start: b0,
                    end: b1,
                    months: None,
                },
                balance: false,
                neg: false,
            });
        }
        _ => {
            let months = turnover_months(st, s, b0, b1);
            if let Some((lo, hi)) = months {
                parts.push(Part::Turnovers { lo, hi });
            }
            if !months.is_some_and(|(lo, hi)| months_cover_window(b0, b1, lo, hi)) {
                parts.push(Part::Movements {
                    keep: Keep::Window {
                        start: b0,
                        end: b1,
                        months,
                    },
                    balance: false,
                    neg: false,
                });
            }
        }
    }
    Ok(parts)
}

fn months_cover_window(b0: Bnd, b1: Bnd, lo: Option<BslDate>, hi: Option<BslDate>) -> bool {
    let start_inside = match (b0, lo) {
        (Bnd::None, None) => true,
        (Bnd::Date(d) | Bnd::Incl(Point::Date(d)), Some(lo)) => d.ticks() >= lo.ticks(),
        _ => false,
    };
    let end_inside = match (b1, hi) {
        (Bnd::None, None) => true,
        (Bnd::Date(d) | Bnd::Incl(Point::Date(d)), Some(hi)) => {
            plus_second(d).is_some_and(|end| end.ticks() <= hi.ticks())
        }
        (Bnd::Excl(Point::Date(d)), Some(hi)) => d.ticks() <= hi.ticks(),
        _ => false,
    };
    start_inside && end_inside
}

fn opening_bound(b: Bnd) -> Bnd {
    match b {
        Bnd::None => Bnd::None,
        Bnd::Date(d) => Bnd::Date(d),
        Bnd::Moment(d, r) => Bnd::Moment(d, r),
        Bnd::Incl(p) => Bnd::Excl(p),
        Bnd::Excl(p) => Bnd::Incl(p),
    }
}

fn balance_parts(
    st: &RegisterRowsL,
    s: &RegisterSettings,
    b: Bnd,
    parts: &mut Vec<Part>,
) -> Result<(), ExecError> {
    let movements = |keep| Part::Movements {
        keep,
        balance: true,
        neg: false,
    };
    if st.totals.is_none() {
        parts.push(movements(Keep::InBalance(b)));
        return Ok(());
    }
    let current = if s.current { current_point() } else { None };
    let last_monthly = s.calculated.and_then(|c| {
        if c.ticks() < s.min_period.ticks() {
            Some(c)
        } else {
            prev_month(s.min_period)
        }
    });
    let want = first_point_after(b).ok_or_else(|| fatal("register boundary out of range"))?;
    let after = match want {
        None => current,
        Some(p) => {
            let m = match month_start(p) {
                Some(x) if x.ticks() == p.ticks() => Some(p),
                _ => next_month(p),
            };
            match (m, last_monthly) {
                (Some(m), Some(last)) if m.ticks() <= last.ticks() => Some(m),
                _ => current,
            }
        }
    };
    if let Some(at) = after {
        parts.push(Part::Totals { at });
        let empty_correction = matches!(b, Bnd::Date(d) | Bnd::Excl(Point::Date(d)) if d == at);
        if want.is_some() && !empty_correction {
            parts.push(Part::Movements {
                keep: Keep::OutBalanceBefore(b, at),
                balance: true,
                neg: true,
            });
        }
        return Ok(());
    }
    match last_monthly {
        Some(last) => {
            parts.push(Part::Totals { at: last });
            parts.push(movements(Keep::InBalanceFrom(b, last)));
        }
        None => parts.push(movements(Keep::InBalance(b))),
    }
    Ok(())
}

fn turnover_months(
    st: &RegisterRowsL,
    s: &RegisterSettings,
    b0: Bnd,
    b1: Bnd,
) -> Option<(Option<BslDate>, Option<BslDate>)> {
    let tn = st.turnovers.as_ref()?;
    if s.agg_mode || !st.coarse {
        return None;
    }
    if st
        .needed_dims
        .iter()
        .zip(&tn.dims)
        .any(|(need, d)| *need && d.is_none())
    {
        return None;
    }
    let (lo, lo_aligned) = match b0 {
        Bnd::None => (None, true),
        Bnd::Date(d) | Bnd::Incl(Point::Date(d)) => {
            let m = month_start(d)?;
            if m.ticks() == d.ticks() {
                (Some(m), true)
            } else {
                (Some(next_month(d)?), false)
            }
        }
        _ => (Some(next_month(date_of(b0)?)?), false),
    };
    let (hi, hi_aligned) = match b1 {
        Bnd::None => (None, true),
        Bnd::Date(d) | Bnd::Incl(Point::Date(d)) => {
            let nm = next_month(d)?;
            let end = plus_second(d)?;
            if end.ticks() == nm.ticks() {
                (Some(nm), true)
            } else {
                (Some(month_start(d)?), false)
            }
        }
        Bnd::Excl(Point::Date(d)) => {
            let m = month_start(d)?;
            (Some(m), m.ticks() == d.ticks())
        }
        _ => (Some(month_start(date_of(b1)?)?), false),
    };
    if st.periodic && !(lo_aligned && hi_aligned) {
        return None;
    }
    if let (Some(l), Some(h)) = (lo, hi)
        && l.ticks() >= h.ticks()
    {
        return None;
    }
    Some((lo, hi))
}

fn get(row: &Row, f: FieldId) -> &QValue {
    row.get(usize::from(f.0)).unwrap_or(&QValue::Null)
}

fn num(v: &QValue) -> Number {
    match v {
        QValue::Number(n) => n.clone(),
        _ => Number::zero(),
    }
}

fn rec(v: &QValue) -> Rec {
    match v {
        QValue::Ref { table, id } => (*table, *id),
        _ => (0, [0; 16]),
    }
}

impl<'a> RegisterRows<'a> {
    fn width(&self) -> usize {
        raw::width(self.step.dims.len(), self.step.resources.len())
    }

    fn base(&self, period: QValue, recorder: QValue, line: QValue, dims: Vec<QValue>) -> Row {
        let mut out = Vec::with_capacity(self.width());
        out.push(period);
        out.push(recorder);
        out.push(line);
        out.extend(dims);
        out.resize(self.width(), QValue::Number(Number::zero()));
        out
    }

    fn set(&self, row: &mut Row, r: usize, c: usize, v: Number) {
        if let Some(x) = row.get_mut(raw::component(self.step.dims.len(), r, c)) {
            *x = QValue::Number(v);
        }
    }

    fn movement(&self, part: &Part, m: &Row) -> Option<Row> {
        let st = &self.step;
        let Part::Movements { keep, balance, neg } = part else {
            return None;
        };
        if !matches!(get(m, st.active), QValue::Bool(true)) {
            return None;
        }
        let QValue::Date(t) = *get(m, st.period) else {
            return None;
        };
        let r = rec(get(m, st.recorder));
        let take = match *keep {
            Keep::InBalance(b) => in_balance(t, r, b),
            Keep::InBalanceFrom(b, from) => in_balance(t, r, b) && t.ticks() >= from.ticks(),
            Keep::OutBalanceBefore(b, to) => !in_balance(t, r, b) && t.ticks() < to.ticks(),
            Keep::Window { start, end, months } => {
                after_start(t, r, start)
                    && before_end(t, r, end)
                    && !months.is_some_and(|(lo, hi)| {
                        lo.is_none_or(|l| t.ticks() >= l.ticks())
                            && hi.is_none_or(|h| t.ticks() < h.ticks())
                    })
            }
        };
        if !take {
            return None;
        }
        let expense = st.record_kind.is_some_and(|k| match get(m, k) {
            QValue::Number(n) => *n != Number::zero(),
            QValue::SysEnum(_, i) => *i != 0,
            _ => false,
        });
        let dims = st.dims.iter().map(|f| get(m, *f).clone()).collect();
        let mut out = self.base(
            QValue::Date(t),
            get(m, st.recorder).clone(),
            get(m, st.line).clone(),
            dims,
        );
        if *balance && st.periodic && st.kind == VtKind::BalanceAndTurnovers {
            if let Some(period) = out.get_mut(raw::PERIOD) {
                *period = QValue::Null;
            }
        }
        for (i, f) in st.resources.iter().enumerate() {
            let q = num(get(m, *f));
            if *balance {
                let mut v = if expense { q.neg() } else { q };
                if *neg {
                    v = v.neg();
                }
                self.set(&mut out, i, raw::BALANCE, v.clone());
                self.set(&mut out, i, raw::CLOSING, v);
            } else {
                let (p, e) = if expense {
                    (Number::zero(), q)
                } else {
                    (q, Number::zero())
                };
                let o = p.sub(&e);
                self.set(&mut out, i, raw::RECEIPT, p);
                self.set(&mut out, i, raw::EXPENSE, e);
                self.set(&mut out, i, raw::TURNOVER, o.clone());
                self.set(&mut out, i, raw::CLOSING, o);
            }
        }
        Some(out)
    }

    fn stored(&self, part: &Part, t: &StoredTotals, row: &Row) -> Option<Row> {
        let QValue::Date(p) = *get(row, t.period) else {
            return None;
        };
        let turnover = match part {
            Part::Totals { at } if p.ticks() == at.ticks() => false,
            Part::Turnovers { lo, hi }
                if lo.is_none_or(|l| p.ticks() >= l.ticks())
                    && hi.is_none_or(|h| p.ticks() < h.ticks()) =>
            {
                true
            }
            _ => return None,
        };
        let dims = t
            .dims
            .iter()
            .map(|f| f.map_or(QValue::Null, |f| get(row, f).clone()))
            .collect();
        let mut out = self.base(
            if turnover {
                QValue::Date(p)
            } else {
                QValue::Null
            },
            QValue::Null,
            QValue::Null,
            dims,
        );
        for (i, f) in t.resources.iter().enumerate() {
            let v = num(get(row, *f));
            if turnover {
                self.set(&mut out, i, raw::RECEIPT, v.clone());
                self.set(&mut out, i, raw::TURNOVER, v.clone());
            } else {
                self.set(&mut out, i, raw::BALANCE, v.clone());
            }
            self.set(&mut out, i, raw::CLOSING, v);
        }
        Some(out)
    }

    fn open(&self, part: &Part, cx: &Cx<'a>) -> Result<RowIter<'a>, ExecError> {
        let st = &self.step;
        let src = cx.src;
        match part {
            Part::Movements { .. } => {
                let mut fields = vec![st.period, st.recorder, st.line, st.active];
                fields.extend(st.record_kind);
                fields.extend(st.dims.iter().copied());
                fields.extend(st.resources.iter().copied());
                src.scan_fields(st.movements, &fields)
            }
            Part::Totals { .. } => {
                let t = st.totals.as_ref().ok_or_else(|| fatal("no totals table"))?;
                src.scan_fields(t.table, &stored_fields(t))
            }
            Part::Turnovers { .. } => {
                let t = st
                    .turnovers
                    .as_ref()
                    .ok_or_else(|| fatal("no turnover table"))?;
                src.scan_fields(t.table, &stored_fields(t))
            }
        }
    }
}

fn stored_fields(t: &StoredTotals) -> Vec<FieldId> {
    let mut f = vec![t.period];
    f.extend(t.dims.iter().flatten().copied());
    f.extend(t.resources.iter().copied());
    f
}

pub(crate) fn totals_off() -> ExecError {
    ExecError::Runtime(query_ir::RuntimeError {
        chain: vec![query_syntax::QueryError::new(
            query_syntax::MsgId::USE_TOTALS_SWITCH_OFF,
            None,
        )],
    })
}

impl<'a> Op<'a> for RegisterRows<'a> {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.selected.is_none() {
            let mut bounds = [None, None];
            for (slot, b) in bounds.iter_mut().zip(&self.step.bounds) {
                *slot = match b {
                    Some(e) => Some(super::eval(e, &[], cx)?),
                    None => None,
                };
            }
            let s = crate::register::read_register_settings_with_observation(
                cx.src,
                self.step.register,
                RegisterKind::Accumulation,
                cx.observation.as_ref(),
            )?;
            if !s.use_totals {
                return Err(totals_off());
            }
            let parts = select_parts(&self.step, &bounds, &s)?;
            let selected = Rc::new(SelectedRegister {
                bounds,
                settings: s,
                parts,
            });
            if let (Some(observation), Some(session)) = (&self.observation, &cx.observation) {
                observation.publish(session, &self.step, selected.clone());
            }
            self.selected = Some(selected);
        }
        loop {
            if self.cur.is_none() {
                let Some(part) = self
                    .selected
                    .as_ref()
                    .and_then(|s| s.parts.get(self.cursor))
                    .cloned()
                else {
                    return Ok(None);
                };
                self.cursor += 1;
                let it = self.open(&part, cx)?;
                self.cur = Some((part, it));
            }
            let Some((part, it)) = self.cur.as_mut() else {
                continue;
            };
            cx.tick()?;
            let Some(row) = it.next() else {
                self.cur = None;
                continue;
            };
            let row = row?;
            let part = part.clone();
            let out = match &part {
                Part::Movements { .. } => self.movement(&part, &row),
                Part::Totals { .. } => match &self.step.totals {
                    Some(t) => self.stored(&part, t, &row),
                    None => None,
                },
                Part::Turnovers { .. } => match &self.step.turnovers {
                    Some(t) => self.stored(&part, t, &row),
                    None => None,
                },
            };
            if let Some(out) = out {
                return Ok(Some(out));
            }
        }
    }
}

pub fn tables_read(step: &RegisterRowsL, s: &RegisterSettings) -> Vec<TableId> {
    let mut bounds = [None, None];
    for (slot, b) in bounds.iter_mut().zip(&step.bounds) {
        if let Some(query_ir::PExpr::Lit(v)) = b {
            *slot = Some(v.clone());
        }
    }
    let mut out = vec![step.movements];
    if let Ok(parts) = select_parts(step, &bounds, s) {
        for p in parts {
            match p {
                Part::Totals { .. } => out.extend(step.totals.as_ref().map(|t| t.table)),
                Part::Turnovers { .. } => out.extend(step.turnovers.as_ref().map(|t| t.table)),
                Part::Movements { .. } => {}
            }
        }
    }
    out.dedup();
    out
}
