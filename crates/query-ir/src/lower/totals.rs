use ibschema::names::StdField;
use ibschema::storage::StorageMap;

use crate::bind::{Bound, PBound};
use crate::exec_error::ExecError;
use crate::ir::{Expr, ExprKind};
use crate::schema::{FieldRole, StdRole};
use crate::totals::{HierTableSpec, PeriodsSpec, PointSpec, TotalsExpr, TotalsS, TotalsSpec};
use crate::value::QValue;
use crate::{QuerySchema, StorageBinding};

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q4" }
}

fn param(b: &Bound<'_>, slot: u16) -> Result<QValue, ExecError> {
    match b.params.get(usize::from(slot)) {
        Some(PBound::Value(v)) => Ok(v.clone()),
        Some(_) => Err(later("ИТОГИ: параметр-коллекция")),
        None => Err(ExecError::Fatal("totals: a missing parameter slot".into())),
    }
}

fn constant(b: &Bound<'_>, e: &Expr) -> Result<QValue, ExecError> {
    match &e.kind {
        ExprKind::Lit(v) => Ok(v.clone()),
        ExprKind::Param(slot) => param(b, *slot),
        ExprKind::Value(slot) => b
            .values
            .get(usize::from(*slot))
            .cloned()
            .ok_or_else(|| ExecError::Fatal("totals: a missing value slot".into())),
        _ => Err(later("ИТОГИ: граница ПЕРИОДАМИ — выражение")),
    }
}

fn date(b: &Bound<'_>, e: &Option<Expr>) -> Result<Option<bsl_prim::BslDate>, ExecError> {
    match e {
        None => Ok(None),
        Some(e) => Ok(match constant(b, e)? {
            QValue::Date(d) => Some(d),
            _ => None,
        }),
    }
}

fn expr(b: &Bound<'_>, e: &TotalsExpr) -> Result<TotalsExpr, ExecError> {
    let mut result = e.clone();
    let mut pending = vec![&mut result];
    while let Some(e) = pending.pop() {
        match e {
            TotalsExpr::Param(slot) => *e = TotalsExpr::Const(param(b, *slot)?),
            TotalsExpr::Neg(x) | TotalsExpr::Not(x) => pending.push(x.as_mut()),
            TotalsExpr::Bin(_, l, r) | TotalsExpr::Cmp(_, l, r) | TotalsExpr::IsNull(l, r) => {
                pending.push(r.as_mut());
                pending.push(l.as_mut());
            }
            TotalsExpr::And(xs) | TotalsExpr::Or(xs) => pending.extend(xs.iter_mut().rev()),
            TotalsExpr::Case { whens, otherwise } => {
                if let Some(x) = otherwise {
                    pending.push(x.as_mut());
                }
                for (condition, value) in whens.iter_mut().rev() {
                    pending.push(value);
                    pending.push(condition);
                }
            }
            TotalsExpr::Agg(_) | TotalsExpr::Column(_) | TotalsExpr::Const(_) => {}
        }
    }
    drop(pending);
    Ok(result)
}

pub(super) fn lower_totals(
    t: &TotalsS,
    b: &Bound<'_>,
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    week_start: Option<u32>,
) -> Result<TotalsSpec, ExecError> {
    let mut points = Vec::with_capacity(t.points.len());
    for p in &t.points {
        let mut hier = Vec::with_capacity(p.hier.len());
        for h in &p.hier {
            let (Some(number), Some(table)) = (storage.ref_number(h.atom), storage.table(h.table))
            else {
                continue;
            };
            if schema.is_unstored(h.table) {
                continue;
            }
            let Some(info) = map.try_table(table) else {
                continue;
            };
            let (Some(id), Some(parent)) = (
                info.field_by_std(StdField::Id),
                info.field_by_std(StdField::ParentId),
            ) else {
                continue;
            };
            let keys = h
                .keys
                .iter()
                .map(|k| {
                    k.and_then(|(qf, not)| {
                        let f = storage.field(h.table, qf)?;
                        let folder = schema
                            .table(h.table)
                            .fields
                            .get(usize::from(qf.0))
                            .is_some_and(|x| x.role == FieldRole::Std(StdRole::IsFolder));
                        Some((f, not ^ folder))
                    })
                })
                .collect();
            hier.push(HierTableSpec {
                number,
                table,
                id,
                parent,
                folder: info.field_by_std(StdField::Folder),
                keys,
            });
        }
        let periods = match &p.periods {
            Some(per) => Some(PeriodsSpec {
                unit: per.unit,
                begin: date(b, &per.begin)?,
                end: date(b, &per.end)?,
                week_start: match (per.unit, week_start) {
                    (_, Some(ws)) => ws,
                    (query_syntax::Kw::Week, None) => {
                        return Err(ExecError::Fatal(
                            "totals: ПЕРИОДАМИ(НЕДЕЛЯ) without the first day of a week of the base"
                                .into(),
                        ));
                    }
                    (_, None) => 1,
                },
            }),
            None => None,
        };
        points.push(PointSpec {
            name: p.name.clone(),
            column: p.column,
            hierarchy: p.hierarchy,
            periods,
            hier,
        });
    }
    let items = t
        .items
        .iter()
        .map(|(e, c)| Ok((expr(b, e)?, *c)))
        .collect::<Result<_, ExecError>>()?;
    Ok(TotalsSpec {
        width: t.width,
        points,
        aggs: t.aggs.clone(),
        items,
        order: t.order.clone(),
    })
}
