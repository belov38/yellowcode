use super::super::{BoxOp, Cx, OwnedCharge};
use super::owned::{PaidRow, PaidVec};
use super::{
    areas_later, exact_area_matches, fatal, find_by_period_index, find_sep_prefix_index,
    index_has_sep_prefix, value,
};
use crate::source::{Row, RowIter};
use bsl_prim::{
    BslDate, Number,
    number::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput},
};
use ibschema::storage::{FieldId, IndexId, TableId};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{
    BalanceAreaExactL, BalanceAreaFieldL, BalanceAreasL, BalanceExt1L, BalanceExtEdL, BalanceRowsL,
    BalanceValueCheckL,
};
use query_ir::{DateQ, ExecError, QValue};
use std::ops::Bound;

#[derive(Clone, Copy)]
enum Slot {
    Absent,
    Balance(u8),
    Turnover,
}
#[derive(Clone, Copy)]
struct Account {
    id: [u8; 16],
    kind: u8,
    signal: Option<bool>,
}

type EdKey = (BslDate, u32, [u8; 16], i64, u8, [u8; 16]);

#[derive(Clone, Copy)]
struct EdEntry {
    period: BslDate,
    rec_table: u32,
    rec_id: [u8; 16],
    line_no: i64,
    correspond: u8,
    kind: [u8; 16],
    row_idx: usize,
}

impl EdEntry {
    fn key(&self) -> EdKey {
        (
            self.period,
            self.rec_table,
            self.rec_id,
            self.line_no,
            self.correspond,
            self.kind,
        )
    }

    fn side_key(&self) -> (BslDate, u32, [u8; 16], i64, u8) {
        (
            self.period,
            self.rec_table,
            self.rec_id,
            self.line_no,
            self.correspond,
        )
    }
}

struct EdState<'a> {
    ed_fields: PaidVec<FieldId>,
    slot_kinds: Option<PaidVec<Option<[u8; 16]>>>,
    streaming: bool,
    ed_areas_verified: bool,
    ed_iter: Option<RowIter<'a>>,
    ed_peek: Option<(BslDate, PaidRow)>,
    bucket_period: Option<BslDate>,
    last_main_period: Option<BslDate>,
    last_ed_period: Option<BslDate>,
    bucket_rows: PaidVec<PaidRow>,
    bucket_entries: PaidVec<EdEntry>,
    current_key: Option<(BslDate, u32, [u8; 16], i64)>,
}

pub(super) struct FirstCursor<'a> {
    fields: Option<PaidVec<FieldId>>,
    chart_fields: Option<PaidVec<FieldId>>,
    section_fields: Option<PaidVec<FieldId>>,
    observed_areas: Option<PaidVec<(ibschema::values::Uuid16, QValue)>>,
    observed_owners: Option<PaidVec<OwnedCharge>>,
    accounts: Option<PaidVec<Account>>,
    slots: Option<PaidVec<Slot>>,
    slot_flags: Option<PaidVec<bool>>,
    ed_state: Option<EdState<'a>>,
    ready: bool,
    iter: Option<RowIter<'a>>,
    row: Option<PaidRow>,
    eligible: [Option<usize>; 2],
    side: usize,
    mask_step: usize,
}

fn cancel(cx: &Cx<'_>) -> Result<(), ExecError> {
    super::super::accounting::check_cancel(cx.env)
}
fn ext1(step: &BalanceRowsL) -> Result<&query_ir::lower::BalanceExt1L, ExecError> {
    step.ext1
        .as_deref()
        .ok_or_else(|| fatal("missing subconto payload"))
}

impl<'a> FirstCursor<'a> {
    pub(super) fn new(
        step: &BalanceRowsL,
        seed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
        cx: &Cx<'_>,
    ) -> Result<Self, ExecError> {
        let ext = ext1(step)?;
        if ext.slots.is_empty()
            || ext.slots.iter().any(|slot| slot.ordinal == 0)
            || ext
                .slots
                .windows(2)
                .any(|w| matches!(w, [a, b] if a.ordinal >= b.ordinal))
        {
            return Err(fatal("invalid subconto slot layout"));
        }
        let chart_has_areas = match &step.areas {
            BalanceAreasL::None => false,
            BalanceAreasL::Exact(t) => !t.chart.is_empty(),
            BalanceAreasL::Unseparated(t) => !t.chart.is_empty(),
        };
        if step.secured && step.chart_layout.is_some() && chart_has_areas {
            return Err(areas_later());
        }
        assert!(
            cx.env
                .memory
                .zip(cx.env.lease)
                .is_some_and(|(memory, lease)| {
                    lease.category() == memory_budget::Category::Execution
                        && memory.budget.same_scope(lease.budget())
                        && memory.budget.same_scope(ext.slots.reservation().budget())
                        && memory
                            .budget
                            .same_scope(ext.ext_flag_fields.reservation().budget())
                        && memory
                            .budget
                            .same_scope(ext.physical_values.reservation().budget())
                        && ext
                            .requested_kinds
                            .as_ref()
                            .is_none_or(|req| memory.budget.same_scope(req.reservation().budget()))
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
        if let Some(ed) = ext.ed {
            for field in [
                step.inputs.active,
                ed.main_period,
                ed.main_recorder,
                ed.main_line,
            ]
            .into_iter()
            .chain(step.period)
            .chain(step.inputs.accounts)
            .chain(step.dims.iter().flat_map(|d| d.main))
            .chain(step.resources.iter().flat_map(|r| r.main))
            .chain(step.policy_fields.iter().copied())
            {
                if !fields.contains(&field) {
                    fields.push(field, &cx.mem)?;
                }
            }
        } else {
            for field in [step.inputs.active]
                .into_iter()
                .chain(step.period)
                .chain(step.inputs.accounts)
                .chain(step.dims.iter().flat_map(|d| d.main))
                .chain(ext.slots.iter().flat_map(|slot| slot.values))
                .chain(ext.physical_values.iter().flat_map(|pair| *pair))
                .chain(step.resources.iter().flat_map(|r| r.main))
                .chain(step.policy_fields.iter().copied())
            {
                if !fields.contains(&field) {
                    fields.push(field, &cx.mem)?;
                }
            }
        }
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for entry in &t.main {
                    if !fields.contains(&entry.field) {
                        fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for entry in &t.main {
                    if !fields.contains(&entry.field) {
                        fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
        }
        let mut chart_fields = PaidVec::new(0, &cx.mem)?;
        for field in [step.inputs.chart_id, step.inputs.chart_kind] {
            if !chart_fields.contains(&field) {
                chart_fields.push(field, &cx.mem)?;
            }
        }
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for entry in &t.chart {
                    if !chart_fields.contains(&entry.field) {
                        chart_fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for entry in &t.chart {
                    if !chart_fields.contains(&entry.field) {
                        chart_fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
        }
        let mut section_fields = PaidVec::new(0, &cx.mem)?;
        for field in [ext.owner, ext.ordinal, ext.turnover_only]
            .into_iter()
            .chain(ext.dim_kind)
            .chain(ext.ext_flag_fields.iter().copied())
        {
            if !section_fields.contains(&field) {
                section_fields.push(field, &cx.mem)?;
            }
        }
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for entry in &t.section {
                    if !section_fields.contains(&entry.field) {
                        section_fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for entry in &t.section {
                    if !section_fields.contains(&entry.field) {
                        section_fields.push(entry.field, &cx.mem)?;
                    }
                }
            }
        }
        let mut observed_owners = PaidVec::new(0, &cx.mem)?;
        let mut observed_areas = PaidVec::new(seed_areas.len(), &cx.mem)?;
        for (ca, val) in seed_areas {
            let copied = copy_area_value(&val, &mut observed_owners, cx)?;
            observed_areas.push((ca, copied), &cx.mem)?;
        }
        let ed_state = if let Some(ed) = ext.ed {
            let mut ed_fields = PaidVec::new(0, &cx.mem)?;
            for field in [
                ed.period,
                ed.recorder,
                ed.line,
                ed.correspond,
                ed.kind,
                ed.value,
            ] {
                if !ed_fields.contains(&field) {
                    ed_fields.push(field, &cx.mem)?;
                }
            }
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    for entry in &t.ext_dims {
                        if !ed_fields.contains(&entry.field) {
                            ed_fields.push(entry.field, &cx.mem)?;
                        }
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    for entry in &t.ext_dims {
                        if !ed_fields.contains(&entry.field) {
                            ed_fields.push(entry.field, &cx.mem)?;
                        }
                    }
                }
            }
            Some(EdState {
                ed_fields,
                slot_kinds: None,
                streaming: ed.main_by_period.is_some() && ed.ed_by_period.is_some(),
                ed_areas_verified: false,
                ed_iter: None,
                ed_peek: None,
                bucket_period: None,
                last_main_period: None,
                last_ed_period: None,
                bucket_rows: PaidVec::new(0, &cx.mem)?,
                bucket_entries: PaidVec::new(0, &cx.mem)?,
                current_key: None,
            })
        } else {
            None
        };
        Ok(Self {
            fields: Some(fields),
            chart_fields: Some(chart_fields),
            section_fields: Some(section_fields),
            observed_areas: Some(observed_areas),
            observed_owners: Some(observed_owners),
            accounts: Some(PaidVec::new(0, &cx.mem)?),
            slots: None,
            slot_flags: None,
            ed_state,
            ready: false,
            iter: None,
            row: None,
            eligible: [None, None],
            side: 0,
            mask_step: 0,
        })
    }

    fn check_unseparated_area(
        &mut self,
        row: &Row,
        fields: &[BalanceAreaFieldL],
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        let observed = self
            .observed_areas
            .as_mut()
            .ok_or_else(|| fatal("closed subconto cursor"))?;
        let owners = self
            .observed_owners
            .as_mut()
            .ok_or_else(|| fatal("closed subconto cursor"))?;
        for entry in fields {
            let cell = value(row, entry.field)?;
            check_unsep_value(entry.common_attribute, cell, observed, owners, cx)?;
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
            && let Some(areas) = super::super::access::data_areas(cx, table, ix, sep_len, false)?
        {
            let observed = self
                .observed_areas
                .as_mut()
                .ok_or_else(|| fatal("closed subconto cursor"))?;
            let owners = self
                .observed_owners
                .as_mut()
                .ok_or_else(|| fatal("closed subconto cursor"))?;
            for area in &areas {
                validate_unsep_tuple(info, fields, area, observed, owners, cx)?;
            }
            return Ok(());
        }
        let mut proj = PaidVec::new(fields.len(), &cx.mem)?;
        for entry in fields {
            proj.push(entry.field, &cx.mem)?;
        }
        let mut scan = cx.src.scan_fields(table, proj.as_slice())?;
        loop {
            cancel(cx)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            let paid = PaidRow::import(row, cx)?;
            self.check_unseparated_area(&paid.row, fields, cx)?;
        }
        Ok(())
    }

    fn load(
        &mut self,
        step: &BalanceRowsL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        cancel(cx)?;
        if step.chart_layout.is_some()
            && let BalanceAreasL::Unseparated(t) = &step.areas
            && !t.chart.is_empty()
        {
            self.verify_unsep_table_areas(step.inputs.chart, &t.chart, cx)?;
        }
        let mut scan = if step.chart_layout.is_none() {
            Some(
                cx.src.scan_fields(
                    step.inputs.chart,
                    self.chart_fields
                        .as_ref()
                        .ok_or_else(|| fatal("closed subconto cursor"))?,
                )?,
            )
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
            if step.chart_layout.is_none() {
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        if !exact_area_matches(&paid.row, &t.chart)? {
                            continue;
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        self.check_unseparated_area(&paid.row, &t.chart, cx)?;
                    }
                }
            }
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
            .ok_or_else(|| fatal("closed subconto cursor"))?;
        accounts.sort_unstable_by_key(|entry| entry.id);
        let no_accounts = accounts.is_empty();
        let ext = ext1(step)?;
        if no_accounts {
            if let BalanceAreasL::Unseparated(t) = &step.areas {
                self.verify_unsep_table_areas(ext.section, &t.section, cx)?;
                self.verify_unsep_table_areas(step.inputs.main, &t.main, cx)?;
                if let Some(ed) = ext.ed {
                    self.verify_unsep_table_areas(ed.table, &t.ext_dims, cx)?;
                }
            }
            self.slots = Some(PaidVec::new(0, &cx.mem)?);
            self.iter = Some(Box::new(std::iter::empty()));
            if let Some(state) = self.ed_state.as_mut() {
                state.ed_areas_verified = true;
            }
            self.ready = true;
            return Ok(());
        }
        let slot_count = ext.slots.len();
        if slot_count > 64 {
            return Err(fatal("subconto slot mask overflow"));
        }
        let total_slots = accounts
            .len()
            .checked_mul(slot_count)
            .ok_or_else(|| fatal("slot matrix overflow"))?;
        cancel(cx)?;
        let mut slots = PaidVec::new(total_slots, &cx.mem)?;
        for _ in 0..total_slots {
            slots.push(Slot::Absent, &cx.mem)?;
        }
        let flag_count = ext.ext_flag_fields.len();
        let mut slot_flags = if flag_count > 0 {
            let total_flags = total_slots
                .checked_mul(flag_count)
                .ok_or_else(|| fatal("slot flag matrix overflow"))?;
            let mut flags = PaidVec::new(total_flags, &cx.mem)?;
            for _ in 0..total_flags {
                flags.push(false, &cx.mem)?;
            }
            Some(flags)
        } else {
            None
        };
        let mut slot_kinds = if ext.ed.is_some() {
            let mut kinds = PaidVec::new(total_slots, &cx.mem)?;
            for _ in 0..total_slots {
                kinds.push(None, &cx.mem)?;
            }
            Some(kinds)
        } else {
            None
        };
        let mut section = cx.src.scan_fields(
            ext.section,
            self.section_fields
                .as_ref()
                .ok_or_else(|| fatal("closed subconto cursor"))?,
        )?;
        loop {
            cancel(cx)?;
            cx.tick()?;
            let Some(row) = section.next().transpose()? else {
                break;
            };
            let paid = PaidRow::import(row, cx)?;
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    if !exact_area_matches(&paid.row, &t.section)? {
                        continue;
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    self.check_unseparated_area(&paid.row, &t.section, cx)?;
                }
            }
            let accounts = self
                .accounts
                .as_ref()
                .ok_or_else(|| fatal("closed subconto cursor"))?;
            let QValue::Number(ordinal) = value(&paid.row, ext.ordinal)? else {
                return Err(fatal("section ordinal is not numeric"));
            };
            let ordinal = ordinal
                .to_i64()
                .filter(|n| (1..=99_999).contains(n))
                .ok_or_else(|| fatal("invalid section ordinal"))?;
            let Ok(ordinal) = u8::try_from(ordinal) else {
                continue;
            };
            let (slot_idx, phys_idx, kind_uuid) = if let Some(ed) = ext.ed {
                let dim_kind_field = ext
                    .dim_kind
                    .ok_or_else(|| fatal("missing section DimKind field"))?;
                let QValue::Ref { table, id } = value(&paid.row, dim_kind_field)? else {
                    return Err(fatal("section DimKind is not a reference"));
                };
                if *table != ext.kind_ref_table {
                    return Err(fatal("section DimKind target mismatch"));
                }
                let slot_idx = if let Some(req_kinds) = &ext.requested_kinds {
                    if ordinal > ed.max_ext_dims {
                        continue;
                    }
                    let Some(req_pos) = req_kinds.iter().position(|k| k == id) else {
                        continue;
                    };
                    let Some(slot_idx) = ext
                        .slots
                        .iter()
                        .position(|slot| usize::from(slot.ordinal) == req_pos + 1)
                    else {
                        continue;
                    };
                    slot_idx
                } else {
                    let Some(slot_idx) = ext.slots.iter().position(|slot| slot.ordinal == ordinal)
                    else {
                        continue;
                    };
                    slot_idx
                };
                (slot_idx, 0u8, Some(*id))
            } else if let Some(req_kinds) = &ext.requested_kinds {
                let phys_idx = usize::from(ordinal)
                    .checked_sub(1)
                    .filter(|&i| i < ext.physical_values.len());
                let Some(phys_idx) = phys_idx else {
                    continue;
                };
                let dim_kind_field = ext
                    .dim_kind
                    .ok_or_else(|| fatal("missing section DimKind field"))?;
                let QValue::Ref { table, id } = value(&paid.row, dim_kind_field)? else {
                    return Err(fatal("section DimKind is not a reference"));
                };
                if *table != ext.kind_ref_table {
                    return Err(fatal("section DimKind target mismatch"));
                }
                let Some(req_pos) = req_kinds.iter().position(|k| k == id) else {
                    continue;
                };
                let Some(slot_idx) = ext
                    .slots
                    .iter()
                    .position(|slot| usize::from(slot.ordinal) == req_pos + 1)
                else {
                    continue;
                };
                let phys_u8 = u8::try_from(phys_idx)
                    .map_err(|_| fatal("subconto physical index overflow"))?;
                (slot_idx, phys_u8, None)
            } else {
                let Some(slot_idx) = ext.slots.iter().position(|slot| slot.ordinal == ordinal)
                else {
                    continue;
                };
                let slot_u8 =
                    u8::try_from(slot_idx).map_err(|_| fatal("subconto slot index overflow"))?;
                (slot_idx, slot_u8, None)
            };
            let QValue::Ref { table, id } = value(&paid.row, ext.owner)? else {
                return Err(fatal("section owner is not a reference"));
            };
            if *table != step.inputs.ref_table {
                return Err(fatal("section owner target mismatch"));
            }
            let Ok(index) = accounts.binary_search_by_key(id, |entry| entry.id) else {
                continue;
            };
            let flat = index
                .checked_mul(slot_count)
                .and_then(|base| base.checked_add(slot_idx))
                .ok_or_else(|| fatal("slot matrix index"))?;
            let entry_slot = slots
                .get_mut(flat)
                .ok_or_else(|| fatal("slot matrix index"))?;
            if !matches!(entry_slot, Slot::Absent) {
                return Err(fatal("duplicate subconto ordinal"));
            }
            *entry_slot = match value(&paid.row, ext.turnover_only)? {
                QValue::Bool(false) => {
                    if let Some(kinds) = slot_kinds.as_mut() {
                        *kinds
                            .get_mut(flat)
                            .ok_or_else(|| fatal("slot kind matrix index"))? = kind_uuid;
                    }
                    if let Some(flags) = slot_flags.as_mut() {
                        let flag_base = flat
                            .checked_mul(flag_count)
                            .ok_or_else(|| fatal("slot flag matrix index"))?;
                        for (f_idx, &flag_field) in ext.ext_flag_fields.iter().enumerate() {
                            let QValue::Bool(flag_val) = value(&paid.row, flag_field)? else {
                                return Err(fatal(
                                    "section ExtDimensionAccountingFlag is not Boolean",
                                ));
                            };
                            *flags
                                .get_mut(flag_base + f_idx)
                                .ok_or_else(|| fatal("slot flag matrix index"))? = *flag_val;
                        }
                    }
                    Slot::Balance(phys_idx)
                }
                QValue::Bool(true) => Slot::Turnover,
                _ => return Err(fatal("section TurnoverOnly is not Boolean")),
            };
        }
        drop(section);
        self.slots = Some(slots);
        self.slot_flags = slot_flags;
        if let Some(ed_state) = self.ed_state.as_mut() {
            ed_state.slot_kinds = slot_kinds;
        }
        if let Some(ed) = ext.ed
            && self.ed_state.as_ref().is_some_and(|state| !state.streaming)
        {
            self.load_ed_fallback(step, ext, ed, cx)?;
        }
        self.ready = true;
        Ok(())
    }

    fn load_ed_fallback(
        &mut self,
        step: &BalanceRowsL,
        ext: &BalanceExt1L,
        ed: BalanceExtEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let mut scan = cx.src.scan_fields(
            ed.table,
            &self
                .ed_state
                .as_ref()
                .ok_or_else(|| fatal("closed AccRgED cursor"))?
                .ed_fields,
        )?;
        loop {
            cancel(cx)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            let paid = PaidRow::import(row, cx)?;
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    if !exact_area_matches(&paid.row, &t.ext_dims)? {
                        continue;
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    self.check_unseparated_area(&paid.row, &t.ext_dims, cx)?;
                }
            }
            let QValue::Date(period) = value(&paid.row, ed.period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            let period = *period;
            if step.date.is_some_and(|date| period >= date) {
                continue;
            }
            let entry = decode_ed_entry(&paid.row, period, ed, ext.kind_ref_table, 0)?;
            let state = self
                .ed_state
                .as_mut()
                .ok_or_else(|| fatal("closed AccRgED cursor"))?;
            let row_idx = state.bucket_rows.len();
            state.bucket_rows.push(paid, &cx.mem)?;
            state
                .bucket_entries
                .push(EdEntry { row_idx, ..entry }, &cx.mem)?;
        }
        drop(scan);
        let state = self
            .ed_state
            .as_mut()
            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
        sort_and_check_ed_entries(&mut state.bucket_entries)
    }

    fn open_ed_iter(
        &mut self,
        step: &BalanceRowsL,
        ed: BalanceExtEdL,
        cx: &Cx<'a>,
    ) -> Result<(), ExecError> {
        let state = self
            .ed_state
            .as_mut()
            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
        if state.ed_iter.is_some() {
            return Ok(());
        }
        let ed_ix = ed
            .ed_by_period
            .ok_or_else(|| fatal("missing AccRgED ByPeriod index"))?;
        let (exact_ed, unsep_ed) = match &step.areas {
            BalanceAreasL::None => (&[][..], &[][..]),
            BalanceAreasL::Exact(t) => (t.ext_dims.as_slice(), &[][..]),
            BalanceAreasL::Unseparated(t) => (&[][..], t.ext_dims.as_slice()),
        };
        let observed = self
            .observed_areas
            .as_mut()
            .ok_or_else(|| fatal("closed subconto cursor"))?;
        let owners = self
            .observed_owners
            .as_mut()
            .ok_or_else(|| fatal("closed subconto cursor"))?;
        let (iter, verified) = open_by_period_range(
            cx,
            ed.table,
            ed_ix,
            &step.areas,
            exact_ed,
            unsep_ed,
            step.date,
            &state.ed_fields,
            observed,
            owners,
        )?;
        state.ed_iter = Some(iter);
        state.ed_areas_verified = verified;
        Ok(())
    }

    fn next_ed_streaming_row(
        &mut self,
        step: &BalanceRowsL,
        ed: BalanceExtEdL,
        cx: &mut Cx<'a>,
    ) -> Result<Option<(BslDate, PaidRow)>, ExecError> {
        let state = self
            .ed_state
            .as_mut()
            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
        if let Some(peek) = state.ed_peek.take() {
            return Ok(Some(peek));
        }
        self.open_ed_iter(step, ed, cx)?;
        loop {
            cancel(cx)?;
            cx.tick()?;
            let next = self
                .ed_state
                .as_mut()
                .ok_or_else(|| fatal("closed AccRgED cursor"))?
                .ed_iter
                .as_mut()
                .ok_or_else(|| fatal("missing AccRgED iterator"))?
                .next()
                .transpose()?;
            let Some(row) = next else {
                return Ok(None);
            };
            let paid = PaidRow::import(row, cx)?;
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    if !exact_area_matches(&paid.row, &t.ext_dims)? {
                        continue;
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    self.check_unseparated_area(&paid.row, &t.ext_dims, cx)?;
                }
            }
            let QValue::Date(period) = value(&paid.row, ed.period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            let period = *period;
            let state = self
                .ed_state
                .as_mut()
                .ok_or_else(|| fatal("closed AccRgED cursor"))?;
            if state.last_ed_period.is_some_and(|prev| period < prev) {
                return Err(fatal("AccRgED ByPeriod index out of order"));
            }
            state.last_ed_period = Some(period);
            if step.date.is_some_and(|date| period >= date) {
                continue;
            }
            return Ok(Some((period, paid)));
        }
    }

    fn advance_ed_bucket(
        &mut self,
        step: &BalanceRowsL,
        ext: &BalanceExt1L,
        ed: BalanceExtEdL,
        target_period: BslDate,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let state = self
            .ed_state
            .as_mut()
            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
        if state.bucket_period == Some(target_period) {
            return Ok(());
        }
        state.bucket_entries = PaidVec::new(0, &cx.mem)?;
        state.bucket_rows = PaidVec::new(0, &cx.mem)?;
        state.bucket_period = Some(target_period);
        while let Some((ed_period, paid)) = self.next_ed_streaming_row(step, ed, cx)? {
            if ed_period < target_period {
                continue;
            }
            if ed_period > target_period {
                let state = self
                    .ed_state
                    .as_mut()
                    .ok_or_else(|| fatal("closed AccRgED cursor"))?;
                state.ed_peek = Some((ed_period, paid));
                break;
            }
            let entry = decode_ed_entry(&paid.row, ed_period, ed, ext.kind_ref_table, 0)?;
            let state = self
                .ed_state
                .as_mut()
                .ok_or_else(|| fatal("closed AccRgED cursor"))?;
            let row_idx = state.bucket_rows.len();
            state.bucket_rows.push(paid, &cx.mem)?;
            state
                .bucket_entries
                .push(EdEntry { row_idx, ..entry }, &cx.mem)?;
        }
        let state = self
            .ed_state
            .as_mut()
            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
        sort_and_check_ed_entries(&mut state.bucket_entries)
    }

    fn drain_ed_unseparated_tail(
        &mut self,
        step: &BalanceRowsL,
        ed: BalanceExtEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if !matches!(step.areas, BalanceAreasL::Unseparated(_)) {
            return Ok(());
        }
        if self.ed_state.as_ref().is_some_and(|s| s.ed_areas_verified) {
            return Ok(());
        }
        self.open_ed_iter(step, ed, cx)?;
        if self.ed_state.as_ref().is_some_and(|s| s.ed_areas_verified) {
            return Ok(());
        }
        while self.next_ed_streaming_row(step, ed, cx)?.is_some() {}
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
                let ext = ext1(step)?;
                if self.iter.is_none() {
                    let fields = self
                        .fields
                        .as_ref()
                        .ok_or_else(|| fatal("closed I1 cursor"))?;
                    let (exact_main, unsep_main) = match &step.areas {
                        BalanceAreasL::None => (&[][..], &[][..]),
                        BalanceAreasL::Exact(t) => (t.main.as_slice(), &[][..]),
                        BalanceAreasL::Unseparated(t) => (&[][..], t.main.as_slice()),
                    };
                    let main_ix = if let Some(ed) = ext.ed
                        && self.ed_state.as_ref().is_some_and(|s| s.streaming)
                    {
                        Some(
                            ed.main_by_period
                                .ok_or_else(|| fatal("missing AccRg ByPeriod index"))?,
                        )
                    } else if step.date.is_some() || !exact_main.is_empty() {
                        cx.src.map().try_table(step.inputs.main).and_then(|info| {
                            match step.period {
                                Some(period) => find_by_period_index(info, period),
                                None if !exact_main.is_empty() => find_sep_prefix_index(info),
                                None => None,
                            }
                        })
                    } else {
                        None
                    };
                    self.iter = Some(if let Some(main_ix) = main_ix {
                        let observed = self
                            .observed_areas
                            .as_mut()
                            .ok_or_else(|| fatal("closed subconto cursor"))?;
                        let owners = self
                            .observed_owners
                            .as_mut()
                            .ok_or_else(|| fatal("closed subconto cursor"))?;
                        open_by_period_range(
                            cx,
                            step.inputs.main,
                            main_ix,
                            &step.areas,
                            exact_main,
                            unsep_main,
                            step.date,
                            fields,
                            observed,
                            owners,
                        )?
                        .0
                    } else {
                        cx.src.scan_fields(step.inputs.main, fields)?
                    });
                }
                let Some(row) = self
                    .iter
                    .as_mut()
                    .ok_or_else(|| fatal("missing main iterator"))?
                    .next()
                    .transpose()?
                else {
                    if let Some(ed) = ext.ed
                        && self.ed_state.as_ref().is_some_and(|s| s.streaming)
                    {
                        self.drain_ed_unseparated_tail(step, ed, cx)?;
                    }
                    if let Some(state) = self.ed_state.as_mut() {
                        state.bucket_entries = PaidVec::new(0, &cx.mem)?;
                        state.bucket_rows = PaidVec::new(0, &cx.mem)?;
                        state.ed_peek = None;
                        state.ed_iter = None;
                        state.current_key = None;
                    }
                    return Ok(None);
                };
                let paid = PaidRow::import(row, cx)?;
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        if !exact_area_matches(&paid.row, &t.main)? {
                            continue;
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        self.check_unseparated_area(&paid.row, &t.main, cx)?;
                    }
                }
                let main_period = if let Some(ed) = ext.ed {
                    let QValue::Date(period) = value(&paid.row, ed.main_period)? else {
                        return Err(fatal("main Period is not Date"));
                    };
                    let period = *period;
                    let state = self
                        .ed_state
                        .as_mut()
                        .ok_or_else(|| fatal("closed AccRgED cursor"))?;
                    if state.streaming {
                        if state.last_main_period.is_some_and(|prev| period < prev) {
                            return Err(fatal("AccRg ByPeriod index out of order"));
                        }
                        state.last_main_period = Some(period);
                    }
                    Some(period)
                } else {
                    None
                };
                match value(&paid.row, step.inputs.active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("main Active is not Boolean")),
                }
                if let Some(period) = main_period {
                    if step.date.is_some_and(|date| period >= date) {
                        continue;
                    }
                } else if let Some(field) = step.period {
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
                if let Some(ed) = ext.ed {
                    let period = main_period.ok_or_else(|| fatal("missing main Period"))?;
                    let QValue::Ref {
                        table: rec_table,
                        id: rec_id,
                    } = value(&paid.row, ed.main_recorder)?
                    else {
                        return Err(fatal("main Recorder is not a reference"));
                    };
                    let line_no = decode_line_no(value(&paid.row, ed.main_line)?, "main")?;
                    let rec_table = *rec_table;
                    let rec_id = *rec_id;
                    if self.ed_state.as_ref().is_some_and(|s| s.streaming) {
                        self.advance_ed_bucket(step, ext, ed, period, cx)?;
                    }
                    let state = self
                        .ed_state
                        .as_mut()
                        .ok_or_else(|| fatal("closed AccRgED cursor"))?;
                    for (side_idx, selected) in eligible.iter().enumerate() {
                        if selected.is_some() {
                            let side_u8 = u8::try_from(side_idx)
                                .map_err(|_| fatal("subconto side offset"))?;
                            let side_prefix = (period, rec_table, rec_id, line_no, side_u8);
                            let start = state
                                .bucket_entries
                                .partition_point(|e| e.side_key() < side_prefix);
                            for entry in state.bucket_entries.get(start..).into_iter().flatten() {
                                if entry.side_key() != side_prefix {
                                    break;
                                }
                                let ed_row = state
                                    .bucket_rows
                                    .get(entry.row_idx)
                                    .ok_or_else(|| fatal("AccRgED row index"))?;
                                validate(value(&ed_row.row, ed.value)?, &ext.value_check, cx)?;
                            }
                        }
                    }
                    state.current_key = Some((period, rec_table, rec_id, line_no));
                } else if ext.requested_kinds.is_some() {
                    for pair in ext.physical_values.iter() {
                        for (selected, field) in eligible.iter().zip(pair) {
                            if selected.is_some() {
                                validate(value(&paid.row, *field)?, &ext.value_check, cx)?;
                            }
                        }
                    }
                } else {
                    for ext_slot in ext.slots.iter() {
                        for (selected, field) in eligible.iter().zip(&ext_slot.values) {
                            if selected.is_some() {
                                validate(value(&paid.row, *field)?, &ext.value_check, cx)?;
                            }
                        }
                    }
                }
                for check in ext.shared_checks.iter() {
                    validate(value(&paid.row, check.field)?, &check.value, cx)?;
                }
                self.eligible = eligible;
                self.row = Some(paid);
                self.side = 0;
                self.mask_step = 0;
            }
            if self.side == 2 {
                self.row = None;
                self.mask_step = 0;
                continue;
            }
            let side = self.side;
            let Some(index) = *self
                .eligible
                .get(side)
                .ok_or_else(|| fatal("subconto side offset"))?
            else {
                self.side += 1;
                self.mask_step = 0;
                continue;
            };
            let entry = *self
                .accounts
                .as_ref()
                .ok_or_else(|| fatal("closed subconto cursor"))?
                .get(index)
                .ok_or_else(|| fatal("account index"))?;
            let account_slots = self
                .slots
                .as_ref()
                .ok_or_else(|| fatal("closed subconto cursor"))?;
            let slot_flags_slice = self.slot_flags.as_ref().map(|v| v.as_slice());
            let row = &self
                .row
                .as_ref()
                .ok_or_else(|| fatal("missing validated row"))?
                .row;
            let ext = ext1(step)?;
            let dim_count = step.dims.len();
            let slot_count = ext.slots.len();
            let flag_count = ext.ext_flag_fields.len();
            let row_base = index
                .checked_mul(slot_count)
                .ok_or_else(|| fatal("slot matrix index"))?;
            let (cur_mask, all_resources) = if flag_count == 0 || step.resources.is_empty() {
                let mask = resource_slot_mask(account_slots, None, 0, row_base, slot_count, None)?;
                self.side += 1;
                self.mask_step = 0;
                (mask, true)
            } else {
                let cur_res = step
                    .resources
                    .get(self.mask_step)
                    .ok_or_else(|| fatal("resource mask step offset"))?;
                let cur_mask = resource_slot_mask(
                    account_slots,
                    slot_flags_slice,
                    flag_count,
                    row_base,
                    slot_count,
                    cur_res.ext_flag,
                )?;
                let mut next_step = None;
                for r in (self.mask_step + 1)..step.resources.len() {
                    let cand_res = step
                        .resources
                        .get(r)
                        .ok_or_else(|| fatal("resource mask offset"))?;
                    let cand_mask = resource_slot_mask(
                        account_slots,
                        slot_flags_slice,
                        flag_count,
                        row_base,
                        slot_count,
                        cand_res.ext_flag,
                    )?;
                    let mut seen = false;
                    for p in 0..r {
                        let prev_res = step
                            .resources
                            .get(p)
                            .ok_or_else(|| fatal("resource mask offset"))?;
                        let prev_mask = resource_slot_mask(
                            account_slots,
                            slot_flags_slice,
                            flag_count,
                            row_base,
                            slot_count,
                            prev_res.ext_flag,
                        )?;
                        if prev_mask == cand_mask {
                            seen = true;
                            break;
                        }
                    }
                    if !seen {
                        next_step = Some(r);
                        break;
                    }
                }
                if let Some(next) = next_step {
                    self.mask_step = next;
                } else {
                    self.side += 1;
                    self.mask_step = 0;
                }
                (cur_mask, false)
            };
            let dims = 2 + dim_count + slot_count;
            let base = raw::width(dims, step.resources.len());
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
                .ok_or_else(|| fatal("subconto side offset"))?;
            *slot(&mut out, raw::DIMS)? = value(row, account)?.clone();
            *slot(&mut out, raw::DIMS + 1)? = QValue::Number(Number::from(u32::from(entry.kind)));
            for (pos, dim) in step.dims.iter().enumerate() {
                let field = *dim
                    .main
                    .get(side)
                    .ok_or_else(|| fatal("dimension side offset"))?;
                *slot(&mut out, raw::DIMS + 2 + pos)? =
                    copy(value(row, field)?, false, &mut owners, cx)?;
            }
            for pos in 0..slot_count {
                if (cur_mask & (1u64 << pos)) == 0 {
                    continue;
                }
                let kind_slot = *account_slots
                    .get(row_base + pos)
                    .ok_or_else(|| fatal("slot matrix index"))?;
                if let Slot::Balance(phys_idx) = kind_slot {
                    if let Some(ed) = ext.ed {
                        let state = self
                            .ed_state
                            .as_ref()
                            .ok_or_else(|| fatal("closed AccRgED cursor"))?;
                        let slot_kinds = state
                            .slot_kinds
                            .as_ref()
                            .ok_or_else(|| fatal("missing subconto slot kinds"))?;
                        let kind_uuid = (*slot_kinds
                            .get(row_base + pos)
                            .ok_or_else(|| fatal("slot kind matrix index"))?)
                        .ok_or_else(|| fatal("missing subconto kind"))?;
                        let (period, rec_table, rec_id, line_no) = state
                            .current_key
                            .ok_or_else(|| fatal("missing current movement key"))?;
                        let side_u8 =
                            u8::try_from(side).map_err(|_| fatal("subconto side offset"))?;
                        let lookup = (period, rec_table, rec_id, line_no, side_u8, kind_uuid);
                        if let Ok(entry_idx) = state
                            .bucket_entries
                            .binary_search_by_key(&lookup, EdEntry::key)
                        {
                            let entry = state
                                .bucket_entries
                                .get(entry_idx)
                                .ok_or_else(|| fatal("AccRgED entry index"))?;
                            let ed_row = state
                                .bucket_rows
                                .get(entry.row_idx)
                                .ok_or_else(|| fatal("AccRgED row index"))?;
                            *slot(&mut out, raw::DIMS + 2 + dim_count + pos)? =
                                copy(value(&ed_row.row, ed.value)?, false, &mut owners, cx)?;
                        }
                    } else {
                        let pair = if ext.requested_kinds.is_some() {
                            ext.physical_values.get(usize::from(phys_idx))
                        } else {
                            ext.slots.get(usize::from(phys_idx)).map(|s| &s.values)
                        }
                        .ok_or_else(|| fatal("subconto physical slot index"))?;
                        let field = *pair
                            .get(side)
                            .ok_or_else(|| fatal("subconto side offset"))?;
                        *slot(&mut out, raw::DIMS + 2 + dim_count + pos)? =
                            copy(value(row, field)?, false, &mut owners, cx)?;
                    }
                }
            }
            for (i, resource) in step.resources.iter().enumerate() {
                let res_active = all_resources
                    || resource_slot_mask(
                        account_slots,
                        slot_flags_slice,
                        flag_count,
                        row_base,
                        slot_count,
                        resource.ext_flag,
                    )? == cur_mask;
                let cell = if !res_active {
                    QValue::Number(Number::zero())
                } else {
                    match super::stored_number(row, resource, side)? {
                        Some(_) => {
                            let field = *resource
                                .main
                                .get(side)
                                .ok_or_else(|| fatal("subconto side offset"))?;
                            copy(value(row, field)?, side == 1, &mut owners, cx)?
                        }
                        None => QValue::Number(Number::zero()),
                    }
                };
                *slot(&mut out, raw::component(dims, i, raw::BALANCE))? = cell;
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
        self.chart_fields = None;
        self.section_fields = None;
        self.observed_areas = None;
        self.observed_owners = None;
        self.accounts = None;
        self.slots = None;
        self.slot_flags = None;
        self.ed_state = None;
        self.eligible = [None, None];
        self.side = 2;
        self.mask_step = 0;
    }
}

fn resource_slot_mask(
    account_slots: &[Slot],
    slot_flags: Option<&[bool]>,
    flag_count: usize,
    row_base: usize,
    slot_count: usize,
    ext_flag: Option<u16>,
) -> Result<u64, ExecError> {
    let mut mask = 0u64;
    for pos in 0..slot_count {
        let kind_slot = *account_slots
            .get(row_base + pos)
            .ok_or_else(|| fatal("slot matrix index"))?;
        if !matches!(kind_slot, Slot::Balance(_)) {
            continue;
        }
        let enabled = match ext_flag {
            None => true,
            Some(flag_idx) => {
                let flags = slot_flags.ok_or_else(|| fatal("missing slot flags"))?;
                let idx = (row_base + pos)
                    .checked_mul(flag_count)
                    .and_then(|base| base.checked_add(usize::from(flag_idx)))
                    .ok_or_else(|| fatal("slot flag index overflow"))?;
                *flags.get(idx).ok_or_else(|| fatal("slot flag index"))?
            }
        };
        if enabled {
            mask |= 1u64 << pos;
        }
    }
    Ok(mask)
}

fn decode_line_no(val: &QValue, label: &str) -> Result<i64, ExecError> {
    let QValue::Number(number) = val else {
        return Err(fatal(format!("{label} LineNo is not numeric")));
    };
    number
        .to_i64()
        .filter(|n| *n >= 1)
        .ok_or_else(|| fatal(format!("invalid {label} LineNo")))
}

fn decode_ed_entry(
    row: &Row,
    period: BslDate,
    ed: BalanceExtEdL,
    kind_ref_table: u32,
    row_idx: usize,
) -> Result<EdEntry, ExecError> {
    let QValue::Ref {
        table: rec_table,
        id: rec_id,
    } = value(row, ed.recorder)?
    else {
        return Err(fatal("AccRgED Recorder is not a reference"));
    };
    let line_no = decode_line_no(value(row, ed.line)?, "AccRgED")?;
    let QValue::Number(corr_num) = value(row, ed.correspond)? else {
        return Err(fatal("AccRgED Correspond is not numeric"));
    };
    let correspond = match corr_num.to_i64() {
        Some(0) => 0u8,
        Some(1) => 1u8,
        _ => return Err(fatal("invalid AccRgED Correspond")),
    };
    let QValue::Ref {
        table: kind_table,
        id: kind_id,
    } = value(row, ed.kind)?
    else {
        return Err(fatal("AccRgED Kind is not a reference"));
    };
    if *kind_table != kind_ref_table {
        return Err(fatal("AccRgED Kind target mismatch"));
    }
    Ok(EdEntry {
        period,
        rec_table: *rec_table,
        rec_id: *rec_id,
        line_no,
        correspond,
        kind: *kind_id,
        row_idx,
    })
}

fn sort_and_check_ed_entries(entries: &mut PaidVec<EdEntry>) -> Result<(), ExecError> {
    entries.sort_unstable_by_key(EdEntry::key);
    if entries
        .windows(2)
        .any(|w| matches!(w, [a, b] if a.key() == b.key()))
    {
        return Err(fatal("duplicate AccRgED entry"));
    }
    Ok(())
}

fn check_unsep_value(
    common_attribute: ibschema::values::Uuid16,
    cell: &QValue,
    observed: &mut PaidVec<(ibschema::values::Uuid16, QValue)>,
    owners: &mut PaidVec<OwnedCharge>,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    if matches!(cell, QValue::Null | QValue::Undefined) {
        return Err(fatal("area separator is NULL or Undefined"));
    }
    if let Some((_, existing)) = observed.iter().find(|(ca, _)| *ca == common_attribute) {
        if !crate::register::area_value_compatible(cell, existing) {
            return Err(fatal("area separator type mismatch"));
        }
        if existing != cell {
            return Err(areas_later());
        }
    } else {
        observed.reserve_one(&cx.mem)?;
        let copied = copy_area_value(cell, owners, cx)?;
        observed.push((common_attribute, copied), &cx.mem)?;
    }
    Ok(())
}

fn validate_unsep_tuple(
    info: &ibschema::storage::TableInfo,
    fields: &[BalanceAreaFieldL],
    tuple: &[QValue],
    observed: &mut PaidVec<(ibschema::values::Uuid16, QValue)>,
    owners: &mut PaidVec<OwnedCharge>,
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
        check_unsep_value(entry.common_attribute, cell, observed, owners, cx)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "range opener takes table, index, area descriptors, optional date, projected fields, and observed area state"
)]
fn open_by_period_range<'a>(
    cx: &Cx<'a>,
    table: TableId,
    index: IndexId,
    areas: &BalanceAreasL,
    exact_entries: &[BalanceAreaExactL],
    unsep_entries: &[BalanceAreaFieldL],
    date: Option<BslDate>,
    fields: &[FieldId],
    observed_areas: &mut PaidVec<(ibschema::values::Uuid16, QValue)>,
    observed_owners: &mut PaidVec<OwnedCharge>,
) -> Result<(RowIter<'a>, bool), ExecError> {
    match areas {
        BalanceAreasL::Exact(_) if !exact_entries.is_empty() => {
            let info = cx
                .src
                .map()
                .try_table(table)
                .ok_or_else(|| fatal("missing ByPeriod table info"))?;
            let mut owners = PaidVec::new(0, &cx.mem)?;
            let mut lo_vals = PaidVec::new(info.separators.len(), &cx.mem)?;
            for sep in &info.separators {
                let entry = exact_entries
                    .iter()
                    .find(|e| e.field == sep.field)
                    .ok_or_else(|| fatal("missing exact separator value for ByPeriod range"))?;
                let copied = copy_area_value(&entry.value, &mut owners, cx)?;
                lo_vals.push(copied, &cx.mem)?;
            }
            if let Some(d) = date {
                let mut hi_vals = PaidVec::new(lo_vals.len() + 1, &cx.mem)?;
                for v in lo_vals.iter() {
                    let copied = copy_area_value(v, &mut owners, cx)?;
                    hi_vals.push(copied, &cx.mem)?;
                }
                hi_vals.push(QValue::Date(d), &cx.mem)?;
                Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Included(lo_vals.as_slice()),
                        Bound::Excluded(hi_vals.as_slice()),
                        false,
                        fields,
                    )?,
                    false,
                ))
            } else {
                Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Included(lo_vals.as_slice()),
                        Bound::Included(lo_vals.as_slice()),
                        false,
                        fields,
                    )?,
                    false,
                ))
            }
        }
        BalanceAreasL::Unseparated(_) if !unsep_entries.is_empty() => {
            let info = cx
                .src
                .map()
                .try_table(table)
                .ok_or_else(|| fatal("missing ByPeriod table info"))?;
            let sep_len = info.separators.len();
            if let Some(d) = date
                && sep_len > 0
                && unsep_entries.len() == sep_len
                && index_has_sep_prefix(info, index)
                && info
                    .separators
                    .iter()
                    .all(|sep| unsep_entries.iter().any(|e| e.field == sep.field))
                && let Some(data_areas) =
                    super::super::access::data_areas(cx, table, index, sep_len, false)?
            {
                if data_areas.is_empty() {
                    return Ok((Box::new(std::iter::empty()), true));
                }
                for area in &data_areas {
                    validate_unsep_tuple(
                        info,
                        unsep_entries,
                        area,
                        observed_areas,
                        observed_owners,
                        cx,
                    )?;
                }
                let Some(prefix) = data_areas.front() else {
                    return Ok((Box::new(std::iter::empty()), true));
                };
                let mut owners = PaidVec::new(0, &cx.mem)?;
                let mut lo_vals = PaidVec::new(prefix.len(), &cx.mem)?;
                for v in prefix {
                    let copied = copy_area_value(v, &mut owners, cx)?;
                    lo_vals.push(copied, &cx.mem)?;
                }
                let mut hi_vals = PaidVec::new(prefix.len() + 1, &cx.mem)?;
                for v in lo_vals.iter() {
                    let copied = copy_area_value(v, &mut owners, cx)?;
                    hi_vals.push(copied, &cx.mem)?;
                }
                hi_vals.push(QValue::Date(d), &cx.mem)?;
                return Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Included(lo_vals.as_slice()),
                        Bound::Excluded(hi_vals.as_slice()),
                        false,
                        fields,
                    )?,
                    true,
                ));
            }
            Ok((
                cx.src.range_fields(
                    table,
                    index,
                    Bound::Unbounded,
                    Bound::Unbounded,
                    false,
                    fields,
                )?,
                false,
            ))
        }
        _ => {
            if let Some(d) = date {
                let bound = [QValue::Date(d)];
                Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Unbounded,
                        Bound::Excluded(bound.as_slice()),
                        false,
                        fields,
                    )?,
                    false,
                ))
            } else {
                Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Unbounded,
                        Bound::Unbounded,
                        false,
                        fields,
                    )?,
                    false,
                ))
            }
        }
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

fn copy_area_value(
    value: &QValue,
    owners: &mut PaidVec<OwnedCharge>,
    cx: &Cx<'_>,
) -> Result<QValue, ExecError> {
    if matches!(value, QValue::Number(_)) {
        return copy(value, false, owners, cx);
    }
    let bytes = crate::model::value_model(value);
    if bytes > 0 {
        owners.reserve_one(&cx.mem)?;
        let owner = cx.mem.hold_owned(bytes)?;
        owners.push(owner, &cx.mem)?;
    }
    Ok(value.clone())
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
