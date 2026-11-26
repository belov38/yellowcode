use super::{BoxOp, Cx, OwnedCharge, accounting::check_cancel};
use crate::source::{Row, RowIter};
use bsl_prim::{BslDate, Number};
use ibschema::storage::{FieldId, IndexId, TableId, TableInfo};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{
    AccountingBalanceAndTurnoversAccRgEdL, AccountingBalanceAndTurnoversExtL,
    AccountingBalanceAndTurnoversL, BalanceAreaExactL, BalanceAreaFieldL, BalanceAreasL,
    BalanceResourceL, TurnoversValueCheckL,
};
use query_ir::value::register_bounds::{Bnd, Point, after_start, before_end};
use query_ir::{DateQ, ExecError, NumQ, QValue};
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;

fn fatal(what: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting BalanceAndTurnovers: {what}"))
}

fn areas_later() -> ExecError {
    ExecError::Later {
        what: "Q6c-AccountingBalanceAndTurnoversAreas",
        stage: "Q7c",
    }
}

fn value(row: &Row, field: FieldId) -> Result<&QValue, ExecError> {
    row.get(usize::from(field.0))
        .ok_or_else(|| fatal("missing stored field"))
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
        _ => Err(fatal("resource is not numeric")),
    }
}

fn numeric_fits(number: &Number, qualifier: NumQ) -> bool {
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

fn validate_ext_value(
    val: &QValue,
    check: &TurnoversValueCheckL,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    let valid = match val {
        QValue::Null => check.nullable,
        QValue::Undefined => check.undefined,
        QValue::Bool(_) => check.boolean,
        QValue::Number(number) => {
            crate::num::guard(cx.env, number)?;
            check.number.is_some_and(|q| numeric_fits(number, q))
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
            "stored ExtDimension value contradicts semantic qualifier",
        ))
    }
}

fn parse_line_no(val: &QValue, cx: &Cx<'_>) -> Result<i64, ExecError> {
    let QValue::Number(line) = val else {
        return Err(fatal("LineNo is not Number"));
    };
    crate::num::guard(cx.env, line)?;
    line.to_i64()
        .ok_or_else(|| fatal("LineNo must be an integer"))
}

fn parse_correspond(val: &QValue) -> Result<u8, ExecError> {
    let QValue::Number(corr) = val else {
        return Err(fatal("AccRgED Correspond is not Number"));
    };
    match corr.to_i64() {
        Some(0) => Ok(0),
        Some(1) => Ok(1),
        _ => Err(fatal("AccRgED Correspond must be 0 or 1")),
    }
}

fn find_by_period_index(info: &TableInfo, period: FieldId) -> Option<IndexId> {
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

fn find_sep_prefix_index(info: &TableInfo) -> Option<IndexId> {
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

fn index_has_sep_prefix(info: &TableInfo, index: IndexId) -> bool {
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

fn bound_date(b: Bnd) -> Option<BslDate> {
    match b {
        Bnd::None => None,
        Bnd::Date(d) | Bnd::Moment(d, _) => Some(d),
        Bnd::Incl(Point::Date(d) | Point::Moment(d, _))
        | Bnd::Excl(Point::Date(d) | Point::Moment(d, _)) => Some(d),
    }
}

#[derive(Clone, Copy)]
struct ExtRow {
    account_id: [u8; 16],
    ordinal: u8,
    dim_kind: [u8; 16],
    turnover_only: bool,
    flag_bits: u64,
}

type EdKey = (u32, [u8; 16], i64, u8);

struct EdEntry {
    key: EdKey,
    kind: [u8; 16],
    value: QValue,
    _val_charge: Option<OwnedCharge>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdMode {
    Inline,
    ByPeriod {
        main_ix: IndexId,
        ed_ix: IndexId,
    },
    ByRecorder {
        ed_ix: IndexId,
        has_correspond: bool,
    },
    FullScan,
}

pub(super) struct Cursor<'a> {
    start: Bnd,
    end: Bnd,
    fields: Vec<FieldId>,
    chart_fields: Vec<FieldId>,
    section_fields: Vec<FieldId>,
    ed_fields: Vec<FieldId>,
    fields_charge: Option<OwnedCharge>,
    observed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
    observed_charge: Option<OwnedCharge>,
    accounts: BTreeMap<[u8; 16], (u8, Option<bool>)>,
    accounts_charge: Option<OwnedCharge>,
    ext_rows: Vec<ExtRow>,
    ext_charge: Option<OwnedCharge>,
    ed_mode: EdMode,
    ed_iter: Option<RowIter<'a>>,
    ed_opened: bool,
    ed_areas_verified: bool,
    ed_exhausted: bool,
    ed_last_period: Option<BslDate>,
    ed_peek: Option<(BslDate, Row, OwnedCharge)>,
    ed_bucket_period: Option<BslDate>,
    ed_bucket: Vec<EdEntry>,
    ed_bucket_charge: Option<OwnedCharge>,
    ed_loaded_full: bool,
    main_last_period: Option<BslDate>,
    ready: bool,
    done: bool,
    iter: Option<RowIter<'a>>,
    emit_queue: VecDeque<(Row, OwnedCharge)>,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(
        step: &AccountingBalanceAndTurnoversL,
        start: Bnd,
        end: Bnd,
        seed_areas: Vec<(ibschema::values::Uuid16, QValue)>,
        cx: &Cx<'_>,
    ) -> Result<Self, ExecError> {
        let chart_area_count = match &step.areas {
            BalanceAreasL::None => 0,
            BalanceAreasL::Exact(t) => t.chart.len(),
            BalanceAreasL::Unseparated(t) => t.chart.len(),
        };
        if step.secured && step.chart_layout.is_some() && chart_area_count != 0 {
            return Err(areas_later());
        }
        let mut fields = Vec::new();
        let push_unique = |vec: &mut Vec<FieldId>, f: FieldId| {
            if !vec.contains(&f) {
                vec.push(f);
            }
        };
        push_unique(&mut fields, step.inputs.active);
        push_unique(&mut fields, step.period);
        for f in step.inputs.accounts {
            push_unique(&mut fields, f);
        }
        if let Some(rk) = step.record_kind {
            push_unique(&mut fields, rk);
        }
        if let Some(rec) = &step.recorder {
            push_unique(&mut fields, rec.field);
        }
        if let Some(line) = step.line {
            push_unique(&mut fields, line);
        }
        for dim in &step.dims {
            for f in dim.main {
                push_unique(&mut fields, f);
            }
        }
        for res in &step.resources {
            for f in res.main {
                push_unique(&mut fields, f);
            }
        }
        if let Some(ext) = &step.ext_dims {
            if let Some(ed) = &ext.acc_rg_ed {
                push_unique(&mut fields, ed.main_period);
                push_unique(&mut fields, ed.main_recorder);
                push_unique(&mut fields, ed.main_line);
            } else {
                for slot in &ext.slots {
                    if let Some(pair) = slot.values {
                        for f in pair {
                            push_unique(&mut fields, f);
                        }
                    }
                }
                for pair in &ext.physical_values {
                    for f in *pair {
                        push_unique(&mut fields, f);
                    }
                }
            }
        }
        for &f in &step.policy_fields {
            push_unique(&mut fields, f);
        }
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for e in &t.main {
                    push_unique(&mut fields, e.field);
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for e in &t.main {
                    push_unique(&mut fields, e.field);
                }
            }
        }

        let mut chart_fields = Vec::new();
        push_unique(&mut chart_fields, step.inputs.chart_id);
        push_unique(&mut chart_fields, step.inputs.chart_kind);
        match &step.areas {
            BalanceAreasL::None => {}
            BalanceAreasL::Exact(t) => {
                for e in &t.chart {
                    push_unique(&mut chart_fields, e.field);
                }
            }
            BalanceAreasL::Unseparated(t) => {
                for e in &t.chart {
                    push_unique(&mut chart_fields, e.field);
                }
            }
        }

        let mut section_fields = Vec::new();
        let mut ed_fields = Vec::new();
        let mut ed_mode = EdMode::Inline;
        if let Some(ext) = &step.ext_dims {
            push_unique(&mut section_fields, ext.owner);
            push_unique(&mut section_fields, ext.ordinal);
            push_unique(&mut section_fields, ext.turnover_only);
            if let Some(dk) = ext.dim_kind {
                push_unique(&mut section_fields, dk);
            }
            for &ff in &ext.ext_flag_fields {
                push_unique(&mut section_fields, ff);
            }
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    for e in &t.section {
                        push_unique(&mut section_fields, e.field);
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    for e in &t.section {
                        push_unique(&mut section_fields, e.field);
                    }
                }
            }
            if let Some(ed) = &ext.acc_rg_ed {
                push_unique(&mut ed_fields, ed.ed_period);
                push_unique(&mut ed_fields, ed.ed_recorder);
                push_unique(&mut ed_fields, ed.ed_line);
                if let Some(c) = ed.ed_correspond {
                    push_unique(&mut ed_fields, c);
                }
                push_unique(&mut ed_fields, ed.ed_kind);
                push_unique(&mut ed_fields, ed.ed_value);
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        for e in &t.ext_dims {
                            push_unique(&mut ed_fields, e.field);
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        for e in &t.ext_dims {
                            push_unique(&mut ed_fields, e.field);
                        }
                    }
                }
                let is_unsep = matches!(step.areas, BalanceAreasL::Unseparated(_));
                ed_mode = match (ed.main_by_period, ed.ed_by_period, ed.ed_by_recorder) {
                    (Some(main_ix), Some(ed_ix), _) => EdMode::ByPeriod { main_ix, ed_ix },
                    (_, _, Some((ed_ix, has_correspond))) if !is_unsep => EdMode::ByRecorder {
                        ed_ix,
                        has_correspond,
                    },
                    _ => EdMode::FullScan,
                };
            }
        }

        let field_bytes = crate::model::size_add(
            crate::model::size_add(
                crate::model::vec_block(fields.capacity(), std::mem::size_of::<FieldId>()),
                crate::model::vec_block(chart_fields.capacity(), std::mem::size_of::<FieldId>()),
            )?,
            crate::model::size_add(
                crate::model::vec_block(section_fields.capacity(), std::mem::size_of::<FieldId>()),
                crate::model::vec_block(ed_fields.capacity(), std::mem::size_of::<FieldId>()),
            )?,
        )?;
        let fields_charge = cx.mem.hold_owned(field_bytes)?;
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
        let ext_charge = cx.mem.hold_owned(0)?;
        let ed_bucket_charge = cx.mem.hold_owned(0)?;
        Ok(Self {
            start,
            end,
            fields,
            chart_fields,
            section_fields,
            ed_fields,
            fields_charge: Some(fields_charge),
            observed_areas: seed_areas,
            observed_charge: Some(observed_charge),
            accounts: BTreeMap::new(),
            accounts_charge: Some(accounts_charge),
            ext_rows: Vec::new(),
            ext_charge: Some(ext_charge),
            ed_mode,
            ed_iter: None,
            ed_opened: false,
            ed_areas_verified: false,
            ed_exhausted: false,
            ed_last_period: None,
            ed_peek: None,
            ed_bucket_period: None,
            ed_bucket: Vec::new(),
            ed_bucket_charge: Some(ed_bucket_charge),
            ed_loaded_full: false,
            main_last_period: None,
            ready: false,
            done: false,
            iter: None,
            emit_queue: VecDeque::new(),
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

    fn period_scan_bounds(&self, need_opening: bool) -> (Option<BslDate>, Option<(BslDate, bool)>) {
        let lo = if need_opening {
            None
        } else {
            bound_date(self.start)
        };
        let hi = match self.end {
            Bnd::None => None,
            Bnd::Excl(Point::Date(d)) => Some((d, false)),
            other => bound_date(other).map(|d| (d, true)),
        };
        (lo, hi)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_period_range(
        &mut self,
        table: TableId,
        index: IndexId,
        exact: &[BalanceAreaExactL],
        unsep: &[BalanceAreaFieldL],
        areas: &BalanceAreasL,
        need_opening: bool,
        fields: &[FieldId],
        cx: &Cx<'a>,
    ) -> Result<(RowIter<'a>, bool), ExecError> {
        let info = cx
            .src
            .map()
            .try_table(table)
            .ok_or_else(|| fatal("missing table descriptor"))?;
        let (lo_date, hi_date) = self.period_scan_bounds(need_opening);
        if info.separators.is_empty() {
            let lo_vec = lo_date.map(|d| vec![QValue::Date(d)]);
            let hi_vec = hi_date.map(|(d, _)| vec![QValue::Date(d)]);
            let lo = match &lo_vec {
                Some(v) => Bound::Included(v.as_slice()),
                None => Bound::Unbounded,
            };
            let hi = match (&hi_vec, hi_date) {
                (Some(v), Some((_, true))) => Bound::Included(v.as_slice()),
                (Some(v), Some((_, false))) => Bound::Excluded(v.as_slice()),
                _ => Bound::Unbounded,
            };
            return Ok((
                cx.src.range_fields(table, index, lo, hi, false, fields)?,
                false,
            ));
        }
        match areas {
            BalanceAreasL::Exact(_) if !exact.is_empty() => {
                let mut prefix = Vec::with_capacity(info.separators.len());
                for sep in &info.separators {
                    let entry = exact
                        .iter()
                        .find(|e| e.field == sep.field)
                        .ok_or_else(|| fatal("missing exact separator value for ByPeriod range"))?;
                    prefix.push(entry.value.clone());
                }
                let mut lo_vec = prefix.clone();
                if let Some(d) = lo_date {
                    lo_vec.push(QValue::Date(d));
                }
                let mut hi_vec = prefix;
                let hi_excl = if let Some((d, incl)) = hi_date {
                    hi_vec.push(QValue::Date(d));
                    !incl
                } else {
                    false
                };
                let hi = if hi_excl {
                    Bound::Excluded(hi_vec.as_slice())
                } else {
                    Bound::Included(hi_vec.as_slice())
                };
                Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Included(lo_vec.as_slice()),
                        hi,
                        false,
                        fields,
                    )?,
                    false,
                ))
            }
            BalanceAreasL::Unseparated(_) if !unsep.is_empty() => {
                let sep_len = info.separators.len();
                let has_bound = lo_date.is_some() || hi_date.is_some();
                if has_bound
                    && sep_len > 0
                    && unsep.len() == sep_len
                    && index_has_sep_prefix(info, index)
                    && info
                        .separators
                        .iter()
                        .all(|sep| unsep.iter().any(|e| e.field == sep.field))
                    && let Some(areas_found) =
                        super::access::data_areas(cx, table, index, sep_len, false)?
                {
                    if areas_found.is_empty() {
                        return Ok((Box::new(std::iter::empty()), true));
                    }
                    for area in &areas_found {
                        self.validate_unsep_tuple(info, unsep, area, cx)?;
                    }
                    let Some(prefix) = areas_found.front() else {
                        return Ok((Box::new(std::iter::empty()), true));
                    };
                    let mut lo_vec = prefix.clone();
                    if let Some(d) = lo_date {
                        lo_vec.push(QValue::Date(d));
                    }
                    let mut hi_vec = prefix.clone();
                    let hi_excl = if let Some((d, incl)) = hi_date {
                        hi_vec.push(QValue::Date(d));
                        !incl
                    } else {
                        false
                    };
                    let hi = if hi_excl {
                        Bound::Excluded(hi_vec.as_slice())
                    } else {
                        Bound::Included(hi_vec.as_slice())
                    };
                    return Ok((
                        cx.src.range_fields(
                            table,
                            index,
                            Bound::Included(lo_vec.as_slice()),
                            hi,
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
            _ => Ok((
                cx.src.range_fields(
                    table,
                    index,
                    Bound::Unbounded,
                    Bound::Unbounded,
                    false,
                    fields,
                )?,
                false,
            )),
        }
    }

    fn load(
        &mut self,
        step: &AccountingBalanceAndTurnoversL,
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
            cx.mem.release_scratch();
        }
        if let Some(mut input) = child.take() {
            input.close(cx);
        }
        if self.accounts.is_empty() {
            if let BalanceAreasL::Unseparated(t) = &step.areas {
                if !t.main.is_empty() {
                    self.verify_unsep_table_areas(step.inputs.main, &t.main, cx)?;
                }
                if let Some(ext) = &step.ext_dims {
                    if !t.section.is_empty() {
                        self.verify_unsep_table_areas(ext.section, &t.section, cx)?;
                    }
                    if let Some(ed) = &ext.acc_rg_ed
                        && !t.ext_dims.is_empty()
                    {
                        self.verify_unsep_table_areas(ed.table, &t.ext_dims, cx)?;
                    }
                }
            }
            self.done = true;
            self.ready = true;
            return Ok(());
        }

        if let Some(ext) = &step.ext_dims {
            let mut scan = cx.src.scan_fields(ext.section, &self.section_fields)?;
            loop {
                check_cancel(cx.env)?;
                cx.tick()?;
                let Some(row) = scan.next().transpose()? else {
                    break;
                };
                check_cancel(cx.env)?;
                cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
                match &step.areas {
                    BalanceAreasL::None => {}
                    BalanceAreasL::Exact(t) => {
                        if !exact_area_matches(&row, &t.section)? {
                            cx.mem.release_scratch();
                            continue;
                        }
                    }
                    BalanceAreasL::Unseparated(t) => {
                        self.check_unseparated_area(&row, &t.section, cx)?;
                    }
                }
                let QValue::Ref {
                    table: owner_tbl,
                    id: account_id,
                } = value(&row, ext.owner)?
                else {
                    return Err(fatal("ExtDimensionTypes owner is not a reference"));
                };
                if *owner_tbl != step.inputs.ref_table {
                    return Err(fatal("ExtDimensionTypes owner table mismatch"));
                }
                if !self.accounts.contains_key(account_id) {
                    cx.mem.release_scratch();
                    continue;
                }
                let ord_i64 = parse_line_no(value(&row, ext.ordinal)?, cx)?;
                let ordinal = u8::try_from(ord_i64)
                    .ok()
                    .filter(|&o| o >= 1)
                    .ok_or_else(|| fatal("invalid ExtDimensionTypes ordinal"))?;
                let QValue::Bool(turnover_only) = value(&row, ext.turnover_only)? else {
                    return Err(fatal("ExtDimensionTypes TurnoversOnly is not Boolean"));
                };
                let dim_kind = match ext.dim_kind {
                    Some(dk_field) => {
                        let QValue::Ref { table: kt, id: kid } = value(&row, dk_field)? else {
                            return Err(fatal("ExtDimensionTypes ExtDimensionType is not a Ref"));
                        };
                        if ext.kind_ref_table.is_some_and(|want| *kt != want) {
                            return Err(fatal("ExtDimensionTypes kind table mismatch"));
                        }
                        *kid
                    }
                    None => [0u8; 16],
                };
                let mut flag_bits = 0u64;
                for (fi, &ff) in ext.ext_flag_fields.iter().enumerate() {
                    let QValue::Bool(b) = value(&row, ff)? else {
                        return Err(fatal("ExtDimensionAccountingFlag is not Boolean"));
                    };
                    if *b {
                        flag_bits |= 1u64 << fi;
                    }
                }
                if self.ext_rows.len() == self.ext_rows.capacity() {
                    let next_cap = self.ext_rows.capacity().saturating_mul(2).max(8);
                    let next_bytes =
                        crate::model::size_vec(next_cap, std::mem::size_of::<ExtRow>())?;
                    let next_charge = cx.mem.hold_owned(next_bytes)?;
                    let mut next = Vec::with_capacity(next_cap);
                    next.extend_from_slice(&self.ext_rows);
                    self.ext_rows = next;
                    self.ext_charge = Some(next_charge);
                }
                self.ext_rows.push(ExtRow {
                    account_id: *account_id,
                    ordinal,
                    dim_kind,
                    turnover_only: *turnover_only,
                    flag_bits,
                });
                cx.mem.release_scratch();
            }
            self.ext_rows
                .sort_unstable_by_key(|e| (e.account_id, e.ordinal));
        }

        if self.ed_mode == EdMode::FullScan
            && let Some(ext) = &step.ext_dims
            && let Some(ed) = &ext.acc_rg_ed
        {
            self.load_ed_full_scan(step, ext, ed, cx)?;
        }

        self.ready = true;
        Ok(())
    }

    fn clear_ed_bucket(&mut self) {
        self.ed_bucket.clear();
        self.ed_bucket.shrink_to_fit();
        self.ed_bucket_charge = None;
    }

    fn decode_and_push_ed_row(
        &mut self,
        row: &Row,
        ext: &AccountingBalanceAndTurnoversExtL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        side_filter: Option<u8>,
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        let QValue::Ref {
            table: rec_table,
            id: rec_id,
        } = value(row, ed.ed_recorder)?
        else {
            return Err(fatal("AccRgED Recorder is not Ref"));
        };
        let line_no = parse_line_no(value(row, ed.ed_line)?, cx)?;
        let side = match ed.ed_correspond {
            Some(c) => parse_correspond(value(row, c)?)?,
            None => 0,
        };
        if side_filter.is_some_and(|want| want != side) {
            return Ok(());
        }
        let QValue::Ref {
            table: kind_table,
            id: kind_id,
        } = value(row, ed.ed_kind)?
        else {
            return Err(fatal("AccRgED Kind is not Ref"));
        };
        if *kind_table != ed.kind_ref_table {
            return Err(fatal("AccRgED Kind reference target mismatch"));
        }
        let val = value(row, ed.ed_value)?;
        validate_ext_value(val, &ext.value_check, cx)?;
        let val_charge = cx.mem.hold_owned(crate::model::value_model(val))?;
        if self.ed_bucket.len() == self.ed_bucket.capacity() {
            let next_cap = self.ed_bucket.capacity().saturating_mul(2).max(8);
            let next_bytes = crate::model::size_vec(next_cap, std::mem::size_of::<EdEntry>())?;
            let next_charge = cx.mem.hold_owned(next_bytes)?;
            let mut next = Vec::with_capacity(next_cap);
            next.append(&mut self.ed_bucket);
            self.ed_bucket = next;
            self.ed_bucket_charge = Some(next_charge);
        }
        self.ed_bucket.push(EdEntry {
            key: (*rec_table, *rec_id, line_no, side),
            kind: *kind_id,
            value: val.clone(),
            _val_charge: Some(val_charge),
        });
        Ok(())
    }

    fn finalize_ed_bucket(&mut self) {
        self.ed_bucket.sort_unstable_by_key(|e| (e.key, e.kind));
    }

    fn open_ed_by_period(
        &mut self,
        ed_ix: IndexId,
        step: &AccountingBalanceAndTurnoversL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_opened {
            return Ok(());
        }
        let (exact_ed, unsep_ed) = match &step.areas {
            BalanceAreasL::None => (&[][..], &[][..]),
            BalanceAreasL::Exact(t) => (t.ext_dims.as_slice(), &[][..]),
            BalanceAreasL::Unseparated(t) => (&[][..], t.ext_dims.as_slice()),
        };
        let fields = self.ed_fields.clone();
        let (iter, verified) = self.open_period_range(
            ed.table,
            ed_ix,
            exact_ed,
            unsep_ed,
            &step.areas,
            step.need_opening,
            &fields,
            cx,
        )?;
        self.ed_iter = Some(iter);
        self.ed_areas_verified = verified;
        self.ed_opened = true;
        Ok(())
    }

    fn next_ed_by_period_row(
        &mut self,
        ed_ix: IndexId,
        step: &AccountingBalanceAndTurnoversL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<Option<(BslDate, Row, OwnedCharge)>, ExecError> {
        if let Some(peek) = self.ed_peek.take() {
            return Ok(Some(peek));
        }
        self.open_ed_by_period(ed_ix, step, ed, cx)?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let next = match self.ed_iter.as_mut() {
                Some(iter) => iter.next().transpose()?,
                None => None,
            };
            let Some(row) = next else {
                self.ed_exhausted = true;
                self.ed_iter = None;
                return Ok(None);
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    if !exact_area_matches(&row, &t.ext_dims)? {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    self.check_unseparated_area(&row, &t.ext_dims, cx)?;
                }
            }
            let QValue::Date(period) = value(&row, ed.ed_period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            let period = *period;
            if self.ed_last_period.is_some_and(|prev| period < prev) {
                return Err(fatal("AccRgED ByPeriod index out of order"));
            }
            self.ed_last_period = Some(period);
            let charge = cx.mem.hold_owned(crate::model::row_cap_model(&row))?;
            cx.mem.release_scratch();
            return Ok(Some((period, row, charge)));
        }
    }

    fn advance_ed_bucket(
        &mut self,
        ed_ix: IndexId,
        target_period: BslDate,
        step: &AccountingBalanceAndTurnoversL,
        ext: &AccountingBalanceAndTurnoversExtL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_bucket_period == Some(target_period) {
            return Ok(());
        }
        self.clear_ed_bucket();
        self.ed_bucket_period = Some(target_period);
        while let Some((ed_period, row, charge)) =
            self.next_ed_by_period_row(ed_ix, step, ed, cx)?
        {
            if ed_period < target_period {
                drop(charge);
                continue;
            }
            if ed_period > target_period {
                self.ed_peek = Some((ed_period, row, charge));
                break;
            }
            self.decode_and_push_ed_row(&row, ext, ed, None, cx)?;
            drop(charge);
        }
        self.finalize_ed_bucket();
        Ok(())
    }

    fn drain_unseparated_ed(
        &mut self,
        ed_ix: IndexId,
        step: &AccountingBalanceAndTurnoversL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if !matches!(step.areas, BalanceAreasL::Unseparated(_)) {
            return Ok(());
        }
        if !self.ed_opened {
            self.open_ed_by_period(ed_ix, step, ed, cx)?;
        }
        if self.ed_areas_verified {
            return Ok(());
        }
        while let Some((_, _, charge)) = self.next_ed_by_period_row(ed_ix, step, ed, cx)? {
            drop(charge);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn load_ed_by_recorder(
        &mut self,
        ed_ix: IndexId,
        has_correspond: bool,
        rec_table: u32,
        rec_id: [u8; 16],
        line_no: i64,
        side: u8,
        step: &AccountingBalanceAndTurnoversL,
        ext: &AccountingBalanceAndTurnoversExtL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        self.clear_ed_bucket();
        let info = cx
            .src
            .map()
            .try_table(ed.table)
            .ok_or_else(|| fatal("missing AccRgED descriptor"))?;
        let mut key = Vec::with_capacity(info.separators.len() + 3);
        if let BalanceAreasL::Exact(t) = &step.areas {
            for sep in &info.separators {
                let entry = t
                    .ext_dims
                    .iter()
                    .find(|e| e.field == sep.field)
                    .ok_or_else(|| fatal("missing exact AccRgED separator"))?;
                key.push(entry.value.clone());
            }
        }
        key.push(QValue::Ref {
            table: rec_table,
            id: rec_id,
        });
        key.push(QValue::Number(Number::from(line_no)));
        if has_correspond {
            key.push(QValue::Number(Number::from(i64::from(side))));
        }
        let mut iter = cx.src.range_fields(
            ed.table,
            ed_ix,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            false,
            &self.ed_fields,
        )?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = iter.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            if let BalanceAreasL::Exact(t) = &step.areas
                && !exact_area_matches(&row, &t.ext_dims)?
            {
                cx.mem.release_scratch();
                continue;
            }
            self.decode_and_push_ed_row(&row, ext, ed, Some(side), cx)?;
            cx.mem.release_scratch();
        }
        drop(iter);
        self.finalize_ed_bucket();
        Ok(())
    }

    fn load_ed_full_scan(
        &mut self,
        step: &AccountingBalanceAndTurnoversL,
        ext: &AccountingBalanceAndTurnoversExtL,
        ed: &AccountingBalanceAndTurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_loaded_full {
            return Ok(());
        }
        check_cancel(cx.env)?;
        let mut scan = cx.src.scan_fields(ed.table, &self.ed_fields)?;
        let (lo_date, hi_date) = self.period_scan_bounds(step.need_opening);
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            match &step.areas {
                BalanceAreasL::None => {}
                BalanceAreasL::Exact(t) => {
                    if !exact_area_matches(&row, &t.ext_dims)? {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                BalanceAreasL::Unseparated(t) => {
                    self.check_unseparated_area(&row, &t.ext_dims, cx)?;
                }
            }
            let QValue::Date(row_period) = value(&row, ed.ed_period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            if lo_date.is_some_and(|lo| *row_period < lo)
                || hi_date.is_some_and(|(hi, incl)| {
                    if incl {
                        *row_period > hi
                    } else {
                        *row_period >= hi
                    }
                })
            {
                cx.mem.release_scratch();
                continue;
            }
            self.decode_and_push_ed_row(&row, ext, ed, None, cx)?;
            cx.mem.release_scratch();
        }
        drop(scan);
        self.finalize_ed_bucket();
        self.ed_loaded_full = true;
        Ok(())
    }

    fn open_main_iter(
        &mut self,
        step: &AccountingBalanceAndTurnoversL,
        cx: &Cx<'a>,
    ) -> Result<(), ExecError> {
        let (exact_main, unsep_main) = match &step.areas {
            BalanceAreasL::None => (&[][..], &[][..]),
            BalanceAreasL::Exact(t) => (t.main.as_slice(), &[][..]),
            BalanceAreasL::Unseparated(t) => (&[][..], t.main.as_slice()),
        };
        let fields = self.fields.clone();
        match self.ed_mode {
            EdMode::ByPeriod { main_ix, .. } => {
                let (iter, _) = self.open_period_range(
                    step.inputs.main,
                    main_ix,
                    exact_main,
                    unsep_main,
                    &step.areas,
                    step.need_opening,
                    &fields,
                    cx,
                )?;
                self.iter = Some(iter);
            }
            EdMode::Inline | EdMode::ByRecorder { .. } | EdMode::FullScan => {
                let (lo_date, hi_date) = self.period_scan_bounds(step.need_opening);
                let want_range = lo_date.is_some() || hi_date.is_some() || !exact_main.is_empty();
                let main_ix =
                    if want_range && let Some(info) = cx.src.map().try_table(step.inputs.main) {
                        find_by_period_index(info, step.period)
                    } else {
                        None
                    };
                if let Some(main_ix) = main_ix {
                    let (iter, _) = self.open_period_range(
                        step.inputs.main,
                        main_ix,
                        exact_main,
                        unsep_main,
                        &step.areas,
                        step.need_opening,
                        &fields,
                        cx,
                    )?;
                    self.iter = Some(iter);
                } else {
                    self.iter = Some(cx.src.scan_fields(step.inputs.main, &fields)?);
                }
            }
        }
        Ok(())
    }

    fn lookup_ext_row(
        &self,
        ext: &AccountingBalanceAndTurnoversExtL,
        account_id: [u8; 16],
        slot_ordinal: u8,
    ) -> Option<ExtRow> {
        let start = self.ext_rows.partition_point(|e| e.account_id < account_id);
        let slice = self.ext_rows.get(start..)?;
        if let Some(req_kinds) = &ext.requested_kinds {
            let wanted = req_kinds.get(usize::from(slot_ordinal.checked_sub(1)?))?;
            for entry in slice {
                if entry.account_id != account_id {
                    break;
                }
                if entry.dim_kind == *wanted {
                    return Some(*entry);
                }
            }
            None
        } else {
            for entry in slice {
                if entry.account_id != account_id {
                    break;
                }
                if entry.ordinal == slot_ordinal {
                    return Some(*entry);
                }
            }
            None
        }
    }

    fn lookup_ed_value(&self, key: EdKey, kind: [u8; 16]) -> QValue {
        match self
            .ed_bucket
            .binary_search_by_key(&(key, kind), |e| (e.key, e.kind))
        {
            Ok(pos) => self
                .ed_bucket
                .get(pos)
                .map_or(QValue::Null, |e| e.value.clone()),
            Err(_) => QValue::Null,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_side(
        &mut self,
        row: &Row,
        period: BslDate,
        rec_val: &QValue,
        line_val: &QValue,
        ed_key: Option<EdKey>,
        side: usize,
        account_id: [u8; 16],
        kind: u8,
        chart_signal: Option<bool>,
        is_opening: bool,
        step: &AccountingBalanceAndTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let nr = step.resources.len();
        let raw_dims = 2 + step.dims.len() + step.ext_count();
        let base_width = raw::width(raw_dims, nr);
        let has_chart_signal = step.chart_layout.is_some_and(|l| l.signal.is_some());
        let total_width = base_width + usize::from(has_chart_signal) + step.policy_fields.len();

        let mut slot_rows: Vec<Option<ExtRow>> = Vec::new();
        let mut slot_vals: Vec<QValue> = Vec::new();
        if let Some(ext) = &step.ext_dims {
            slot_rows.reserve(ext.slots.len());
            slot_vals.reserve(ext.slots.len());
            for slot in &ext.slots {
                let info = self.lookup_ext_row(ext, account_id, slot.ordinal);
                slot_rows.push(info);
                let val = if let Some(ext_row) = info {
                    if let Some(ed) = &ext.acc_rg_ed {
                        let key = ed_key.ok_or_else(|| fatal("missing AccRgED lookup key"))?;
                        let lookup_key = if ed.ed_correspond.is_some() {
                            key
                        } else {
                            (key.0, key.1, key.2, 0)
                        };
                        self.lookup_ed_value(lookup_key, ext_row.dim_kind)
                    } else if ext.requested_kinds.is_some() {
                        let phys_idx = usize::from(ext_row.ordinal.saturating_sub(1));
                        let pair = ext
                            .physical_values
                            .get(phys_idx)
                            .ok_or_else(|| fatal("physical subconto ordinal out of range"))?;
                        let f = pair
                            .get(side)
                            .copied()
                            .ok_or_else(|| fatal("subconto side offset"))?;
                        let v = value(row, f)?;
                        validate_ext_value(v, &ext.value_check, cx)?;
                        v.clone()
                    } else {
                        let pair = slot
                            .values
                            .ok_or_else(|| fatal("missing inline subconto fields"))?;
                        let f = pair
                            .get(side)
                            .copied()
                            .ok_or_else(|| fatal("subconto side offset"))?;
                        let v = value(row, f)?;
                        validate_ext_value(v, &ext.value_check, cx)?;
                        v.clone()
                    }
                } else {
                    QValue::Null
                };
                slot_vals.push(val);
            }
        }

        let mut bal_masks = vec![0u64; nr];
        let mut turn_masks = vec![0u64; nr];
        if step.ext_dims.is_some() {
            for (r, res) in step.resources.iter().enumerate() {
                let mut bm = 0u64;
                let mut tm = 0u64;
                for (s, info) in slot_rows.iter().enumerate() {
                    let Some(ext_row) = info else { continue };
                    let flag_ok = match res.ext_flag {
                        None => true,
                        Some(fi) => ((ext_row.flag_bits >> fi) & 1) != 0,
                    };
                    if flag_ok {
                        tm |= 1u64 << s;
                        if !ext_row.turnover_only {
                            bm |= 1u64 << s;
                        }
                    }
                }
                if let Some(slot) = bal_masks.get_mut(r) {
                    *slot = bm;
                }
                if let Some(slot) = turn_masks.get_mut(r) {
                    *slot = tm;
                }
            }
        }

        let mut distinct_masks: Vec<u64> = Vec::with_capacity(2);
        if is_opening {
            for &bm in &bal_masks {
                if !distinct_masks.contains(&bm) {
                    distinct_masks.push(bm);
                }
            }
        } else {
            if step.need_opening {
                for &bm in &bal_masks {
                    if !distinct_masks.contains(&bm) {
                        distinct_masks.push(bm);
                    }
                }
            }
            if step.need_turnovers {
                for &tm in &turn_masks {
                    if !distinct_masks.contains(&tm) {
                        distinct_masks.push(tm);
                    }
                }
            }
            if distinct_masks.is_empty() {
                distinct_masks.push(0);
            }
        }

        for mask in distinct_masks {
            let mut out = Vec::with_capacity(total_width);
            if is_opening && step.periodic {
                out.push(QValue::Null);
                out.push(QValue::Null);
                out.push(QValue::Null);
            } else {
                out.push(QValue::Date(period));
                out.push(if is_opening {
                    QValue::Null
                } else {
                    rec_val.clone()
                });
                out.push(if is_opening {
                    QValue::Null
                } else {
                    line_val.clone()
                });
            }
            out.push(QValue::Ref {
                table: step.inputs.ref_table,
                id: account_id,
            });
            out.push(QValue::Number(Number::from(i64::from(kind))));
            for dim in &step.dims {
                let f = dim
                    .main
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("dimension side offset"))?;
                let v = value(row, f)?;
                if matches!(v, QValue::Null) && !dim.nullable {
                    return Err(fatal("nonnullable dimension is NULL"));
                }
                out.push(v.clone());
            }
            for (s, val) in slot_vals.iter().enumerate() {
                if ((mask >> s) & 1) != 0 {
                    out.push(val.clone());
                } else {
                    out.push(QValue::Null);
                }
            }
            for (r, res) in step.resources.iter().enumerate() {
                let bm = bal_masks.get(r).copied().unwrap_or(0);
                let tm = turn_masks.get(r).copied().unwrap_or(0);
                let val_opt = stored_number(row, res, side)?;
                if let Some(n) = val_opt {
                    crate::num::guard(cx.env, n)?;
                }
                let mut comps = [
                    Number::zero(),
                    Number::zero(),
                    Number::zero(),
                    Number::zero(),
                    Number::zero(),
                ];
                if let Some(n) = val_opt {
                    if is_opening {
                        if bm == mask {
                            let signed = if side == 0 { n.clone() } else { n.neg() };
                            comps[raw::BALANCE] = signed.clone();
                            if !step.periodic {
                                comps[raw::CLOSING] = signed;
                            }
                        }
                    } else {
                        let signed = if side == 0 { n.clone() } else { n.neg() };
                        if step.need_opening && bm == mask {
                            comps[raw::CLOSING] = signed.clone();
                        }
                        if (step.need_turnovers || bm == tm) && tm == mask {
                            if side == 0 {
                                comps[raw::RECEIPT] = n.clone();
                            } else {
                                comps[raw::EXPENSE] = n.clone();
                            }
                            comps[raw::TURNOVER] = signed;
                        }
                    }
                }
                for c in comps {
                    out.push(QValue::Number(c));
                }
            }
            if has_chart_signal {
                out.push(QValue::Bool(chart_signal.unwrap_or(true)));
            }
            for &pf in &step.policy_fields {
                out.push(value(row, pf)?.clone());
            }
            let charge = cx.mem.hold_owned(crate::model::row_cap_model(&out))?;
            self.emit_queue.push_back((out, charge));
        }
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        step: &AccountingBalanceAndTurnoversL,
        child: &mut Option<BoxOp<'a>>,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        if let Some((row, charge)) = self.emit_queue.pop_front() {
            drop(charge);
            return Ok(Some(row));
        }
        if self.done {
            return Ok(None);
        }
        if !self.ready {
            self.load(step, child, cx)?;
            if self.done {
                return Ok(None);
            }
        }
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            if let Some((row, charge)) = self.emit_queue.pop_front() {
                drop(charge);
                return Ok(Some(row));
            }
            if self.iter.is_none() {
                self.open_main_iter(step, cx)?;
            }
            let Some(iter) = self.iter.as_mut() else {
                return Err(fatal("missing main iterator"));
            };
            let Some(row) = iter.next().transpose()? else {
                if let (EdMode::ByPeriod { ed_ix, .. }, Some(ext)) = (self.ed_mode, &step.ext_dims)
                    && let Some(ed) = &ext.acc_rg_ed
                {
                    self.drain_unseparated_ed(ed_ix, step, ed, cx)?;
                }
                self.close(cx);
                return Ok(None);
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
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
                _ => return Err(fatal("Active is not Boolean")),
            }
            let QValue::Date(period) = value(&row, step.period)? else {
                return Err(fatal("Period is not Date"));
            };
            let period = *period;
            if matches!(self.ed_mode, EdMode::ByPeriod { .. }) {
                if self.main_last_period.is_some_and(|prev| period < prev) {
                    return Err(fatal("main ByPeriod out of order"));
                }
                self.main_last_period = Some(period);
            }

            let acc_rg_ed = step.ext_dims.as_ref().and_then(|e| e.acc_rg_ed.as_deref());
            let (rec_val, rec_pair) = if let Some(rec) = &step.recorder {
                let v = value(&row, rec.field)?;
                let QValue::Ref { table, id } = v else {
                    return Err(fatal("Recorder is not Ref"));
                };
                if rec.reference_tags.binary_search(table).is_err() {
                    return Err(fatal("Recorder reference target mismatch"));
                }
                (v.clone(), (*table, *id))
            } else if let Some(ed) = acc_rg_ed {
                let v = value(&row, ed.main_recorder)?;
                let QValue::Ref { table, id } = v else {
                    return Err(fatal("Recorder is not Ref"));
                };
                (v.clone(), (*table, *id))
            } else {
                (QValue::Null, (0, [0u8; 16]))
            };

            if !before_end(period, rec_pair, self.end) {
                cx.mem.release_scratch();
                continue;
            }
            let is_opening = self.start != Bnd::None && !after_start(period, rec_pair, self.start);
            if is_opening && !step.need_opening {
                cx.mem.release_scratch();
                continue;
            }

            let (line_val, line_no_opt) = if let Some(lf) = step.line {
                let v = value(&row, lf)?;
                let ln = parse_line_no(v, cx)?;
                (v.clone(), Some(ln))
            } else if let Some(ed) = acc_rg_ed {
                let v = value(&row, ed.main_line)?;
                let ln = parse_line_no(v, cx)?;
                (QValue::Null, Some(ln))
            } else {
                (QValue::Null, None)
            };

            let sides: [Option<usize>; 2] = if step.correspondence {
                [Some(0), Some(1)]
            } else {
                let rk_field = step
                    .record_kind
                    .ok_or_else(|| fatal("missing RecordKind field"))?;
                let s = match value(&row, rk_field)? {
                    QValue::Number(n) if *n == Number::from(0) => 0,
                    QValue::Number(n) if *n == Number::from(1) => 1,
                    QValue::SysEnum(_, i) if *i == 0 => 0,
                    QValue::SysEnum(_, i) if *i == 1 => 1,
                    _ => return Err(fatal("invalid RecordKind value")),
                };
                [Some(s), None]
            };

            type EligibleSide = (usize, [u8; 16], u8, Option<bool>);
            let mut eligible: [Option<EligibleSide>; 2] = [None, None];
            for (slot, side_opt) in eligible.iter_mut().zip(sides) {
                let Some(side) = side_opt else { continue };
                let field = step
                    .inputs
                    .accounts
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("account side offset"))?;
                *slot = match value(&row, field)? {
                    QValue::Null => None,
                    QValue::Ref { table, id } if *table == step.inputs.ref_table => {
                        if *id == [0u8; 16] {
                            None
                        } else {
                            self.accounts
                                .get(id)
                                .map(|&(kind, sig)| (side, *id, kind, sig))
                        }
                    }
                    _ => return Err(fatal("account reference target mismatch")),
                };
            }
            if eligible.iter().all(Option::is_none) {
                cx.mem.release_scratch();
                continue;
            }

            if let (Some(ext), Some(ed)) = (&step.ext_dims, acc_rg_ed)
                && let EdMode::ByPeriod { ed_ix, .. } = self.ed_mode
            {
                self.advance_ed_bucket(ed_ix, period, step, ext, ed, cx)?;
            }

            for (side, account_id, kind, sig) in eligible.into_iter().flatten() {
                let ed_key = if let (Some(ext), Some(ed)) = (&step.ext_dims, acc_rg_ed) {
                    let ln = line_no_opt.ok_or_else(|| fatal("missing LineNo for AccRgED"))?;
                    let side_u8 = if ed.ed_correspond.is_some() {
                        side as u8
                    } else {
                        0
                    };
                    if let EdMode::ByRecorder {
                        ed_ix,
                        has_correspond,
                    } = self.ed_mode
                    {
                        self.load_ed_by_recorder(
                            ed_ix,
                            has_correspond,
                            rec_pair.0,
                            rec_pair.1,
                            ln,
                            side_u8,
                            step,
                            ext,
                            ed,
                            cx,
                        )?;
                    }
                    Some((rec_pair.0, rec_pair.1, ln, side_u8))
                } else {
                    None
                };
                self.emit_side(
                    &row, period, &rec_val, &line_val, ed_key, side, account_id, kind, sig,
                    is_opening, step, cx,
                )?;
            }
            cx.mem.release_scratch();
            if let Some((emitted, charge)) = self.emit_queue.pop_front() {
                drop(charge);
                return Ok(Some(emitted));
            }
        }
    }

    pub(super) fn close(&mut self, _cx: &mut Cx<'a>) {
        self.done = true;
        self.iter = None;
        self.ed_iter = None;
        self.ed_peek = None;
        self.emit_queue.clear();
        self.ed_bucket.clear();
        self.ed_bucket_charge = None;
        self.ext_rows.clear();
        self.ext_charge = None;
        self.accounts.clear();
        self.accounts_charge = None;
        self.observed_areas.clear();
        self.observed_charge = None;
        self.fields.clear();
        self.chart_fields.clear();
        self.section_fields.clear();
        self.ed_fields.clear();
        self.fields_charge = None;
    }
}
