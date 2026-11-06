use std::rc::Rc;

use bsl_prim::{BslDate, Number};
use ibschema::storage::{FieldId, TableId};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{AccumulationRowsL, RegisterRowsKindL, RegisterRowsL, StoredTotals};
use query_ir::schema::VtKind;
use query_ir::{ExecError, QValue};

use super::accounting::{self, DrCrSelection};
use super::{Cx, Op, accounting_balance, accounting_turnovers};
use crate::register::{RegisterKind, RegisterSettings};
use crate::source::{Row, RowIter};

fn fatal(s: impl Into<String>) -> ExecError {
    ExecError::Fatal(s.into())
}

use query_ir::value::register_bounds::{
    Bnd, Point, Rec, after_start, before_end, bound_of, in_balance,
};

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
    parts: SelectedParts,
    _charge: Option<super::OwnedCharge>,
}

#[derive(Debug)]
enum SelectedParts {
    Accumulation(Vec<Part>),
    Accounting(DrCrSelection),
    AccountingTurnoversMain,
    AccountingBalance,
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
            .saturating_add(match &self.parts {
                SelectedParts::Accumulation(parts) => {
                    parts.capacity().saturating_mul(std::mem::size_of::<Part>())
                }
                SelectedParts::Accounting(_)
                | SelectedParts::AccountingTurnoversMain
                | SelectedParts::AccountingBalance => 0,
            })
            .saturating_add(usize::try_from(values).unwrap_or(usize::MAX))
    }

    pub(crate) fn tables(&self, step: &RegisterRowsL) -> impl Iterator<Item = TableId> + '_ {
        let (movements, totals, turnovers) = match &step.rows {
            RegisterRowsKindL::Accumulation(a) => (
                Some(a.movements),
                a.totals.as_ref().map(|t| t.table),
                a.turnovers.as_ref().map(|t| t.table),
            ),
            RegisterRowsKindL::Accounting(_)
            | RegisterRowsKindL::AccountingTurnovers(_)
            | RegisterRowsKindL::AccountingBalance(_) => (None, None, None),
        };
        let (parts, accounting) = match &self.parts {
            SelectedParts::Accumulation(parts) => (parts.as_slice(), [None, None]),
            SelectedParts::AccountingTurnoversMain => {
                let table = match &step.rows {
                    RegisterRowsKindL::AccountingTurnovers(a) => Some(a.inputs.main),
                    _ => None,
                };
                (&[][..], [table, None])
            }
            SelectedParts::AccountingBalance => {
                let tables = match &step.rows {
                    RegisterRowsKindL::AccountingBalance(a) => {
                        [Some(a.inputs.main), Some(a.inputs.chart)]
                    }
                    _ => [None, None],
                };
                (&[][..], tables)
            }
            SelectedParts::Accounting(mode) => {
                let tables = match (&step.rows, mode) {
                    (RegisterRowsKindL::Accounting(a), DrCrSelection::Main) => {
                        [Some(a.inputs.main), None]
                    }
                    (RegisterRowsKindL::Accounting(a), DrCrSelection::CtAndMain { .. }) => {
                        [Some(a.inputs.ct), Some(a.inputs.main)]
                    }
                    _ => [None, None],
                };
                (&[][..], tables)
            }
        };
        parts
            .iter()
            .filter_map(move |part| match part {
                Part::Movements { .. } => movements,
                Part::Totals { .. } => totals,
                Part::Turnovers { .. } => turnovers,
            })
            .chain(accounting.into_iter().flatten())
    }
}

pub struct RegisterRows<'a> {
    step: RegisterRowsL,
    chart: Option<super::BoxOp<'a>>,
    selected: Option<Rc<SelectedRegister>>,
    cursor: usize,
    observation: Option<crate::observe::RegisterObservation>,
    cur: Option<(Part, RowIter<'a>)>,
    accounting: Option<accounting::Cursor<'a>>,
    turnovers: Option<accounting_turnovers::Cursor<'a>>,
    balance: Option<accounting_balance::Cursor<'a>>,
}

impl<'a> RegisterRows<'a> {
    pub(crate) fn with_chart(mut self, chart: Option<super::BoxOp<'a>>) -> Self {
        self.chart = chart;
        self
    }

    pub fn new(step: RegisterRowsL) -> RegisterRows<'a> {
        Self::observed(step, None)
    }

    pub(crate) fn observed(
        step: RegisterRowsL,
        observation: Option<crate::observe::RegisterObservation>,
    ) -> RegisterRows<'a> {
        RegisterRows {
            step,
            chart: None,
            selected: None,
            cursor: 0,
            observation,
            cur: None,
            accounting: None,
            turnovers: None,
            balance: None,
        }
    }
}

fn select_parts(
    st: &RegisterRowsL,
    bounds: &[Option<QValue>; 2],
    s: &RegisterSettings,
) -> Result<Vec<Part>, ExecError> {
    let RegisterRowsKindL::Accumulation(stored) = &st.rows else {
        return Err(fatal("accounting DrCrTurnovers: execution is not enabled"));
    };
    let b0 = bound_of(bounds[0].as_ref());
    let b1 = bound_of(bounds[1].as_ref());
    let mut parts = Vec::new();
    let balance_at = |b: Bnd, parts: &mut Vec<Part>| -> Result<(), ExecError> {
        balance_parts(stored, s, b, parts)
    };
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
            let months = turnover_months(stored, s, b0, b1);
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
    st: &AccumulationRowsL,
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
    st: &AccumulationRowsL,
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
        self.step.raw_width()
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
        if let Some(x) = row.get_mut(raw::component(
            match &self.step.rows {
                RegisterRowsKindL::Accumulation(a) => a.dims.len(),
                RegisterRowsKindL::Accounting(a) => a.keys.len(),
                RegisterRowsKindL::AccountingTurnovers(a) => usize::from(a.account),
                RegisterRowsKindL::AccountingBalance(_) => 2,
            },
            r,
            c,
        )) {
            *x = QValue::Number(v);
        }
    }

    fn movement(&self, part: &Part, m: &Row) -> Option<Row> {
        let RegisterRowsKindL::Accumulation(st) = &self.step.rows else {
            return None;
        };
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
        if *balance && st.periodic && self.step.kind == VtKind::BalanceAndTurnovers {
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
        let RegisterRowsKindL::Accumulation(st) = &self.step.rows else {
            return Err(fatal(
                "accounting DrCrTurnovers: invalid accumulation cursor",
            ));
        };
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
        if matches!(&self.step.rows, RegisterRowsKindL::AccountingBalance(_)) {
            return self.next_balance(cx);
        }
        if matches!(&self.step.rows, RegisterRowsKindL::AccountingTurnovers(_)) {
            return self.next_turnovers(cx);
        }
        if matches!(&self.step.rows, RegisterRowsKindL::Accounting(_)) {
            return self.next_accounting(cx);
        }
        if self.selected.is_none() {
            let mut bounds = [None, None];
            for (slot, b) in bounds.iter_mut().zip(self.step.bounds()) {
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
                parts: SelectedParts::Accumulation(parts),
                _charge: None,
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
                    .and_then(|s| match &s.parts {
                        SelectedParts::Accumulation(parts) => parts.get(self.cursor),
                        SelectedParts::Accounting(_)
                        | SelectedParts::AccountingTurnoversMain
                        | SelectedParts::AccountingBalance => None,
                    })
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
                Part::Totals { .. } => match &self.step.rows {
                    RegisterRowsKindL::Accumulation(st) => match &st.totals {
                        Some(t) => self.stored(&part, t, &row),
                        None => None,
                    },
                    RegisterRowsKindL::Accounting(_)
                    | RegisterRowsKindL::AccountingTurnovers(_)
                    | RegisterRowsKindL::AccountingBalance(_) => None,
                },
                Part::Turnovers { .. } => match &self.step.rows {
                    RegisterRowsKindL::Accumulation(st) => match &st.turnovers {
                        Some(t) => self.stored(&part, t, &row),
                        None => None,
                    },
                    RegisterRowsKindL::Accounting(_)
                    | RegisterRowsKindL::AccountingTurnovers(_)
                    | RegisterRowsKindL::AccountingBalance(_) => None,
                },
            };
            if let Some(out) = out {
                return Ok(Some(out));
            }
        }
    }

    fn close(&mut self, cx: &mut Cx<'a>) {
        if let Some(mut chart) = self.chart.take() {
            chart.close(cx);
        }
        if let Some(cursor) = self.balance.as_mut() {
            cursor.close(cx);
            self.selected = None;
        }
        if let Some(cursor) = self.turnovers.as_mut() {
            cursor.close();
            self.selected = None;
        }
        if let Some(cursor) = self.accounting.as_mut() {
            cursor.close(cx);
        }
    }
}

impl<'a> RegisterRows<'a> {
    fn next_balance(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let RegisterRowsKindL::AccountingBalance(step) = &self.step.rows else {
            return Err(fatal("accounting Balance: missing payload"));
        };
        if step.chart_input.is_some()
            || (self.selected.is_none() && step.chart_layout.is_some() != self.chart.is_some())
        {
            return Err(fatal(
                "accounting Balance: inconsistent secured chart child",
            ));
        }
        if self.step.kind != VtKind::Balance || step.resources.is_empty() {
            return Err(fatal("accounting Balance: invalid kind or resource layout"));
        }
        if self.selected.is_none() {
            accounting::check_cancel(cx.env)?;
            let settings = crate::register::read_register_settings_with_observation(
                cx.src,
                self.step.register,
                RegisterKind::Accounting,
                cx.observation.as_ref(),
            )?;
            accounting::check_cancel(cx.env)?;
            if !settings.use_totals {
                return Err(totals_off());
            }
            accounting_balance::check_balance_settings_shape(&settings)?;
            let bytes = std::mem::size_of::<SelectedRegister>()
                .checked_add(2 * std::mem::size_of::<usize>())
                .ok_or_else(|| fatal("accounting Balance: selection model overflow"))?;
            let charge = cx.mem.hold_owned(
                u64::try_from(bytes)
                    .map_err(|_| fatal("accounting Balance: selection model overflow"))?,
            )?;
            let selected = Rc::new(SelectedRegister {
                bounds: [step.date.map(QValue::Date), None],
                settings,
                parts: SelectedParts::AccountingBalance,
                _charge: Some(charge),
            });
            if let (Some(observation), Some(session)) = (&self.observation, &cx.observation) {
                observation.publish(session, &self.step, selected.clone());
            }
            self.selected = Some(selected);
            self.balance = Some(accounting_balance::Cursor::new(step, cx)?);
        }
        self.balance
            .as_mut()
            .ok_or_else(|| fatal("accounting Balance: missing cursor"))?
            .next(step, &mut self.chart, cx)
    }

    fn next_turnovers(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let RegisterRowsKindL::AccountingTurnovers(step) = &self.step.rows else {
            return Err(fatal("accounting Turnovers: missing payload"));
        };
        if self.step.kind != VtKind::Turnovers || step.resources.is_empty() {
            return Err(fatal("accounting Turnovers: invalid kind or resources"));
        }
        if self.selected.is_none() {
            accounting::check_cancel(cx.env)?;
            let settings = crate::register::read_register_settings_with_observation(
                cx.src,
                self.step.register,
                RegisterKind::Accounting,
                cx.observation.as_ref(),
            )?;
            accounting::check_cancel(cx.env)?;
            if !settings.use_totals {
                return Err(totals_off());
            }
            let bytes = std::mem::size_of::<SelectedRegister>()
                .checked_add(2 * std::mem::size_of::<usize>())
                .ok_or_else(|| fatal("accounting Turnovers: selection model overflow"))?;
            let charge = cx.mem.hold_owned(
                u64::try_from(bytes)
                    .map_err(|_| fatal("accounting Turnovers: selection model overflow"))?,
            )?;
            let selected = Rc::new(SelectedRegister {
                bounds: step.dates.map(|date| date.map(QValue::Date)),
                settings,
                parts: SelectedParts::AccountingTurnoversMain,
                _charge: Some(charge),
            });
            if let (Some(observation), Some(session)) = (&self.observation, &cx.observation) {
                observation.publish(session, &self.step, selected.clone());
            }
            self.selected = Some(selected);
            self.turnovers = Some(accounting_turnovers::Cursor::new(step, cx)?);
        }
        self.turnovers
            .as_mut()
            .ok_or_else(|| fatal("accounting Turnovers: missing cursor"))?
            .next(step, cx)
    }

    fn next_accounting(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let RegisterRowsKindL::Accounting(step) = &self.step.rows else {
            return Err(fatal(
                "accounting DrCrTurnovers: missing accounting payload",
            ));
        };
        if self.step.kind != VtKind::DrCrTurnovers
            || step.resources.is_empty()
            || !matches!(
                step.keys.as_slice(),
                [] | [query_ir::lower::DrCrSideL::Dr]
                    | [query_ir::lower::DrCrSideL::Cr]
                    | [
                        query_ir::lower::DrCrSideL::Dr,
                        query_ir::lower::DrCrSideL::Cr
                    ]
            )
        {
            return Err(fatal(
                "accounting DrCrTurnovers: invalid kind or raw layout",
            ));
        }
        if self.selected.is_none() {
            accounting::check_cancel(cx.env)?;
            let settings = crate::register::read_register_settings_with_observation(
                cx.src,
                self.step.register,
                RegisterKind::Accounting,
                cx.observation.as_ref(),
            )?;
            accounting::check_cancel(cx.env)?;
            if !settings.use_totals {
                return Err(totals_off());
            }
            let mode =
                accounting::selection(&settings, step.date_bounds.iter().any(Option::is_some))?;
            let bytes = std::mem::size_of::<SelectedRegister>()
                .checked_add(2 * std::mem::size_of::<usize>())
                .ok_or_else(|| fatal("accounting DrCrTurnovers: selection model overflow"))?;
            let charge =
                cx.mem
                    .hold_owned(u64::try_from(bytes).map_err(|_| {
                        fatal("accounting DrCrTurnovers: selection model overflow")
                    })?)?;
            let selected = Rc::new(SelectedRegister {
                bounds: step.date_bounds.map(|date| date.map(QValue::Date)),
                settings,
                parts: SelectedParts::Accounting(mode),
                _charge: Some(charge),
            });
            if let (Some(observation), Some(session)) = (&self.observation, &cx.observation) {
                observation.publish(session, &self.step, selected.clone());
            }
            self.selected = Some(selected);
            self.accounting = Some(accounting::Cursor::new(step, cx)?);
        }
        let mode = match self.selected.as_ref().map(|s| &s.parts) {
            Some(SelectedParts::Accounting(mode)) => *mode,
            _ => return Err(fatal("accounting DrCrTurnovers: invalid selection owner")),
        };
        let cursor = self
            .accounting
            .as_mut()
            .ok_or_else(|| fatal("accounting DrCrTurnovers: missing cursor"))?;
        cursor.next(step, mode, cx)
    }
}

pub fn tables_read(step: &RegisterRowsL, s: &RegisterSettings) -> Vec<TableId> {
    if let RegisterRowsKindL::AccountingBalance(a) = &step.rows {
        return if s.use_totals && accounting_balance::check_balance_settings_shape(s).is_ok() {
            vec![a.inputs.main, a.inputs.chart]
        } else {
            Vec::new()
        };
    }
    if let RegisterRowsKindL::AccountingTurnovers(a) = &step.rows {
        return if s.use_totals {
            vec![a.inputs.main]
        } else {
            Vec::new()
        };
    }
    let RegisterRowsKindL::Accumulation(a) = &step.rows else {
        let RegisterRowsKindL::Accounting(a) = &step.rows else {
            return Vec::new();
        };
        if !s.use_totals {
            return Vec::new();
        }
        return match accounting::selection(s, a.date_bounds.iter().any(Option::is_some)) {
            Ok(DrCrSelection::Main) => vec![a.inputs.main],
            Ok(DrCrSelection::CtAndMain { .. }) => vec![a.inputs.ct, a.inputs.main],
            Err(_) => Vec::new(),
        };
    };
    let mut bounds = [None, None];
    for (slot, b) in bounds.iter_mut().zip(step.bounds()) {
        if let Some(query_ir::PExpr::Lit(v)) = b {
            *slot = Some(v.clone());
        }
    }
    let mut out = vec![a.movements];
    if let Ok(parts) = select_parts(step, &bounds, s) {
        for p in parts {
            match p {
                Part::Totals { .. } => out.extend(a.totals.as_ref().map(|t| t.table)),
                Part::Turnovers { .. } => out.extend(a.turnovers.as_ref().map(|t| t.table)),
                Part::Movements { .. } => {}
            }
        }
    }
    out.dedup();
    out
}
