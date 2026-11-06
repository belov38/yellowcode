use super::deref::plan_expr;
use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::{PExpr, RelL};
use crate::bind::const_value;
use crate::ir::{ExprKind, RegisterVtS};
use crate::schema::{FieldRole, QFieldId, StdRole};
use crate::value::register_bounds::{Bnd, bound_of};

impl Lower<'_> {
    pub(super) fn records(&mut self, v: &RegisterVtS) -> R<Row> {
        self.added = self.added.saturating_add(6);
        let Some(table) = self.storage.table(v.table) else {
            return internal("accounting records without main storage");
        };
        let vt = self.schema.table(v.table);
        let mut needed = std::collections::HashSet::new();
        for src in [v.src, v.inner] {
            needed.extend(
                self.vt_used
                    .get(src.0 as usize)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
        if v.bounds.iter().any(Option::is_some) {
            for (i, f) in vt.fields.iter().enumerate() {
                if matches!(f.role, FieldRole::Std(StdRole::Period | StdRole::Recorder)) {
                    needed.insert(QFieldId(p16(i as i64)?));
                }
            }
        }
        let required: Vec<_> = (0..vt.fields.len())
            .filter_map(|i| u16::try_from(i).ok().map(QFieldId))
            .filter(|f| needed.contains(f))
            .collect();
        self.slice_cols.insert(v.src, required.clone());
        self.slice_cols.insert(v.inner, required.clone());
        let mut stored = Vec::new();
        let mut plans = Vec::with_capacity(required.len());
        for f in &required {
            let plan = self.plan(v.table, *f)?;
            let mut fields = Vec::new();
            plan.needs(&mut fields);
            for (input, field) in fields {
                if input != table {
                    return internal("accounting record field in another storage input");
                }
                if !stored.contains(&field) {
                    stored.push(field);
                }
            }
            plans.push(plan);
        }
        let mut projection = Vec::with_capacity(plans.len());
        for plan in &plans {
            projection.push(plan_expr(plan, &mut |(_, field)| {
                stored
                    .iter()
                    .position(|f| *f == field)
                    .ok_or_else(|| crate::ExecError::Fatal("record scan field missing".into()))
                    .and_then(|i| p16(i as i64).map(PExpr::Col))
            })?);
        }
        let mut row = Row {
            signal: None,
            rel: RelL::Project {
                input: Box::new(RelL::Scan {
                    table,
                    fields: stored,
                }),
                exprs: projection,
            },
            width: required.len(),
            rep: Some(v.inner),
            keys: None,
        };
        self.set_level(v.inner);
        self.inner += 1;
        let result = (|| {
            let mut bounds = [Bnd::None; 2];
            for (out, e) in bounds.iter_mut().zip(&v.bounds) {
                if let Some(e) = e {
                    if !matches!(e.kind, ExprKind::Lit(_) | ExprKind::Param(_)) {
                        return internal("deferred accounting bound reached lowering");
                    }
                    let value =
                        const_value(e, &self.b.params, &self.b.values)?.ok_or_else(|| {
                            crate::ExecError::Fatal("record boundary is not a bound scalar".into())
                        })?;
                    *out = bound_of(Some(&value));
                }
            }
            if bounds != [Bnd::None; 2] {
                let column = |role| {
                    required
                        .iter()
                        .position(|f| {
                            vt.fields
                                .get(usize::from(f.0))
                                .is_some_and(|f| f.role == FieldRole::Std(role))
                        })
                        .ok_or_else(|| {
                            crate::ExecError::Fatal("record boundary field missing".into())
                        })
                        .and_then(|i| p16(i as i64).map(PExpr::Col))
                };
                let pred = PExpr::RecordInInterval {
                    period: Box::new(column(StdRole::Period)?),
                    recorder: Box::new(column(StdRole::Recorder)?),
                    bounds,
                };
                row.rel = RelL::Filter {
                    input: row.take(),
                    pred,
                };
            }
            if let Some(condition) = &v.cond {
                row = self.filter_row(row, condition)?;
            }
            if let Some(order) = &v.order {
                let key = self.expr(&order.expr, &mut row)?;
                if self.membership
                    && let Some(signal) = row.signal
                {
                    row.rel = RelL::CheckAccess {
                        input: row.take(),
                        signal,
                    };
                    self.added = self.added.saturating_add(1);
                }
                row.rel = RelL::Sort {
                    input: row.take(),
                    keys: vec![(key, order.desc)],
                };
            }
            if let Some(n) = v.top {
                row.rel = RelL::Top {
                    input: row.take(),
                    n,
                };
            }
            let mut exprs = (0..required.len())
                .map(|i| p16(i as i64).map(PExpr::Col))
                .collect::<R<Vec<_>>>()?;
            let signal = row
                .signal
                .map(|_| {
                    let i = exprs.len();
                    exprs.push(row.signal_expr());
                    p16(i as i64)
                })
                .transpose()?;
            row.width = exprs.len();
            row.rel = RelL::Project {
                input: row.take(),
                exprs,
            };
            row.signal = signal;
            row.rep = Some(v.src);
            self.set_level(v.src);
            Ok(row)
        })();
        self.inner -= 1;
        result
    }
}
