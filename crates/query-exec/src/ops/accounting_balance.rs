use super::{BoxOp, Cx, OwnedCharge, accounting::check_cancel};
use crate::register::RegisterSettings;
use crate::source::{Row, RowIter};
use bsl_prim::{BslDate, Number};
use ibschema::storage::FieldId;
use query_ir::ir::raw_layout as raw;
use query_ir::lower::BalanceRowsL;
use query_ir::{ExecError, QValue};
use std::collections::BTreeMap;

fn fatal(what: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting Balance: {what}"))
}

pub(crate) fn check_balance_settings_shape(s: &RegisterSettings) -> Result<(), ExecError> {
    let sentinel = BslDate::from_parts(3999, 11, 1, 0, 0, 0)
        .ok_or_else(|| fatal("invalid totals sentinel"))?;
    if s.current
        || s.calculated.is_some()
        || s.min_period != sentinel
        || s.min_calculated.is_some()
        || s.use_splitter
    {
        return Err(ExecError::Later {
            what: "Q6a-AccountingBalanceSettingsShape",
            stage: "Q6a",
        });
    }
    Ok(())
}

pub(super) struct LegacyCursor<'a> {
    accounts: BTreeMap<[u8; 16], (u8, Option<bool>)>,
    ready: bool,
    fields: Vec<FieldId>,
    iter: Option<RowIter<'a>>,
    row: Option<(Row, OwnedCharge)>,
    side: usize,
    fields_charge: Option<OwnedCharge>,
    accounts_charge: Option<OwnedCharge>,
}

impl<'a> LegacyCursor<'a> {
    pub(super) fn new(step: &BalanceRowsL, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let count = step
            .resources
            .len()
            .checked_add(3)
            .and_then(|count| count.checked_add(usize::from(step.period.is_some())))
            .and_then(|count| count.checked_add(step.policy_fields.len()))
            .ok_or_else(|| fatal("field count overflow"))?;
        let charge = crate::model::vec_block(count, std::mem::size_of::<FieldId>());
        let fields_charge = cx.mem.hold_owned(charge)?;
        let mut fields = Vec::with_capacity(count);
        fields.push(step.inputs.active);
        fields.extend(step.period);
        fields.extend(step.inputs.accounts);
        fields.extend(step.resources.iter().map(|r| r.main));
        fields.extend(&step.policy_fields);
        let accounts_charge = cx.mem.hold_owned(0)?;
        Ok(Self {
            accounts: BTreeMap::new(),
            ready: false,
            fields,
            iter: None,
            row: None,
            side: 0,
            fields_charge: Some(fields_charge),
            accounts_charge: Some(accounts_charge),
        })
    }

    fn load_accounts(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        check_cancel(cx.env)?;
        let inputs = &step.inputs;
        let mut rows = if step.chart_layout.is_none() {
            Some(
                cx.src
                    .scan_fields(inputs.chart, &[inputs.chart_id, inputs.chart_kind])?,
            )
        } else {
            None
        };
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let row = if let Some(child) = child.as_mut() {
                child.next(cx)?
            } else if let Some(rows) = rows.as_mut() {
                rows.next().transpose()?
            } else {
                return Err(fatal("missing secured chart input"));
            };
            let Some(row) = row else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            let QValue::Ref { table, id } = value(
                &row,
                step.chart_layout.map_or(inputs.chart_id, |l| FieldId(l.id)),
            )?
            else {
                return Err(fatal("chart ID is not a reference"));
            };
            if *table != inputs.ref_table || *id == [0; 16] || self.accounts.contains_key(id) {
                return Err(fatal("invalid or duplicate chart identity"));
            }
            let QValue::Number(kind) = value(
                &row,
                step.chart_layout
                    .map_or(inputs.chart_kind, |l| FieldId(l.kind)),
            )?
            else {
                return Err(fatal("chart Kind is not numeric"));
            };
            let kind = if *kind == Number::from(0) {
                0
            } else if *kind == Number::from(1) {
                1
            } else if *kind == Number::from(2) {
                2
            } else {
                return Err(fatal("unknown chart Kind"));
            };
            let signal = match step.chart_layout.and_then(|l| l.signal) {
                Some(i) => match value(&row, FieldId(i))? {
                    QValue::Bool(b) => Some(*b),
                    _ => return Err(fatal("chart signal is not Boolean")),
                },
                None => None,
            };
            let entry = std::mem::size_of::<([u8; 16], (u8, Option<bool>))>();
            let bytes = crate::model::size_vec(24, entry + 3 * std::mem::size_of::<usize>())?;
            let owner = self
                .accounts_charge
                .as_mut()
                .ok_or_else(|| fatal("missing account map owner"))?;
            cx.mem.grow_owned(owner, bytes)?;
            self.accounts.insert(*id, (kind, signal));
        }
        if let Some(mut input) = child.take() {
            input.close(cx);
        }
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
            self.load_accounts(step, child, cx)?;
        }
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            if self.row.is_none() {
                if self.iter.is_none() {
                    self.iter = Some(cx.src.scan_fields(step.inputs.main, &self.fields)?);
                }
                let Some(iter) = self.iter.as_mut() else {
                    return Err(fatal("missing main iterator"));
                };
                let Some(row) = iter.next() else {
                    return Ok(None);
                };
                let row = row?;
                check_cancel(cx.env)?;
                let charge = crate::model::row_cap_model(&row);
                cx.mem.grow_scratch(charge)?;
                match value(&row, step.inputs.active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("main Active is not Boolean")),
                }
                if let Some(field) = step.period {
                    let QValue::Date(period) = value(&row, field)? else {
                        return Err(fatal("main Period is not Date"));
                    };
                    if step.date.is_some_and(|date| *period >= date) {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                let owner = cx.mem.hold_owned(charge)?;
                cx.mem.release_scratch();
                self.row = Some((row, owner));
                self.side = 0;
            }
            if self.side == 2 {
                self.row = None;
                continue;
            }
            let side = self.side;
            self.side += 1;
            let (row, _) = self.row.as_ref().ok_or_else(|| fatal("missing main row"))?;
            let field = step
                .inputs
                .accounts
                .get(side)
                .copied()
                .ok_or_else(|| fatal("account side offset"))?;
            let account = value(row, field)?;
            let (kind, signal) = match account {
                QValue::Null => continue,
                QValue::Ref { table, id } if *table == step.inputs.ref_table => {
                    match self.accounts.get(id) {
                        Some((kind, signal)) => (*kind, *signal),
                        None => continue,
                    }
                }
                _ => return Err(fatal("account reference target mismatch")),
            };
            let base = raw::width(2, step.resources.len());
            let width = base
                + usize::from(step.chart_layout.is_some_and(|l| l.signal.is_some()))
                + step.policy_fields.len();
            let mut bytes = crate::model::vec_block(width, std::mem::size_of::<QValue>());
            for resource in &step.resources {
                let stored = value(row, resource.main)?;
                let QValue::Number(n) = stored else {
                    return Err(fatal("nonnullable resource is not numeric"));
                };
                crate::num::guard(cx.env, n)?;
                bytes = bytes
                    .checked_add(
                        crate::model::value_model(stored)
                            .checked_mul(2)
                            .ok_or_else(|| fatal("numeric model overflow"))?,
                    )
                    .ok_or_else(|| fatal("raw row model overflow"))?;
            }
            for field in &step.policy_fields {
                bytes =
                    crate::model::size_add(bytes, crate::model::value_model(value(row, *field)?))?;
            }
            cx.mem.grow_scratch(bytes)?;
            let mut out = vec![QValue::Null; width];
            *out.get_mut(raw::DIMS)
                .ok_or_else(|| fatal("account output offset"))? = account.clone();
            *out.get_mut(raw::DIMS + 1)
                .ok_or_else(|| fatal("kind output offset"))? =
                QValue::Number(Number::from(u32::from(kind)));
            for (index, resource) in step.resources.iter().enumerate() {
                let QValue::Number(n) = value(row, resource.main)? else {
                    return Err(fatal("non-numeric resource"));
                };
                *out.get_mut(raw::component(2, index, raw::BALANCE))
                    .ok_or_else(|| fatal("resource output offset"))? =
                    QValue::Number(if side == 0 { n.clone() } else { n.neg() });
            }
            let mut offset = base;
            if step.chart_layout.is_some_and(|l| l.signal.is_some()) {
                *out.get_mut(offset)
                    .ok_or_else(|| fatal("raw chart signal offset"))? =
                    QValue::Bool(signal.ok_or_else(|| fatal("missing chart signal"))?);
                offset += 1;
            }
            for field in &step.policy_fields {
                *out.get_mut(offset)
                    .ok_or_else(|| fatal("raw policy offset"))? = value(row, *field)?.clone();
                offset += 1;
            }
            return Ok(Some(out));
        }
    }

    pub(super) fn close(&mut self, _cx: &Cx<'_>) {
        self.iter = None;
        self.row = None;
        self.fields = Vec::new();
        self.accounts = BTreeMap::new();
        self.fields_charge = None;
        self.accounts_charge = None;
    }
}

fn value(row: &Row, field: FieldId) -> Result<&QValue, ExecError> {
    row.get(usize::from(field.0))
        .ok_or_else(|| fatal("projected row is too short"))
}

mod first;
mod owned;

enum Kind<'a> {
    Legacy(LegacyCursor<'a>),
    First(first::FirstCursor<'a>),
}
pub(super) struct Cursor<'a> {
    kind: Kind<'a>,
}
impl<'a> Cursor<'a> {
    pub(super) fn new(step: &BalanceRowsL, cx: &Cx<'_>) -> Result<Self, ExecError> {
        let kind = match &step.ext1 {
            Some(_) => first::FirstCursor::new(step, cx).map(Kind::First),
            None => LegacyCursor::new(step, cx).map(Kind::Legacy),
        }?;
        Ok(Self { kind })
    }
    pub(super) fn next(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        match &mut self.kind {
            Kind::Legacy(cursor) => cursor.next(step, child, cx),
            Kind::First(cursor) => cursor.next(step, child, cx),
        }
    }
    pub(super) fn close(&mut self, cx: &Cx<'_>) {
        match &mut self.kind {
            Kind::Legacy(cursor) => cursor.close(cx),
            Kind::First(cursor) => cursor.close(),
        }
    }
}

fn ext1_numeric_fits(number: &Number, qualifier: query_ir::NumQ) -> bool {
    if qualifier.nonneg && number.is_negative() {
        return false;
    }
    if qualifier.digits == 0 {
        return qualifier.scale == 0;
    }
    if qualifier.digits > 38 || qualifier.scale > qualifier.digits {
        return false;
    }
    struct Digits {
        integer_limit: u8,
        fraction_limit: u8,
        integer: u8,
        fraction: u8,
        after_dot: bool,
        leading_zero: bool,
    }
    impl std::fmt::Write for Digits {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for byte in text.bytes() {
                match byte {
                    b'-' => {}
                    b'.' => self.after_dot = true,
                    b'0'..=b'9' if self.after_dot => {
                        if self.fraction == self.fraction_limit {
                            return Err(std::fmt::Error);
                        }
                        self.fraction += 1;
                    }
                    b'0' if self.integer == 0 && !self.leading_zero => {
                        self.leading_zero = true;
                    }
                    b'0'..=b'9' => {
                        if self.integer == self.integer_limit {
                            return Err(std::fmt::Error);
                        }
                        self.integer += 1;
                    }
                    _ => return Err(std::fmt::Error),
                }
            }
            Ok(())
        }
    }
    let mut digits = Digits {
        integer_limit: qualifier.digits - qualifier.scale,
        fraction_limit: qualifier.scale,
        integer: 0,
        fraction: 0,
        after_dot: false,
        leading_zero: false,
    };
    number.write_xml(&mut digits).is_ok()
}
