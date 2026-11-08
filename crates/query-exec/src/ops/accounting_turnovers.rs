use super::{Cx, OwnedCharge, accounting::check_cancel, row_cap_bytes};
use crate::source::{Row, RowIter};
use bsl_prim::Number;
use ibschema::storage::FieldId;
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{AccountingTurnoversL, AccountingTurnoversResourceL};
use query_ir::{ExecError, QValue};

fn fatal(message: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting Turnovers: {message}"))
}

fn value(row: &Row, field: FieldId) -> Result<&QValue, ExecError> {
    row.get(usize::from(field.0))
        .ok_or_else(|| fatal("missing stored field"))
}

fn account(row: &Row, field: FieldId, target: u32) -> Result<&QValue, ExecError> {
    match value(row, field)? {
        QValue::Ref { table, .. } if *table == target => value(row, field),
        QValue::Null => Err(ExecError::Later {
            what: "Q6b-TurnoversNullAccount",
            stage: "Q6b",
        }),
        _ => Err(fatal("stored account reference target mismatch")),
    }
}

fn stored_number<'row>(
    row: &'row Row,
    resource: &AccountingTurnoversResourceL,
    side: usize,
) -> Result<Option<&'row Number>, ExecError> {
    let field = resource
        .main
        .get(side)
        .copied()
        .ok_or_else(|| fatal("resource side offset"))?;
    match value(row, field)? {
        QValue::Number(number) => Ok(Some(number)),
        QValue::Null if resource.null_zero.get(side) == Some(&true) => Ok(None),
        QValue::Null if resource.main[0] == resource.main[1] => Err(ExecError::Later {
            what: "Q6b-TurnoversNullResource",
            stage: "Q6b",
        }),
        QValue::Null => Err(fatal("nonnullable resource is NULL")),
        _ if resource.main[0] == resource.main[1] => Err(fatal("balanced resource is not Number")),
        _ => Err(fatal("nonbalanced resource is not Number")),
    }
}

pub(super) struct Cursor<'a> {
    fields: Vec<FieldId>,
    fields_charge: Option<OwnedCharge>,
    iter: Option<RowIter<'a>>,
    pending: Option<(Row, OwnedCharge)>,
    side: usize,
    done: bool,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(step: &AccountingTurnoversL, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let capacity = step
            .resources
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(3))
            .and_then(|count| count.checked_add(usize::from(step.period.is_some())))
            .and_then(|count| count.checked_add(step.policy_fields.len()))
            .ok_or_else(|| fatal("field count overflow"))?;
        let bytes = capacity
            .checked_mul(std::mem::size_of::<FieldId>())
            .ok_or_else(|| fatal("field allocation overflow"))?;
        let fields_charge = cx.mem.hold_owned(crate::model::block(
            u64::try_from(bytes).map_err(|_| fatal("field allocation overflow"))?,
        ))?;
        let mut fields = Vec::with_capacity(capacity);
        fields.push(step.inputs.active);
        fields.extend(step.period);
        fields.extend(step.inputs.accounts);
        for resource in &step.resources {
            for field in resource.main {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        fields.extend(&step.policy_fields);
        Ok(Self {
            fields,
            fields_charge: Some(fields_charge),
            iter: None,
            pending: None,
            side: 0,
            done: false,
        })
    }

    pub(super) fn next(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            if self.done {
                return Ok(None);
            }
            if self.pending.is_none() {
                if self.iter.is_none() {
                    self.iter = Some(cx.src.scan_fields(step.inputs.main, &self.fields)?);
                }
                let Some(iter) = self.iter.as_mut() else {
                    return Err(fatal("missing main cursor"));
                };
                let Some(row) = iter.next() else {
                    self.close();
                    return Ok(None);
                };
                let row = row?;
                check_cancel(cx.env)?;
                match value(&row, step.inputs.active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("Active is not Boolean")),
                }
                if let Some(field) = step.period {
                    let QValue::Date(period) = value(&row, field)? else {
                        return Err(fatal("Period is not Date"));
                    };
                    if step.dates[0].is_some_and(|start| *period < start)
                        || step.dates[1].is_some_and(|end| *period > end)
                    {
                        continue;
                    }
                }
                for field in step.inputs.accounts {
                    account(&row, field, step.inputs.ref_table)?;
                }
                for resource in &step.resources {
                    for side in 0..2 {
                        if side == 1 && resource.main[0] == resource.main[1] {
                            continue;
                        }
                        if let Some(number) = stored_number(&row, resource, side)? {
                            crate::num::guard(cx.env, number)?;
                        }
                    }
                }
                let charge = cx.mem.hold_owned(row_cap_bytes(&row))?;
                self.pending = Some((row, charge));
                self.side = 0;
            }
            if self.side >= 2 {
                self.pending = None;
                continue;
            }
            let side = self.side;
            self.side += 1;
            let Some((row, _)) = &self.pending else {
                return Err(fatal("missing held movement"));
            };
            let field = step
                .inputs
                .accounts
                .get(side)
                .copied()
                .ok_or_else(|| fatal("account side offset"))?;
            let key = account(row, field, step.inputs.ref_table)?;
            if matches!(key, QValue::Ref { id, .. } if *id == [0; 16]) {
                continue;
            }
            let nk = usize::from(step.account);
            let width = step
                .resources
                .len()
                .checked_mul(raw::PER_RESOURCE)
                .and_then(|n| n.checked_add(raw::DIMS + nk))
                .and_then(|n| n.checked_add(step.policy_fields.len()))
                .ok_or_else(|| fatal("raw width overflow"))?;
            let bytes = width
                .checked_mul(std::mem::size_of::<QValue>())
                .ok_or_else(|| fatal("raw allocation overflow"))?;
            let mut charge = crate::model::block(
                u64::try_from(bytes).map_err(|_| fatal("raw allocation overflow"))?,
            );
            for resource in &step.resources {
                if stored_number(row, resource, side)?.is_some() {
                    let field = resource
                        .main
                        .get(side)
                        .copied()
                        .ok_or_else(|| fatal("resource side offset"))?;
                    charge = charge
                        .checked_add(
                            crate::model::value_model(value(row, field)?)
                                .checked_mul(2)
                                .ok_or_else(|| fatal("numeric allocation overflow"))?,
                        )
                        .ok_or_else(|| fatal("raw model overflow"))?;
                }
            }
            for field in &step.policy_fields {
                charge =
                    crate::model::size_add(charge, crate::model::value_model(value(row, *field)?))?;
            }
            cx.mem.grow_scratch(charge)?;
            let mut out = vec![QValue::Number(Number::zero()); width];
            for slot in out.iter_mut().take(raw::DIMS) {
                *slot = QValue::Null;
            }
            if step.account {
                *out.get_mut(raw::DIMS)
                    .ok_or_else(|| fatal("raw account offset"))? = key.clone();
            }
            for (r, resource) in step.resources.iter().enumerate() {
                let Some(number) = stored_number(row, resource, side)? else {
                    continue;
                };
                let net = if side == 0 {
                    number.clone()
                } else {
                    number.neg()
                };
                *out.get_mut(raw::component(nk, r, raw::TURNOVER))
                    .ok_or_else(|| fatal("net offset"))? = QValue::Number(net);
                let component = if side == 0 {
                    raw::RECEIPT
                } else {
                    raw::EXPENSE
                };
                *out.get_mut(raw::component(nk, r, component))
                    .ok_or_else(|| fatal("side offset"))? = QValue::Number(number.clone());
            }
            let base = raw::width(nk, step.resources.len());
            for (i, field) in step.policy_fields.iter().enumerate() {
                *out.get_mut(base + i)
                    .ok_or_else(|| fatal("policy output offset"))? = value(row, *field)?.clone();
            }
            return Ok(Some(out));
        }
    }

    pub(super) fn close(&mut self) {
        #[cfg(test)]
        crate::ordinary_calibration_trace::mark(
            "ordinary-cursor-before-close",
            0,
            [
                crate::ordinary_calibration_trace::Backing::of(&self.fields),
                self.pending
                    .as_ref()
                    .map(|(row, _)| crate::ordinary_calibration_trace::Backing::of(row))
                    .unwrap_or_default(),
                Default::default(),
            ],
        );
        self.iter = None;
        self.pending = None;
        self.fields = Vec::new();
        drop(self.fields_charge.take());
        self.done = true;
        #[cfg(test)]
        crate::ordinary_calibration_trace::mark(
            "ordinary-cursor-closed",
            0,
            [Default::default(); 3],
        );
    }
}
