use super::layout::{R, Row, internal, later};
use super::rel::Lower;
use super::vt::VtShape;
use super::{AggL, Dec, PExpr, RelL};
use crate::ir::{RegisterVtS, raw_layout as raw};
use crate::schema::{Complement, PeriodField, Periodicity, QFieldId};
use crate::types::DateQ;
use crate::{QType, QValue};
use query_syntax::ast::CmpOp;
use query_syntax::{Func, Kw};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct RunningBalanceL {
    pub dims: usize,
    pub resources: usize,
    pub period: PExpr,
    pub period_fields: Vec<PExpr>,
    pub bounds: [Option<PExpr>; 2],
    pub boundaries: bool,
    pub wanted: Vec<(usize, usize)>,
}
impl RunningBalanceL {
    pub fn width(&self) -> usize {
        raw::width(self.dims, self.resources) + self.period_fields.len()
    }
}
fn col(i: usize) -> R<PExpr> {
    u16::try_from(i)
        .map(PExpr::Col)
        .or_else(|_| internal("running balance column"))
}
fn zero() -> PExpr {
    PExpr::Lit(QValue::Number(bsl_prim::Number::zero()))
}
fn begin(kw: Kw, x: PExpr) -> PExpr {
    PExpr::Func {
        f: Func::BeginOfPeriod,
        args: vec![x, PExpr::Unit(kw)],
        ty: QType::date(DateQ::DateTime),
        arg_dec: Dec::UNKNOWN,
    }
}
fn max(a: PExpr, b: PExpr) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::Compare(CmpOp::Lt, Box::new(a.clone()), Box::new(b.clone())),
            b,
        )],
        else_: Some(Box::new(a)),
    }
}
fn opening_or(e: PExpr) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::IsNull {
                l: Box::new(PExpr::Col(0)),
                not: false,
            },
            PExpr::Lit(QValue::Null),
        )],
        else_: Some(Box::new(e)),
    }
}

impl Lower<'_> {
    pub(super) fn running_vt(
        &mut self,
        v: &RegisterVtS,
        shape: &VtShape,
        mut input: Row,
        used: &HashSet<QFieldId>,
        bounds: [Option<PExpr>; 2],
    ) -> R<Row> {
        let vt = self.schema.table(v.table);
        let chosen: Vec<_> = shape
            .dims
            .iter()
            .enumerate()
            .filter(|(_, f)| used.contains(f))
            .collect();
        let nd = chosen.len();
        let nr = shape.resources.len();
        let fields: Vec<_> = shape
            .period
            .iter()
            .copied()
            .filter(|(f, _)| used.contains(f))
            .collect();
        let (recorder, line, period) = match v.periodicity {
            Periodicity::Unit(kw) => (false, false, begin(kw, col(0)?)),
            Periodicity::Recorder => (true, false, col(0)?),
            Periodicity::Record => (true, true, col(0)?),
            Periodicity::Auto => {
                let line = fields.iter().any(|(_, p)| *p == PeriodField::LineNumber);
                let rec = line || fields.iter().any(|(_, p)| *p == PeriodField::Recorder);
                let units: Vec<_> = fields
                    .iter()
                    .filter_map(|(_, p)| match p {
                        PeriodField::Unit(kw) => Some(*kw),
                        _ => None,
                    })
                    .collect();
                let period = if rec {
                    col(0)?
                } else {
                    let finer = [Kw::Second, Kw::Minute, Kw::Hour, Kw::Day]
                        .iter()
                        .any(|kw| units.contains(kw));
                    let coarse = [Kw::TenDays, Kw::Month, Kw::Quarter, Kw::HalfYear, Kw::Year]
                        .iter()
                        .any(|kw| units.contains(kw));
                    if units.contains(&Kw::Week) && coarse && !finer {
                        return later(
                            "Авто Неделя с невложенными календарными периодами в ОстаткиИОбороты",
                            "Q5b",
                        );
                    }
                    let unit = [
                        Kw::Second,
                        Kw::Minute,
                        Kw::Hour,
                        Kw::Day,
                        Kw::Week,
                        Kw::TenDays,
                        Kw::Month,
                        Kw::Quarter,
                        Kw::HalfYear,
                        Kw::Year,
                    ]
                    .into_iter()
                    .find(|kw| units.contains(kw));
                    let Some(unit) = unit else {
                        return later("Авто без полей периода в ОстаткиИОбороты", "Q5b");
                    };
                    begin(unit, PExpr::Col(0))
                };
                (rec, line, period)
            }
            Periodicity::Absent => return internal("nonperiodic running balance"),
        };
        let start = match &bounds[0] {
            Some(PExpr::Lit(v)) => super::vt::bound_date(v).map(|d| PExpr::Lit(QValue::Date(d))),
            Some(e) => Some(e.clone()),
            None => None,
        };
        let cut = |e: PExpr| match &start {
            Some(s) => max(e, s.clone()),
            None => e,
        };
        let mut period_fields = Vec::new();
        for (_, p) in &fields {
            period_fields.push(match p {
                PeriodField::Period => col(0)?,
                PeriodField::Unit(kw) => cut(begin(*kw, col(0)?)),
                PeriodField::Recorder => col(1)?,
                PeriodField::LineNumber => col(2)?,
            });
        }
        let mut keys = vec![
            opening_or(cut(period.clone())),
            if recorder {
                opening_or(col(1)?)
            } else {
                PExpr::Lit(QValue::Undefined)
            },
            if line { opening_or(col(2)?) } else { zero() },
        ];
        for (i, _) in &chosen {
            keys.push(col(raw::DIMS + *i)?);
        }
        for (_, p) in &fields {
            keys.push(opening_or(match p {
                PeriodField::Period => cut(period.clone()),
                PeriodField::Unit(kw) => cut(begin(*kw, col(0)?)),
                PeriodField::Recorder => {
                    if recorder {
                        col(1)?
                    } else {
                        PExpr::Lit(QValue::Undefined)
                    }
                }
                PeriodField::LineNumber => {
                    if line {
                        col(2)?
                    } else {
                        zero()
                    }
                }
            }));
        }
        let nk = keys.len();
        let mut aggs = Vec::new();
        for (r, (_, fs)) in shape.resources.iter().enumerate() {
            let ty = fs
                .first()
                .and_then(|(f, _)| vt.fields.get(usize::from(f.0)))
                .map(|f| f.ty.clone())
                .unwrap_or_default();
            for c in 0..raw::PER_RESOURCE {
                aggs.push(AggL {
                    f: Func::Sum,
                    distinct: false,
                    arg: Some(col(raw::component(shape.dims.len(), r, c))?),
                    ty: ty.clone(),
                    arg_dec: Dec::UNKNOWN,
                });
            }
        }
        input.rel = RelL::Aggregate {
            input: input.take(),
            keys,
            sets: None,
            aggs,
        };
        let mut reorder: Vec<PExpr> = (0..raw::DIMS + nd).map(col).collect::<R<_>>()?;
        reorder.extend(
            (0..nr * raw::PER_RESOURCE)
                .map(|i| col(nk + i))
                .collect::<R<Vec<_>>>()?,
        );
        reorder.extend((raw::DIMS + nd..nk).map(col).collect::<R<Vec<_>>>()?);
        input.rel = RelL::Project {
            input: input.take(),
            exprs: reorder,
        };
        let mut sort: Vec<_> = (0..nd)
            .map(|i| col(raw::DIMS + i).map(|e| (e, false)))
            .collect::<R<_>>()?;
        sort.extend(
            (0..3)
                .map(|i| col(i).map(|e| (e, false)))
                .collect::<R<Vec<_>>>()?,
        );
        sort.extend(
            (0..fields.len())
                .map(|i| col(raw::width(nd, nr) + i).map(|e| (e, false)))
                .collect::<R<Vec<_>>>()?,
        );
        input.rel = RelL::Sort {
            input: input.take(),
            keys: sort,
        };
        let wanted: Vec<_> = shape
            .resources
            .iter()
            .enumerate()
            .flat_map(|(r, (_, fs))| {
                fs.iter()
                    .filter(|(f, _)| used.contains(f))
                    .map(move |(_, c)| (r, *c))
            })
            .collect();
        if wanted.is_empty() {
            return later("периодическая таблица без выбранных ресурсов", "Q5b");
        }
        let step = RunningBalanceL {
            dims: nd,
            resources: nr,
            period,
            period_fields,
            bounds,
            boundaries: v.complement != Some(Complement::Movements),
            wanted,
        };
        input.width = step.width();
        input.rel = RelL::RunningBalance {
            input: input.take(),
            step: Box::new(step),
        };
        let mut output = Vec::new();
        for i in 0..vt.fields.len() {
            let id = QFieldId(u16::try_from(i).or_else(|_| internal("VT field"))?);
            let expr = if let Some(j) = chosen.iter().position(|(_, f)| **f == id) {
                col(raw::DIMS + j)?
            } else if let Some(j) = fields.iter().position(|(f, _)| *f == id) {
                col(raw::width(nd, nr) + j)?
            } else if let Some((r, c)) =
                shape.resources.iter().enumerate().find_map(|(r, (_, fs))| {
                    fs.iter().find(|(f, _)| *f == id).map(|(_, c)| (r, *c))
                })
            {
                if used.contains(&id) {
                    col(raw::component(nd, r, c))?
                } else {
                    PExpr::Lit(QValue::Null)
                }
            } else {
                PExpr::Lit(QValue::Null)
            };
            output.push(expr);
        }
        self.set_level(v.src);
        Ok(Row {
            signal: None,
            rel: RelL::Project {
                input: input.take(),
                exprs: output,
            },
            width: vt.fields.len(),
            rep: Some(v.src),
            keys: None,
        })
    }
}
