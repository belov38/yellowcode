use std::collections::HashSet;

use super::layout::{R, Row, internal, later, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RelL};
use crate::ir::{ColSel, ExprKind, RegisterVtS, raw_layout as raw};
use crate::schema::{FieldRole, PeriodField, Periodicity, QFieldId, QTableId, QuerySchema, VtKind};
use crate::types::{DateQ, QType};
use crate::value::QValue;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, TableId, TableInfo};
use query_syntax::ast::CmpOp;
use query_syntax::{Func, Kw};

#[derive(Clone, Debug, PartialEq)]
pub struct RegisterRowsL {
    pub register: brace::Guid,
    pub kind: VtKind,
    pub balance_register: bool,
    pub movements: TableId,
    pub period: FieldId,
    pub recorder: FieldId,
    pub line: FieldId,
    pub active: FieldId,
    pub record_kind: Option<FieldId>,
    pub dims: Vec<FieldId>,
    pub resources: Vec<FieldId>,
    pub totals: Option<StoredTotals>,
    pub turnovers: Option<StoredTotals>,
    pub bounds: [Option<PExpr>; 2],
    pub coarse: bool,
    pub periodic: bool,
    pub needed_dims: Vec<bool>,
    pub view: Option<query_syntax::Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredTotals {
    pub table: TableId,
    pub period: FieldId,
    pub dims: Vec<Option<FieldId>>,
    pub resources: Vec<FieldId>,
}

pub(crate) struct VtShape {
    pub dims: Vec<QFieldId>,
    pub resources: Vec<(brace::Guid, Vec<(QFieldId, usize)>)>,
    pub period: Vec<(QFieldId, PeriodField)>,
}

fn component(name: &str) -> Option<usize> {
    for (suffix, c) in [
        ("НачальныйОстаток", raw::BALANCE),
        ("КонечныйОстаток", raw::CLOSING),
        ("Остаток", raw::BALANCE),
        ("Приход", raw::RECEIPT),
        ("Расход", raw::EXPENSE),
        ("Оборот", raw::TURNOVER),
    ] {
        if name.ends_with(suffix) {
            return Some(c);
        }
    }
    None
}

pub(crate) fn vt_shape(schema: &QuerySchema, t: QTableId) -> VtShape {
    let mut out = VtShape {
        dims: Vec::new(),
        resources: Vec::new(),
        period: Vec::new(),
    };
    for (i, f) in schema.table(t).fields.iter().enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        let id = QFieldId(i);
        if let Some(p) = f.period {
            out.period.push((id, p));
            continue;
        }
        if f.role == FieldRole::Virtual
            && let (Some(md), Some(c)) = (f.md, component(&f.name_ru))
        {
            match out.resources.iter_mut().find(|(m, _)| *m == md) {
                Some((_, fs)) => fs.push((id, c)),
                None => out.resources.push((md, vec![(id, c)])),
            }
            continue;
        }
        out.dims.push(id);
    }
    out
}

fn field_by_md(info: &TableInfo, md: brace::Guid) -> Option<FieldId> {
    info.fields
        .iter()
        .position(|f| f.md == Some(md))
        .and_then(|i| u16::try_from(i).ok())
        .map(FieldId)
}

fn std_field(info: &TableInfo, s: StdField) -> R<FieldId> {
    match info.field_by_std(s) {
        Some(f) => Ok(f),
        None => internal("a register table without a standard field"),
    }
}

pub(super) fn bound_date(v: &QValue) -> Option<bsl_prim::BslDate> {
    match v {
        QValue::Date(d) if *d != bsl_prim::BslDate::EMPTY => Some(*d),
        QValue::PointInTime(m) if m.date != bsl_prim::BslDate::EMPTY => Some(m.date),
        QValue::Boundary(b) => bound_date(&b.point),
        _ => None,
    }
}

fn number() -> QType {
    QType::num(crate::types::NumQ {
        digits: 0,
        scale: 0,
        nonneg: false,
    })
}

impl Lower<'_> {
    fn stored_totals(
        &self,
        md: brace::Guid,
        role_name: &str,
        dim_mds: &[Option<brace::Guid>],
        res_mds: &[brace::Guid],
    ) -> R<Option<StoredTotals>> {
        let Some(t) = self
            .map
            .object(md)
            .and_then(|o| o.service.iter().find(|(p, _)| p == role_name))
            .map(|(_, t)| *t)
        else {
            return Ok(None);
        };
        let info = self.map.table(t);
        let mut resources = Vec::with_capacity(res_mds.len());
        for m in res_mds {
            match field_by_md(info, *m) {
                Some(f) => resources.push(f),
                None => return internal("a resource missing from the totals table"),
            }
        }
        Ok(Some(StoredTotals {
            table: t,
            period: std_field(info, StdField::Period)?,
            dims: dim_mds
                .iter()
                .map(|m| m.and_then(|m| field_by_md(info, m)))
                .collect(),
            resources,
        }))
    }

    pub(super) fn register_vt(&mut self, v: &RegisterVtS) -> R<Row> {
        if !matches!(
            v.kind,
            VtKind::Balance | VtKind::Turnovers | VtKind::BalanceAndTurnovers
        ) {
            return later("виртуальная таблица этого вида", "Q5");
        }
        let shape = vt_shape(self.schema, v.table);
        let reg = self.schema.table(v.register);
        let Some(md) = reg.object else {
            return internal("a register without a metadata uuid");
        };
        let Some(mv) = self.storage.table(v.register) else {
            return internal("a register not bound to storage");
        };
        let info = self.map.table(mv);
        let vt = self.schema.table(v.table);
        let dim_mds: Vec<Option<brace::Guid>> = shape
            .dims
            .iter()
            .map(|f| vt.fields.get(usize::from(f.0)).and_then(|x| x.md))
            .collect();
        let mut dims = Vec::with_capacity(dim_mds.len());
        for m in &dim_mds {
            match m.and_then(|m| field_by_md(info, m)) {
                Some(f) => dims.push(f),
                None => return later("поле виртуальной таблицы без поля регистра", "Q5"),
            }
        }
        let res_mds: Vec<brace::Guid> = shape.resources.iter().map(|(m, _)| *m).collect();
        let mut resources = Vec::with_capacity(res_mds.len());
        for m in &res_mds {
            match field_by_md(info, *m) {
                Some(f) => resources.push(f),
                None => return internal("a resource missing from the movement table"),
            }
        }
        let record_kind = info.field_by_std(StdField::RecordKind);
        let balance_register = record_kind.is_some();
        let mut bounds: [Option<PExpr>; 2] = [None, None];
        for (slot, b) in bounds.iter_mut().zip(&v.bounds) {
            let Some(b) = b else { continue };
            let mut empty = Row::empty();
            let p = self.expr(b, &mut empty)?;
            if empty.width != 0 {
                return later("граница виртуальной таблицы, читающая таблицы", "Q5");
            }
            *slot = Some(p);
        }
        let start: Option<PExpr> = match (v.kind, &bounds[0]) {
            (VtKind::Balance, _) | (_, None) => None,
            (_, Some(PExpr::Lit(x))) => bound_date(x).map(|d| PExpr::Lit(QValue::Date(d))),
            (_, Some(e)) => Some(e.clone()),
        };
        let used: HashSet<QFieldId> = self
            .vt_used
            .get(v.src.0 as usize)
            .map(|u| u.iter().copied().collect())
            .unwrap_or_default();
        let mut in_cond: HashSet<QFieldId> = HashSet::new();
        if let Some(c) = &v.cond {
            crate::ir::visit_expr_in_place(c, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(f),
                } = &e.kind
                    && *src == v.inner
                {
                    in_cond.insert(*f);
                }
            });
        }
        if let Some(condition) = self.b.security.conditions.get(&v.src) {
            crate::ir::visit_expr_in_place(condition, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } = e.kind
                    && src == v.src
                {
                    in_cond.insert(field);
                }
            });
        }
        let needed_dims: Vec<bool> = shape
            .dims
            .iter()
            .map(|f| used.contains(f) || in_cond.contains(f))
            .collect();
        let forced = |p: PeriodField| match v.periodicity {
            Periodicity::Unit(_) => p == PeriodField::Period,
            Periodicity::Recorder => matches!(p, PeriodField::Period | PeriodField::Recorder),
            Periodicity::Record => matches!(
                p,
                PeriodField::Period | PeriodField::Recorder | PeriodField::LineNumber
            ),
            Periodicity::Absent | Periodicity::Auto => false,
        };
        let used_period: Vec<(QFieldId, PeriodField)> = shape
            .period
            .iter()
            .copied()
            .filter(|(f, p)| used.contains(f) || forced(*p))
            .collect();
        let flat_auto = v.kind == VtKind::BalanceAndTurnovers
            && v.periodicity == Periodicity::Auto
            && used_period.is_empty();
        let coarse = match v.periodicity {
            Periodicity::Absent => true,
            Periodicity::Record | Periodicity::Recorder => false,
            Periodicity::Unit(kw) => coarse_unit(kw),
            Periodicity::Auto => used_period.iter().all(|(_, p)| match p {
                PeriodField::Unit(kw) => coarse_unit(*kw),
                PeriodField::Period => true,
                PeriodField::Recorder | PeriodField::LineNumber => false,
            }),
        };
        let step = RegisterRowsL {
            register: md,
            kind: v.kind,
            balance_register,
            movements: mv,
            period: std_field(info, StdField::Period)?,
            recorder: std_field(info, StdField::Recorder)?,
            line: std_field(info, StdField::LineNo)?,
            active: std_field(info, StdField::Active)?,
            record_kind,
            dims,
            resources,
            totals: if balance_register {
                self.stored_totals(md, role::TOTALS, &dim_mds, &res_mds)?
            } else {
                None
            },
            turnovers: if balance_register {
                None
            } else {
                self.stored_totals(md, role::TURNOVERS, &dim_mds, &res_mds)?
            },
            bounds: bounds.clone(),
            coarse,
            periodic: !matches!(v.periodicity, Periodicity::Absent) && !flat_auto,
            needed_dims,
            view: v.span,
        };
        let nd = shape.dims.len();
        let nr = shape.resources.len();
        self.set_level(v.inner);
        self.inner += 1;
        let mut row = Row {
            signal: None,
            rel: RelL::RegisterRows(Box::new(step)),
            width: raw::width(nd, nr),
            rep: Some(v.inner),
            keys: None,
        };
        if let Some(c) = &v.cond {
            match self.filter_row(row, c) {
                Ok(r) => row = r,
                Err(e) => {
                    self.inner -= 1;
                    return Err(e);
                }
            }
        }
        row = self.secure_raw(row, v.src, v.inner)?;
        self.inner -= 1;
        if v.kind == VtKind::BalanceAndTurnovers
            && v.periodicity != Periodicity::Absent
            && !flat_auto
        {
            return self.running_vt(v, &shape, row.checked(), &used, bounds);
        }
        let col = |i: usize| -> R<PExpr> {
            u16::try_from(i)
                .map(PExpr::Col)
                .or_else(|_| internal("raw column"))
        };
        let cut = |x: PExpr| -> PExpr {
            match &start {
                Some(s) => PExpr::Case {
                    operand: None,
                    whens: vec![(
                        PExpr::Compare(CmpOp::Lt, Box::new(x.clone()), Box::new(s.clone())),
                        s.clone(),
                    )],
                    else_: Some(Box::new(x)),
                },
                None => x,
            }
        };
        let begin = |kw: Kw, x: PExpr| -> PExpr {
            PExpr::Func {
                f: Func::BeginOfPeriod,
                args: vec![x, PExpr::Unit(kw)],
                ty: QType::date(DateQ::DateTime),
                arg_dec: Dec::UNKNOWN,
            }
        };
        let mut keys: Vec<PExpr> = Vec::new();
        let mut key_of: Vec<(QFieldId, u16)> = Vec::new();
        for (f, p) in &used_period {
            let e = match (p, v.periodicity) {
                (PeriodField::Period, Periodicity::Unit(kw)) => cut(begin(kw, col(raw::PERIOD)?)),
                (PeriodField::Period, _) => col(raw::PERIOD)?,
                (PeriodField::Unit(kw), _) => cut(begin(*kw, col(raw::PERIOD)?)),
                (PeriodField::Recorder, _) => col(raw::RECORDER)?,
                (PeriodField::LineNumber, _) => col(raw::LINE)?,
            };
            key_of.push((*f, u16::try_from(keys.len()).or_else(|_| internal("keys"))?));
            keys.push(e);
        }
        for (i, f) in shape.dims.iter().enumerate() {
            if used.contains(f) {
                key_of.push((*f, u16::try_from(keys.len()).or_else(|_| internal("keys"))?));
                keys.push(col(raw::DIMS + i)?);
            }
        }
        let nk = keys.len();
        let mut aggs = Vec::with_capacity(nr * raw::PER_RESOURCE);
        for (r, (_, fields)) in shape.resources.iter().enumerate() {
            let ty = fields
                .first()
                .and_then(|(f, _)| vt.fields.get(usize::from(f.0)))
                .map_or_else(number, |x| x.ty.clone());
            for c in 0..raw::PER_RESOURCE {
                aggs.push(AggL {
                    f: Func::Sum,
                    distinct: false,
                    arg: Some(col(raw::component(nd, r, c))?),
                    ty: ty.clone(),
                    arg_dec: Dec::UNKNOWN,
                });
            }
        }
        let agg_col = |r: usize, c: usize| -> R<PExpr> { col(nk + raw::PER_RESOURCE * r + c) };
        let signal = row
            .signal
            .map(|s| {
                aggs.push(super::security::min(s));
                p16((nk + aggs.len() - 1) as i64)
            })
            .transpose()?;
        row = Row {
            signal,
            width: nk + aggs.len(),
            rel: RelL::Aggregate {
                input: row.take(),
                keys,
                sets: None,
                aggs,
            },
            rep: None,
            keys: Some(nk),
        };
        let mut wanted: Vec<(usize, usize)> = Vec::new();
        for (r, (_, fields)) in shape.resources.iter().enumerate() {
            for (f, c) in fields {
                if used.contains(f) {
                    wanted.push((r, *c));
                }
            }
        }
        let defaults: &[usize] = match v.kind {
            VtKind::Balance => &[raw::BALANCE],
            VtKind::Turnovers => &[raw::RECEIPT, raw::EXPENSE],
            _ => &[raw::BALANCE, raw::RECEIPT, raw::EXPENSE],
        };
        if wanted.is_empty() {
            for r in 0..nr {
                wanted.extend(defaults.iter().map(|c| (r, *c)));
            }
        }
        let one_row = nk == 0 && v.kind != VtKind::Turnovers;
        if !one_row && !wanted.is_empty() {
            let mut ors = Vec::new();
            for (r, c) in &wanted {
                let parts: &[usize] = match *c {
                    raw::CLOSING => &[raw::BALANCE, raw::TURNOVER],
                    other => std::slice::from_ref(match other {
                        raw::BALANCE => &raw::BALANCE,
                        raw::RECEIPT => &raw::RECEIPT,
                        raw::EXPENSE => &raw::EXPENSE,
                        _ => &raw::TURNOVER,
                    }),
                };
                for p in parts {
                    ors.push(PExpr::Compare(
                        CmpOp::Ne,
                        Box::new(agg_col(*r, *p)?),
                        Box::new(PExpr::Lit(QValue::Number(bsl_prim::Number::zero()))),
                    ));
                }
            }
            row.rel = RelL::Filter {
                input: row.take(),
                pred: PExpr::Or(ors),
            };
        }
        let zero_if_null = |x: PExpr| -> PExpr {
            PExpr::Case {
                operand: None,
                whens: vec![(
                    PExpr::IsNull {
                        l: Box::new(x.clone()),
                        not: false,
                    },
                    PExpr::Lit(QValue::Number(bsl_prim::Number::zero())),
                )],
                else_: Some(Box::new(x)),
            }
        };
        let mut exprs = Vec::with_capacity(vt.fields.len());
        for i in 0..vt.fields.len() {
            let f = QFieldId(u16::try_from(i).or_else(|_| internal("fields"))?);
            if let Some((_, k)) = key_of.iter().find(|(x, _)| *x == f) {
                exprs.push(PExpr::Col(*k));
                continue;
            }
            let res =
                shape.resources.iter().enumerate().find_map(|(r, (_, fs))| {
                    fs.iter().find(|(x, _)| *x == f).map(|(_, c)| (r, *c))
                });
            match res {
                Some((r, c)) if used.contains(&f) => exprs.push(zero_if_null(agg_col(r, c)?)),
                _ => exprs.push(PExpr::Lit(QValue::Null)),
            }
        }
        let signal = row
            .signal
            .map(|_| {
                let i = exprs.len();
                exprs.push(row.signal_expr());
                p16(i as i64)
            })
            .transpose()?;
        self.set_level(v.src);
        Ok(Row {
            signal,
            width: exprs.len(),
            rel: RelL::Project {
                input: row.take(),
                exprs,
            },
            rep: Some(v.src),
            keys: None,
        })
    }
}

fn coarse_unit(kw: Kw) -> bool {
    matches!(kw, Kw::Month | Kw::Quarter | Kw::HalfYear | Kw::Year)
}
