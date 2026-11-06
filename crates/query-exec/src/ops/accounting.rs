use super::{Cx, row_cap_bytes};
use crate::register::RegisterSettings;
use crate::source::{ExecEnv, Row, RowIter};
use bsl_prim::{BslDate, Number};
use ibschema::storage::FieldId;
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{AccountingRowsL, DrCrSideL};
use query_ir::{ExecError, QValue};
use std::sync::atomic::Ordering;

fn fatal(message: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting DrCrTurnovers: {message}"))
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DrCrSelection {
    Main,
    CtAndMain { point: BslDate },
}

pub(crate) fn check_cancel(env: &ExecEnv<'_>) -> Result<(), ExecError> {
    if env.cancel.load(Ordering::Relaxed) || env.stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
        return Err(ExecError::Fatal("выполнение прервано".into()));
    }
    Ok(())
}

pub(crate) fn check_drcr_settings_shape(
    s: &RegisterSettings,
    has_dates: bool,
) -> Result<(), ExecError> {
    let sentinel = BslDate::from_parts(3999, 11, 1, 0, 0, 0)
        .ok_or_else(|| fatal("invalid totals sentinel"))?;
    let refused = || ExecError::Later {
        what: "Q6b-DrCrTurnoversSettingsShape",
        stage: "Q6b",
    };
    if s.min_period != sentinel || s.min_calculated.is_some() || has_dates && s.calculated.is_some()
    {
        return Err(refused());
    }
    if let Some(point) = s.calculated {
        let p = point.parts();
        if point >= sentinel || BslDate::from_parts(p.year, p.month, 1, 0, 0, 0) != Some(point) {
            return Err(refused());
        }
    }
    Ok(())
}

pub(super) fn selection(s: &RegisterSettings, has_dates: bool) -> Result<DrCrSelection, ExecError> {
    check_drcr_settings_shape(s, has_dates)?;
    Ok(match s.calculated {
        None => DrCrSelection::Main,
        Some(point) => DrCrSelection::CtAndMain { point },
    })
}

pub(super) struct Cursor<'a> {
    phase: u8,
    iter: Option<RowIter<'a>>,
    main_fields: Vec<FieldId>,
    ct_fields: Vec<FieldId>,
    charge: u64,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(step: &AccountingRowsL, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let main = step
            .keys
            .len()
            .checked_add(step.resources.len())
            .and_then(|n| n.checked_add(2))
            .and_then(|n| n.checked_add(usize::from(step.area.is_some())))
            .ok_or_else(|| fatal("main field count overflow"))?;
        let ct = main
            .checked_sub(1)
            .ok_or_else(|| fatal("CT field count overflow"))?;
        let bytes = main
            .checked_add(ct)
            .and_then(|n| n.checked_mul(std::mem::size_of::<FieldId>()))
            .ok_or_else(|| fatal("field allocation overflow"))?;
        let charge = u64::try_from(bytes).map_err(|_| fatal("field allocation overflow"))?;
        cx.mem.grow(charge)?;
        let mut main_fields = Vec::with_capacity(main);
        let mut ct_fields = Vec::with_capacity(ct);
        main_fields.extend([step.inputs.main_period, step.inputs.main_active]);
        ct_fields.push(step.inputs.ct_period);
        for side in &step.keys {
            let account = match side {
                DrCrSideL::Dr => &step.inputs.accounts[0],
                DrCrSideL::Cr => &step.inputs.accounts[1],
            };
            if !main_fields.contains(&account.main) {
                main_fields.push(account.main);
            }
            if !ct_fields.contains(&account.ct) {
                ct_fields.push(account.ct);
            }
        }
        for resource in &step.resources {
            if !main_fields.contains(&resource.main) {
                main_fields.push(resource.main);
            }
            if !ct_fields.contains(&resource.ct) {
                ct_fields.push(resource.ct);
            }
        }
        if let Some(area) = step.area {
            if !main_fields.contains(&area.main) {
                main_fields.push(area.main);
            }
            if !ct_fields.contains(&area.ct) {
                ct_fields.push(area.ct);
            }
        }
        Ok(Self {
            phase: 0,
            iter: None,
            main_fields,
            ct_fields,
            charge,
        })
    }

    pub(super) fn next(
        &mut self,
        step: &AccountingRowsL,
        mode: DrCrSelection,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        loop {
            let mixed = matches!(mode, DrCrSelection::CtAndMain { .. });
            let is_ct = mixed && self.phase == 0;
            if self.phase >= if mixed { 2 } else { 1 } {
                return Ok(None);
            }
            if self.iter.is_none() {
                cx.tick()?;
                self.iter = Some(if is_ct {
                    cx.src.scan_fields(step.inputs.ct, &self.ct_fields)?
                } else {
                    cx.src.scan_fields(step.inputs.main, &self.main_fields)?
                });
            }
            cx.tick()?;
            let Some(iter) = self.iter.as_mut() else {
                return Err(fatal("missing contribution iterator"));
            };
            let Some(row) = iter.next() else {
                self.iter = None;
                self.phase += 1;
                continue;
            };
            let row = row?;
            if let Some(area) = step.area {
                let field = if is_ct { area.ct } else { area.main };
                match value(&row, field)? {
                    QValue::Number(value) if value.to_i64() == Some(area.value) => {}
                    QValue::Number(_) => continue,
                    _ => return Err(fatal("area separator is not Number")),
                }
            }
            if !is_ct {
                match value(&row, step.inputs.main_active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("main Active is not Boolean")),
                }
            }
            let period_field = if is_ct {
                step.inputs.ct_period
            } else {
                step.inputs.main_period
            };
            let QValue::Date(period) = value(&row, period_field)? else {
                return Err(fatal("contribution Period is not Date"));
            };
            if !is_ct
                && (step.date_bounds[0]
                    .is_some_and(|start| start != BslDate::EMPTY && *period < start)
                    || step.date_bounds[1]
                        .is_some_and(|end| end != BslDate::EMPTY && *period > end))
            {
                continue;
            }
            if let DrCrSelection::CtAndMain { point } = mode
                && ((is_ct && *period >= point) || (!is_ct && *period < point))
            {
                continue;
            }
            if is_ct {
                let mut nonzero = false;
                for resource in &step.resources {
                    match value(&row, resource.ct)? {
                        QValue::Null => {}
                        QValue::Number(n) => nonzero |= !n.is_zero(),
                        _ => return Err(fatal("CT resource is not Number or Null")),
                    }
                }
                if !nonzero {
                    continue;
                }
            }
            let width = step
                .resources
                .len()
                .checked_mul(raw::PER_RESOURCE)
                .and_then(|n| n.checked_add(step.keys.len()))
                .and_then(|n| n.checked_add(raw::DIMS))
                .ok_or_else(|| fatal("raw row width overflow"))?;
            let bytes = width
                .checked_mul(std::mem::size_of::<QValue>())
                .ok_or_else(|| fatal("raw row allocation overflow"))?;
            let mut charge = crate::model::block(
                u64::try_from(bytes).map_err(|_| fatal("raw row allocation overflow"))?,
            );
            charge = charge
                .checked_add(row_cap_bytes(&row))
                .ok_or_else(|| fatal("source row model overflow"))?;
            for resource in &step.resources {
                let field = if is_ct { resource.ct } else { resource.main };
                charge = charge
                    .checked_add(
                        crate::model::value_model(value(&row, field)?)
                            .checked_mul(2)
                            .ok_or_else(|| fatal("numeric model overflow"))?,
                    )
                    .ok_or_else(|| fatal("numeric model overflow"))?;
            }
            cx.mem.grow_scratch(charge)?;
            let mut out = vec![QValue::Null; width];
            if let Some(slot) = out.get_mut(raw::PERIOD) {
                *slot = QValue::Date(*period);
            }
            for (index, side) in step.keys.iter().enumerate() {
                let account = match side {
                    DrCrSideL::Dr => &step.inputs.accounts[0],
                    DrCrSideL::Cr => &step.inputs.accounts[1],
                };
                let field = if is_ct { account.ct } else { account.main };
                let key = value(&row, field)?;
                if !matches!(key, QValue::Null)
                    && !matches!(key,QValue::Ref {table,..} if *table==account.ref_table)
                {
                    return Err(fatal("account reference target mismatch"));
                }
                *out.get_mut(raw::DIMS + index)
                    .ok_or_else(|| fatal("account raw offset"))? = key.clone();
            }
            for (index, resource) in step.resources.iter().enumerate() {
                let field = if is_ct { resource.ct } else { resource.main };
                let resource_value = match value(&row, field)? {
                    QValue::Null if mixed => QValue::Number(Number::zero()),
                    QValue::Null => QValue::Null,
                    QValue::Number(n) if mixed && !is_ct => {
                        QValue::Number(crate::num::round_to(n, resource.ct_dec, cx.env)?)
                    }
                    QValue::Number(n) => {
                        crate::num::guard(cx.env, n)?;
                        QValue::Number(n.clone())
                    }
                    _ => return Err(fatal("resource is not Number or Null")),
                };
                *out.get_mut(raw::component(step.keys.len(), index, raw::TURNOVER))
                    .ok_or_else(|| fatal("resource raw offset"))? = resource_value;
            }
            return Ok(Some(out));
        }
    }

    pub(super) fn close(&mut self, cx: &Cx<'_>) {
        self.iter = None;
        self.phase = 2;
        self.main_fields = Vec::new();
        self.ct_fields = Vec::new();
        cx.mem.shrink(std::mem::take(&mut self.charge));
    }
}

fn value(row: &Row, field: FieldId) -> Result<&QValue, ExecError> {
    row.get(usize::from(field.0))
        .ok_or_else(|| fatal(format_args!("missing row field {field:?}")))
}
