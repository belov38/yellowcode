use super::super::{BoxOp, Cx, OwnedCharge};
use super::owned::{PaidRow, PaidVec};
use super::{fatal, value};
use crate::source::{Row, RowIter};
use bsl_prim::{
    Number,
    number::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput},
};
use ibschema::storage::FieldId;
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{BalanceRowsL, BalanceValueCheckL};
use query_ir::{DateQ, ExecError, QValue};

#[derive(Clone, Copy)]
enum Slot {
    Absent,
    Balance,
    Turnover,
}
#[derive(Clone, Copy)]
struct Account {
    id: [u8; 16],
    kind: u8,
    signal: Option<bool>,
    slot: Slot,
}

pub(super) struct FirstCursor<'a> {
    fields: Option<PaidVec<FieldId>>,
    accounts: Option<PaidVec<Account>>,
    ready: bool,
    iter: Option<RowIter<'a>>,
    row: Option<PaidRow>,
    eligible: [Option<usize>; 2],
    side: usize,
}

fn cancel(cx: &Cx<'_>) -> Result<(), ExecError> {
    super::super::accounting::check_cancel(cx.env)
}
fn ext1(step: &BalanceRowsL) -> Result<&query_ir::lower::BalanceExt1L, ExecError> {
    step.ext1
        .as_deref()
        .ok_or_else(|| fatal("missing I1 payload"))
}

impl<'a> FirstCursor<'a> {
    pub(super) fn new(step: &BalanceRowsL, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let ext = ext1(step)?;
        assert!(
            cx.env
                .memory
                .zip(cx.env.lease)
                .is_some_and(|(memory, lease)| {
                    lease.category() == memory_budget::Category::Execution
                        && memory.budget.same_scope(lease.budget())
                        && memory
                            .budget
                            .same_scope(ext.value_check.reference_tags.reservation().budget())
                        && memory
                            .budget
                            .same_scope(ext.shared_checks.reservation().budget())
                        && ext.shared_checks.iter().all(|check| {
                            memory
                                .budget
                                .same_scope(check.value.reference_tags.reservation().budget())
                        })
                }),
            "managed accounting request/lease scope invariant"
        );
        let mut fields = PaidVec::new(0, &cx.mem)?;
        for field in [step.inputs.active]
            .into_iter()
            .chain(step.period)
            .chain(step.inputs.accounts)
            .chain(ext.values)
            .chain(step.resources.iter().map(|r| r.main))
            .chain(step.policy_fields.iter().copied())
        {
            if !fields.contains(&field) {
                fields.push(field, &cx.mem)?;
            }
        }
        Ok(Self {
            fields: Some(fields),
            accounts: Some(PaidVec::new(0, &cx.mem)?),
            ready: false,
            iter: None,
            row: None,
            eligible: [None, None],
            side: 0,
        })
    }

    fn load(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        cancel(cx)?;
        let mut scan = if step.chart_layout.is_none() {
            Some(cx.src.scan_fields(
                step.inputs.chart,
                &[step.inputs.chart_id, step.inputs.chart_kind],
            )?)
        } else {
            None
        };
        loop {
            cancel(cx)?;
            cx.tick()?;
            let next = if let Some(child) = child.as_mut() {
                child.next(cx)?
            } else if let Some(scan) = scan.as_mut() {
                scan.next().transpose()?
            } else {
                return Err(fatal("missing secured chart child"));
            };
            let Some(row) = next else {
                break;
            };
            let paid = PaidRow::import(row, cx)?;
            let QValue::Ref { table, id } = value(
                &paid.row,
                step.chart_layout
                    .map_or(step.inputs.chart_id, |l| FieldId(l.id)),
            )?
            else {
                return Err(fatal("chart ID is not a reference"));
            };
            let accounts = self
                .accounts
                .as_mut()
                .ok_or_else(|| fatal("closed I1 cursor"))?;
            if *table != step.inputs.ref_table
                || *id == [0; 16]
                || accounts.iter().any(|entry| entry.id == *id)
            {
                return Err(fatal("invalid or duplicate chart identity"));
            }
            let QValue::Number(kind) = value(
                &paid.row,
                step.chart_layout
                    .map_or(step.inputs.chart_kind, |l| FieldId(l.kind)),
            )?
            else {
                return Err(fatal("chart Kind is not numeric"));
            };
            let kind = match kind.to_i64() {
                Some(0) => 0,
                Some(1) => 1,
                Some(2) => 2,
                _ => return Err(fatal("unknown chart Kind")),
            };
            let signal = match step.chart_layout.and_then(|l| l.signal) {
                Some(field) => match value(&paid.row, FieldId(field))? {
                    QValue::Bool(value) => Some(*value),
                    _ => return Err(fatal("chart signal is not Boolean")),
                },
                None => None,
            };
            accounts.push(
                Account {
                    id: *id,
                    kind,
                    signal,
                    slot: Slot::Absent,
                },
                &cx.mem,
            )?;
        }
        drop(scan);
        if let Some(mut child) = child.take() {
            child.close(cx);
        }
        let accounts = self
            .accounts
            .as_mut()
            .ok_or_else(|| fatal("closed I1 cursor"))?;
        accounts.sort_unstable_by_key(|entry| entry.id);
        let ext = ext1(step)?;
        let mut section = cx
            .src
            .scan_fields(ext.section, &[ext.owner, ext.ordinal, ext.turnover_only])?;
        loop {
            cancel(cx)?;
            cx.tick()?;
            let Some(row) = section.next().transpose()? else {
                break;
            };
            let paid = PaidRow::import(row, cx)?;
            let QValue::Number(ordinal) = value(&paid.row, ext.ordinal)? else {
                return Err(fatal("section ordinal is not numeric"));
            };
            let ordinal = ordinal
                .to_i64()
                .filter(|n| (1..=99_999).contains(n))
                .ok_or_else(|| fatal("invalid section ordinal"))?;
            if ordinal != 1 {
                continue;
            }
            let QValue::Ref { table, id } = value(&paid.row, ext.owner)? else {
                return Err(fatal("section owner is not a reference"));
            };
            if *table != step.inputs.ref_table {
                return Err(fatal("section owner target mismatch"));
            }
            let Ok(index) = accounts.binary_search_by_key(id, |entry| entry.id) else {
                continue;
            };
            let entry = accounts
                .get_mut(index)
                .ok_or_else(|| fatal("account index"))?;
            if !matches!(entry.slot, Slot::Absent) {
                return Err(fatal("duplicate first subconto ordinal"));
            }
            entry.slot = match value(&paid.row, ext.turnover_only)? {
                QValue::Bool(false) => Slot::Balance,
                QValue::Bool(true) => Slot::Turnover,
                _ => return Err(fatal("section TurnoverOnly is not Boolean")),
            };
        }
        drop(section);
        self.ready = true;
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        if !self.ready {
            self.load(step, child, cx)?;
        }
        loop {
            cancel(cx)?;
            cx.tick()?;
            if self.row.is_none() {
                if self.iter.is_none() {
                    self.iter = Some(
                        cx.src.scan_fields(
                            step.inputs.main,
                            self.fields
                                .as_ref()
                                .ok_or_else(|| fatal("closed I1 cursor"))?,
                        )?,
                    );
                }
                let Some(row) = self
                    .iter
                    .as_mut()
                    .ok_or_else(|| fatal("missing main iterator"))?
                    .next()
                    .transpose()?
                else {
                    return Ok(None);
                };
                let paid = PaidRow::import(row, cx)?;
                match value(&paid.row, step.inputs.active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("main Active is not Boolean")),
                }
                if let Some(field) = step.period {
                    let QValue::Date(period) = value(&paid.row, field)? else {
                        return Err(fatal("main Period is not Date"));
                    };
                    if step.date.is_some_and(|date| *period >= date) {
                        continue;
                    }
                }
                let accounts = self
                    .accounts
                    .as_ref()
                    .ok_or_else(|| fatal("closed I1 cursor"))?;
                let mut eligible = [None, None];
                for (slot, field) in eligible.iter_mut().zip(&step.inputs.accounts) {
                    *slot = match value(&paid.row, *field)? {
                        QValue::Null => None,
                        QValue::Ref { table, id } if *table == step.inputs.ref_table => {
                            if *id == [0; 16] {
                                None
                            } else {
                                accounts.binary_search_by_key(id, |a| a.id).ok()
                            }
                        }
                        _ => return Err(fatal("account reference target mismatch")),
                    };
                }
                if eligible == [None, None] {
                    continue;
                }
                let ext = ext1(step)?;
                for (selected, field) in eligible.iter().zip(&ext.values) {
                    if selected.is_some() {
                        validate(value(&paid.row, *field)?, &ext.value_check, cx)?;
                    }
                }
                for check in ext.shared_checks.iter() {
                    validate(value(&paid.row, check.field)?, &check.value, cx)?;
                }
                self.eligible = eligible;
                self.row = Some(paid);
                self.side = 0;
            }
            if self.side == 2 {
                self.row = None;
                continue;
            }
            let side = self.side;
            self.side += 1;
            let Some(index) = *self
                .eligible
                .get(side)
                .ok_or_else(|| fatal("I1 side offset"))?
            else {
                continue;
            };
            let entry = *self
                .accounts
                .as_ref()
                .ok_or_else(|| fatal("closed I1 cursor"))?
                .get(index)
                .ok_or_else(|| fatal("account index"))?;
            let row = &self
                .row
                .as_ref()
                .ok_or_else(|| fatal("missing validated row"))?
                .row;
            let ext = ext1(step)?;
            let base = raw::width(3, step.resources.len());
            let signal = step
                .chart_layout
                .is_some_and(|layout| layout.signal.is_some());
            let width = base + usize::from(signal) + step.policy_fields.len();
            let mut owners = PaidVec::new(0, &cx.mem)?;
            let mut out = PaidVec::new(width, &cx.mem)?;
            for _ in 0..width {
                out.push(QValue::Null, &cx.mem)?;
            }
            let account = *step
                .inputs
                .accounts
                .get(side)
                .ok_or_else(|| fatal("I1 side offset"))?;
            *slot(&mut out, raw::DIMS)? = value(row, account)?.clone();
            *slot(&mut out, raw::DIMS + 1)? = QValue::Number(Number::from(u32::from(entry.kind)));
            if matches!(entry.slot, Slot::Balance) {
                let field = *ext
                    .values
                    .get(side)
                    .ok_or_else(|| fatal("I1 side offset"))?;
                *slot(&mut out, raw::DIMS + 2)? = copy(value(row, field)?, false, &mut owners, cx)?;
            }
            for (i, resource) in step.resources.iter().enumerate() {
                *slot(&mut out, raw::component(3, i, raw::BALANCE))? =
                    copy(value(row, resource.main)?, side == 1, &mut owners, cx)?;
            }
            let mut offset = base;
            if signal {
                *slot(&mut out, offset)? =
                    QValue::Bool(entry.signal.ok_or_else(|| fatal("missing chart signal"))?);
                offset += 1;
            }
            for field in &step.policy_fields {
                *slot(&mut out, offset)? = copy(value(row, *field)?, false, &mut owners, cx)?;
                offset += 1;
            }
            let result = super::owned::output(out, cx)?;
            drop(owners);
            return Ok(Some(result));
        }
    }

    pub(super) fn close(&mut self) {
        self.iter = None;
        self.row = None;
        self.fields = None;
        self.accounts = None;
        self.eligible = [None, None];
        self.side = 2;
    }
}

pub(super) fn validate(
    value: &QValue,
    check: &BalanceValueCheckL,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    let valid = match value {
        QValue::Null => check.nullable,
        QValue::Undefined => check.undefined,
        QValue::Bool(_) => check.boolean,
        QValue::Number(number) => {
            crate::num::guard(cx.env, number)?;
            check
                .number
                .is_some_and(|q| super::ext1_numeric_fits(number, q))
        }
        QValue::String(text) => check
            .string
            .is_some_and(|q| q.len == 0 || text.len_utf16() <= q.len as usize),
        QValue::Date(date) => check.date.is_some_and(|q| {
            if date.frac_ticks() != 0 {
                return false;
            }
            let p = date.parts();
            match q {
                DateQ::DateTime => true,
                DateQ::Date => p.hour == 0 && p.minute == 0 && p.second == 0,
                DateQ::Time => p.year == 1 && p.month == 1 && p.day == 1,
            }
        }),
        QValue::Uuid(_) => check.uuid,
        QValue::Ref { table, .. } => check.reference_tags.binary_search(table).is_ok(),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(fatal(
            "stored value contradicts semantic accounting qualifier",
        ))
    }
}

struct Limbs(PaidVec<u32>);
impl AsRef<[u32]> for Limbs {
    fn as_ref(&self) -> &[u32] {
        &self.0
    }
}
impl AsMut<[u32]> for Limbs {
    fn as_mut(&mut self) -> &mut [u32] {
        &mut self.0
    }
}
impl LimbBuffer for Limbs {
    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}
struct Alloc<'m, 'a>(&'m super::super::MemBudget<'a>);
impl LimbAllocator for Alloc<'_, '_> {
    type Error = ExecError;
    type Buffer = Limbs;
    fn allocate_zeroed(&self, len: usize) -> Result<Limbs, ExecError> {
        let mut limbs = PaidVec::new(len, self.0)?;
        for _ in 0..len {
            limbs.push(0, self.0)?;
        }
        Ok(Limbs(limbs))
    }
}
fn slot(out: &mut [QValue], index: usize) -> Result<&mut QValue, ExecError> {
    out.get_mut(index).ok_or_else(|| fatal("raw output offset"))
}

fn copy(
    value: &QValue,
    negative: bool,
    owners: &mut PaidVec<OwnedCharge>,
    cx: &Cx<'_>,
) -> Result<QValue, ExecError> {
    if let QValue::Number(number) = value {
        owners.reserve_one(&cx.mem)?;
        let output = if negative {
            number.try_neg_with(&Alloc(&cx.mem))
        } else {
            number.try_add_with(&Number::zero(), &Alloc(&cx.mem))
        }
        .map_err(|error| match error {
            NumberOpError::Buffer(error) => error,
            _ => crate::model::invalid_size(),
        })?;
        return match output {
            NumberOutput::Inline(number) => Ok(QValue::Number(number)),
            NumberOutput::Buffered { parts, limbs } => {
                let (limbs, owner) = limbs.0.into_parts();
                let number = parts.into_number(limbs);
                owners.push(owner, &cx.mem)?;
                Ok(QValue::Number(number))
            }
        };
    }
    Ok(value.clone())
}
