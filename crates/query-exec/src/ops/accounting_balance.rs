use super::{BoxOp, Cx, OwnedCharge, accounting::check_cancel};
use crate::source::{Row, RowIter};
use bsl_prim::Number;
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{
    BalanceAreaExactL, BalanceAreaFieldL, BalanceAreasL, BalanceResourceL, BalanceRowsL,
};
use query_ir::{ExecError, QValue};
use std::collections::BTreeMap;
use std::ops::Bound;

fn fatal(what: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting Balance: {what}"))
}

fn areas_later() -> ExecError {
    ExecError::Later {
        what: "Q6a-AccountingBalanceAreas",
        stage: "Q7c",
    }
}

fn exact_area_matches(row: &Row, filters: &[BalanceAreaExactL]) -> Result<bool, ExecError> {
    for filter in filters {
        let cell = value(row, filter.field)?;
        if !crate::register::area_value_compatible(cell, &filter.value) {
            return Err(fatal("area separator type mismatch"));
        }
        if cell != &filter.value {
            return Ok(false);
        }
    }
    Ok(true)
}

fn stored_number<'row>(
    row: &'row Row,
    resource: &BalanceResourceL,
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
        QValue::Null => Err(fatal("nonnullable resource is NULL")),
        _ => Err(fatal("nonnullable resource is not numeric")),
    }
}

pub(super) struct LegacyCursor<'a> {
    accounts: BTreeMap<[u8; 16], (u8, Option<bool>)>,
    ready: bool,
    fields: Vec<FieldId>,
    chart_fields: Vec<FieldId>,
    observed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
    iter: Option<RowIter<'a>>,
    row: Option<(Row, OwnedCharge)>,
    eligible: [Option<(u8, Option<bool>)>; 2],
    side: usize,
    fields_charge: Option<OwnedCharge>,
    accounts_charge: Option<OwnedCharge>,
    observed_charge: Option<OwnedCharge>,
}

impl<'a> LegacyCursor<'a> {
    pub(super) fn new(
        step: &BalanceRowsL,
        seed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
        cx: &Cx<'_>,
    ) -> Result<Self, ExecError> {
        let main_area_count = match &step.areas {
            BalanceAreasL::None => 0,
            BalanceAreasL::Exact(t) => t.main.len(),
            BalanceAreasL::Unseparated(t) => t.main.len(),
        };
        let chart_area_count = match &step.areas {
            BalanceAreasL::None => 0,
            BalanceAreasL::Exact(t) => t.chart.len(),
            BalanceAreasL::Unseparated(t) => t.chart.len(),
        };
        if step.secured && step.chart_layout.is_some() && chart_area_count != 0 {
            return Err(areas_later());
        }
        let count = step
            .resources
            .len()
            .checked_add(step.dims.len())
            .and_then(|count| count.checked_mul(2))
            .and_then(|count| count.checked_add(3))
            .and_then(|count| count.checked_add(usize::from(step.period.is_some())))
            .and_then(|count| count.checked_add(step.policy_fields.len()))
            .and_then(|count| count.checked_add(main_area_count))
            .ok_or_else(|| fatal("field count overflow"))?;
        let chart_count = 2usize
            .checked_add(chart_area_count)
            .ok_or_else(|| fatal("chart field count overflow"))?;
        let charge = crate::model::size_add(
            crate::model::vec_block(count, std::mem::size_of::<FieldId>()),
            crate::model::vec_block(chart_count, std::mem::size_of::<FieldId>()),
        )?;
        let fields_charge = cx.mem.hold_owned(charge)?;
        let mut fields = Vec::with_capacity(count);
        fields.push(step.inputs.active);
        fields.extend(step.period);
        fields.extend(step.inputs.accounts);
        for dim in &step.dims {
            for field in dim.main {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        for resource in &step.resources {
            for field in resource.main {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        fields.extend(&step.policy_fields);
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for entry in &t.main {
                    if !fields.contains(&entry.field) {
                        fields.push(entry.field);
                    }
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for entry in &t.main {
                    if !fields.contains(&entry.field) {
                        fields.push(entry.field);
                    }
                }
            }
        }
        let mut chart_fields = Vec::with_capacity(chart_count);
        chart_fields.push(step.inputs.chart_id);
        if !chart_fields.contains(&step.inputs.chart_kind) {
            chart_fields.push(step.inputs.chart_kind);
        }
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for entry in &t.chart {
                    if !chart_fields.contains(&entry.field) {
                        chart_fields.push(entry.field);
                    }
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for entry in &t.chart {
                    if !chart_fields.contains(&entry.field) {
                        chart_fields.push(entry.field);
                    }
                }
            }
        }
        let mut observed_bytes = crate::model::size_vec(
            seed_areas.capacity(),
            std::mem::size_of::<(ibschema::values::Uuid16, QValue)>(),
        )?;
        for (_, val) in &seed_areas {
            observed_bytes =
                crate::model::size_add(observed_bytes, crate::model::value_model(val))?;
        }
        let observed_charge = cx.mem.hold_owned(observed_bytes)?;
        let accounts_charge = cx.mem.hold_owned(0)?;
        Ok(Self {
            accounts: BTreeMap::new(),
            ready: false,
            fields,
            chart_fields,
            observed_areas: seed_areas,
            iter: None,
            row: None,
            eligible: [None, None],
            side: 0,
            fields_charge: Some(fields_charge),
            accounts_charge: Some(accounts_charge),
            observed_charge: Some(observed_charge),
        })
    }

    fn check_unseparated_value(
        &mut self,
        common_attribute: ibschema::values::Uuid16,
        cell: &QValue,
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        if matches!(cell, QValue::Null | QValue::Undefined) {
            return Err(fatal("area separator is NULL or Undefined"));
        }
        if let Some((_, existing)) = self
            .observed_areas
            .iter()
            .find(|(ca, _)| *ca == common_attribute)
        {
            if !crate::register::area_value_compatible(cell, existing) {
                return Err(fatal("area separator type mismatch"));
            }
            if existing != cell {
                return Err(areas_later());
            }
        } else {
            let owner = self
                .observed_charge
                .as_mut()
                .ok_or_else(|| fatal("missing observed area owner"))?;
            let add = crate::model::size_add(
                crate::model::size_vec(
                    self.observed_areas.len().saturating_add(1).max(4),
                    std::mem::size_of::<(ibschema::values::Uuid16, QValue)>(),
                )?,
                crate::model::value_model(cell),
            )?;
            cx.mem.grow_owned(owner, add)?;
            self.observed_areas.push((common_attribute, cell.clone()));
        }
        Ok(())
    }

    fn check_unseparated_area(
        &mut self,
        row: &Row,
        fields: &[BalanceAreaFieldL],
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        for entry in fields {
            let cell = value(row, entry.field)?;
            self.check_unseparated_value(entry.common_attribute, cell, cx)?;
        }
        Ok(())
    }

    fn validate_unsep_tuple(
        &mut self,
        info: &TableInfo,
        fields: &[BalanceAreaFieldL],
        tuple: &[QValue],
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        for (sep_idx, sep) in info.separators.iter().enumerate() {
            let entry = fields
                .iter()
                .find(|e| e.field == sep.field)
                .ok_or_else(|| fatal("missing unseparated area field mapping"))?;
            let cell = tuple
                .get(sep_idx)
                .ok_or_else(|| fatal("data_areas tuple shorter than separators"))?;
            self.check_unseparated_value(entry.common_attribute, cell, cx)?;
        }
        Ok(())
    }

    fn verify_unsep_table_areas(
        &mut self,
        table: TableId,
        fields: &[BalanceAreaFieldL],
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if fields.is_empty() {
            return Ok(());
        }
        let info = cx
            .src
            .map()
            .try_table(table)
            .ok_or_else(|| fatal("missing table descriptor"))?;
        let sep_len = info.separators.len();
        if sep_len > 0
            && fields.len() == sep_len
            && info
                .separators
                .iter()
                .all(|sep| fields.iter().any(|e| e.field == sep.field))
            && let Some(ix) = find_sep_prefix_index(info)
            && let Some(areas) = super::access::data_areas(cx, table, ix, sep_len, false)?
        {
            for area in &areas {
                self.validate_unsep_tuple(info, fields, area, cx)?;
            }
            return Ok(());
        }
        let proj: Vec<FieldId> = fields.iter().map(|e| e.field).collect();
        let mut scan = cx.src.scan_fields(table, &proj)?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            self.check_unseparated_area(&row, fields, cx)?;
            cx.mem.release_scratch();
        }
        Ok(())
    }

    fn open_main_iter(
        &mut self,
        step: &BalanceRowsL,
        cx: &Cx<'a>,
    ) -> Result<RowIter<'a>, ExecError> {
        let exact_nonempty = matches!(&step.areas, BalanceAreasL::Exact(t) if !t.main.is_empty());
        let want_range = step.date.is_some() || exact_nonempty;
        if !want_range {
            return cx.src.scan_fields(step.inputs.main, &self.fields);
        }
        let Some(info) = cx.src.map().try_table(step.inputs.main) else {
            return cx.src.scan_fields(step.inputs.main, &self.fields);
        };
        let index = match step.period {
            Some(period) => find_by_period_index(info, period),
            None if exact_nonempty => find_sep_prefix_index(info),
            None => None,
        };
        let Some(index) = index else {
            return cx.src.scan_fields(step.inputs.main, &self.fields);
        };
        match &step.areas {
            BalanceAreasL::Exact(t) if !t.main.is_empty() => {
                let mut lo_vals = Vec::with_capacity(info.separators.len());
                for sep in &info.separators {
                    let entry = t
                        .main
                        .iter()
                        .find(|e| e.field == sep.field)
                        .ok_or_else(|| fatal("missing exact separator value for ByPeriod range"))?;
                    lo_vals.push(entry.value.clone());
                }
                if let Some(d) = step.date {
                    let mut hi_vals = lo_vals.clone();
                    hi_vals.push(QValue::Date(d));
                    cx.src.range_fields(
                        step.inputs.main,
                        index,
                        Bound::Included(lo_vals.as_slice()),
                        Bound::Excluded(hi_vals.as_slice()),
                        false,
                        &self.fields,
                    )
                } else {
                    cx.src.range_fields(
                        step.inputs.main,
                        index,
                        Bound::Included(lo_vals.as_slice()),
                        Bound::Included(lo_vals.as_slice()),
                        false,
                        &self.fields,
                    )
                }
            }
            BalanceAreasL::Unseparated(t) if !t.main.is_empty() => {
                let sep_len = info.separators.len();
                if let Some(d) = step.date
                    && sep_len > 0
                    && t.main.len() == sep_len
                    && index_has_sep_prefix(info, index)
                    && info
                        .separators
                        .iter()
                        .all(|sep| t.main.iter().any(|e| e.field == sep.field))
                    && let Some(areas) =
                        super::access::data_areas(cx, step.inputs.main, index, sep_len, false)?
                {
                    if areas.is_empty() {
                        return Ok(Box::new(std::iter::empty()));
                    }
                    for area in &areas {
                        self.validate_unsep_tuple(info, &t.main, area, cx)?;
                    }
                    let Some(prefix) = areas.front() else {
                        return Ok(Box::new(std::iter::empty()));
                    };
                    let mut hi_vals = prefix.clone();
                    hi_vals.push(QValue::Date(d));
                    return cx.src.range_fields(
                        step.inputs.main,
                        index,
                        Bound::Included(prefix.as_slice()),
                        Bound::Excluded(hi_vals.as_slice()),
                        false,
                        &self.fields,
                    );
                }
                cx.src.scan_fields(step.inputs.main, &self.fields)
            }
            _ => {
                if let Some(d) = step.date {
                    let bound = [QValue::Date(d)];
                    cx.src.range_fields(
                        step.inputs.main,
                        index,
                        Bound::Unbounded,
                        Bound::Excluded(bound.as_slice()),
                        false,
                        &self.fields,
                    )
                } else {
                    cx.src.scan_fields(step.inputs.main, &self.fields)
                }
            }
        }
    }

    fn load_accounts(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        check_cancel(cx.env)?;
        let inputs = &step.inputs;
        if step.chart_layout.is_some()
            && let BalanceAreasL::Unseparated(t) = &step.areas
            && !t.chart.is_empty()
        {
            self.verify_unsep_table_areas(inputs.chart, &t.chart, cx)?;
        }
        let mut rows = if step.chart_layout.is_none() {
            Some(cx.src.scan_fields(inputs.chart, &self.chart_fields)?)
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
            if step.chart_layout.is_none() {
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        if !exact_area_matches(&row, &t.chart)? {
                            cx.mem.release_scratch();
                            continue;
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        self.check_unseparated_area(&row, &t.chart, cx)?;
                    }
                }
            }
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
        if self.accounts.is_empty() {
            if let BalanceAreasL::Unseparated(t) = &step.areas
                && !t.main.is_empty()
            {
                self.verify_unsep_table_areas(step.inputs.main, &t.main, cx)?;
            }
            self.iter = Some(Box::new(std::iter::empty()));
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
                    self.iter = Some(self.open_main_iter(step, cx)?);
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
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        if !exact_area_matches(&row, &t.main)? {
                            cx.mem.release_scratch();
                            continue;
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        self.check_unseparated_area(&row, &t.main, cx)?;
                    }
                }
                match value(&row, step.inputs.active)? {
                    QValue::Bool(false) => {
                        cx.mem.release_scratch();
                        continue;
                    }
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
                let mut eligible = [None, None];
                for (slot, field) in eligible.iter_mut().zip(&step.inputs.accounts) {
                    *slot = match value(&row, *field)? {
                        QValue::Null => None,
                        QValue::Ref { table, id } if *table == step.inputs.ref_table => {
                            self.accounts.get(id).copied()
                        }
                        _ => return Err(fatal("account reference target mismatch")),
                    };
                }
                if eligible == [None, None] {
                    cx.mem.release_scratch();
                    continue;
                }
                for dim in &step.dims {
                    for side in 0..2 {
                        let field = dim
                            .main
                            .get(side)
                            .copied()
                            .ok_or_else(|| fatal("dimension side offset"))?;
                        match value(&row, field)? {
                            QValue::Null if !dim.nullable => {
                                return Err(fatal("nonnullable dimension is NULL"));
                            }
                            QValue::Number(number) => crate::num::guard(cx.env, number)?,
                            _ => {}
                        }
                    }
                }
                for resource in &step.resources {
                    for side in 0..2 {
                        if let Some(number) = stored_number(&row, resource, side)? {
                            crate::num::guard(cx.env, number)?;
                        }
                    }
                }
                let owner = cx.mem.hold_owned(charge)?;
                cx.mem.release_scratch();
                self.eligible = eligible;
                self.row = Some((row, owner));
                self.side = 0;
            }
            if self.side == 2 {
                self.row = None;
                continue;
            }
            let side = self.side;
            self.side += 1;
            let Some((kind, signal)) = *self
                .eligible
                .get(side)
                .ok_or_else(|| fatal("account side offset"))?
            else {
                continue;
            };
            let (row, _) = self.row.as_ref().ok_or_else(|| fatal("missing main row"))?;
            let field = step
                .inputs
                .accounts
                .get(side)
                .copied()
                .ok_or_else(|| fatal("account side offset"))?;
            let account = value(row, field)?;
            let dims = 2 + step.dims.len();
            let base = raw::width(dims, step.resources.len());
            let width = base
                + usize::from(step.chart_layout.is_some_and(|l| l.signal.is_some()))
                + step.policy_fields.len();
            let mut bytes = crate::model::vec_block(width, std::mem::size_of::<QValue>());
            for dim in &step.dims {
                let field = dim
                    .main
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("dimension side offset"))?;
                bytes =
                    crate::model::size_add(bytes, crate::model::value_model(value(row, field)?))?;
            }
            for resource in &step.resources {
                if stored_number(row, resource, side)?.is_some() {
                    let field = resource
                        .main
                        .get(side)
                        .copied()
                        .ok_or_else(|| fatal("resource side offset"))?;
                    bytes = bytes
                        .checked_add(
                            crate::model::value_model(value(row, field)?)
                                .checked_mul(2)
                                .ok_or_else(|| fatal("numeric model overflow"))?,
                        )
                        .ok_or_else(|| fatal("raw row model overflow"))?;
                }
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
            for (index, dim) in step.dims.iter().enumerate() {
                let field = dim
                    .main
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("dimension side offset"))?;
                *out.get_mut(raw::DIMS + 2 + index)
                    .ok_or_else(|| fatal("dimension output offset"))? = value(row, field)?.clone();
            }
            for (index, resource) in step.resources.iter().enumerate() {
                let n = match stored_number(row, resource, side)? {
                    Some(n) if side == 0 => n.clone(),
                    Some(n) => n.neg(),
                    None => Number::zero(),
                };
                *out.get_mut(raw::component(dims, index, raw::BALANCE))
                    .ok_or_else(|| fatal("resource output offset"))? = QValue::Number(n);
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
        self.eligible = [None, None];
        self.side = 2;
        self.fields = Vec::new();
        self.chart_fields = Vec::new();
        self.observed_areas = Vec::new();
        self.accounts = BTreeMap::new();
        self.fields_charge = None;
        self.accounts_charge = None;
        self.observed_charge = None;
    }
}

fn value(row: &Row, field: FieldId) -> Result<&QValue, ExecError> {
    row.get(usize::from(field.0))
        .ok_or_else(|| fatal("projected row is too short"))
}

pub(super) fn find_by_period_index(info: &TableInfo, period: FieldId) -> Option<IndexId> {
    let sep_len = info.separators.len();
    let matches_layout = |ix: &ibschema::storage::IndexInfo| -> bool {
        if ix.absent {
            return false;
        }
        let Some(prefix) = ix.fields.get(..sep_len) else {
            return false;
        };
        prefix
            .iter()
            .zip(&info.separators)
            .all(|(field, sep)| *field == sep.field)
            && ix.fields.get(sep_len) == Some(&period)
    };
    let preferred = info
        .indexes
        .iter()
        .enumerate()
        .find(|(_, ix)| ix.name == "ByPeriod" && matches_layout(ix))
        .or_else(|| {
            info.indexes
                .iter()
                .enumerate()
                .find(|(_, ix)| matches_layout(ix))
        });
    preferred.and_then(|(idx, _)| u16::try_from(idx).ok().map(IndexId::Named))
}

pub(super) fn find_sep_prefix_index(info: &TableInfo) -> Option<IndexId> {
    let sep_len = info.separators.len();
    if sep_len == 0 {
        return None;
    }
    let has_seps = |fields: &[FieldId]| -> bool {
        fields.get(..sep_len).is_some_and(|prefix| {
            prefix
                .iter()
                .zip(&info.separators)
                .all(|(field, sep)| *field == sep.field)
        })
    };
    if let Some(key) = &info.key_index
        && !key.absent
        && has_seps(&key.fields)
    {
        return Some(IndexId::Id);
    }
    info.indexes
        .iter()
        .enumerate()
        .find(|(_, ix)| !ix.absent && has_seps(&ix.fields))
        .and_then(|(idx, _)| u16::try_from(idx).ok().map(IndexId::Named))
}

pub(super) fn index_has_sep_prefix(info: &TableInfo, index: IndexId) -> bool {
    let sep_len = info.separators.len();
    let Some(fields) = crate::source::index_fields(info, index) else {
        return false;
    };
    fields.get(..sep_len).is_some_and(|prefix| {
        prefix
            .iter()
            .zip(&info.separators)
            .all(|(field, sep)| *field == sep.field)
    })
}

mod first;
mod owned;

#[allow(
    clippy::large_enum_variant,
    reason = "FirstCursor stays inline to avoid an uncharged heap allocation"
)]
enum Kind<'a> {
    Legacy(LegacyCursor<'a>),
    First(first::FirstCursor<'a>),
}
pub(super) struct Cursor<'a> {
    kind: Kind<'a>,
}
impl<'a> Cursor<'a> {
    pub(super) fn new(
        step: &BalanceRowsL,
        seed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
        cx: &Cx<'_>,
    ) -> Result<Self, ExecError> {
        let kind = match &step.ext1 {
            Some(_) => first::FirstCursor::new(step, seed_areas, cx).map(Kind::First),
            None => LegacyCursor::new(step, seed_areas, cx).map(Kind::Legacy),
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
