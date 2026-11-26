use super::{Cx, OwnedCharge, accounting::check_cancel, row_cap_bytes};
use crate::source::{Row, RowIter};
use bsl_prim::{BslDate, Number};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use query_ir::ir::raw_layout as raw;
use query_ir::lower::{
    AccountingTurnoversL, AccountingTurnoversResourceL, TurnoversAccRgEdL, TurnoversAreasL,
    TurnoversValueCheckL,
};
use query_ir::{DateQ, ExecError, NumQ, QValue};
use std::ops::Bound;

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

fn validate_typed_value(
    val: &QValue,
    check: &TurnoversValueCheckL,
    cx: &Cx<'_>,
    label: &str,
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
        Err(fatal(format!(
            "stored {label} value contradicts semantic qualifier"
        )))
    }
}

fn validate_ext_value(
    val: &QValue,
    check: &TurnoversValueCheckL,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    validate_typed_value(val, check, cx, "ExtDimension")
}

fn areas_refused() -> ExecError {
    ExecError::Later {
        what: "Q6b-TurnoversAreas",
        stage: "Q7c",
    }
}

fn matches_exact_area(filters: &[(FieldId, QValue)], row: &Row) -> Result<bool, ExecError> {
    for (field, want) in filters {
        if value(row, *field)? != want {
            return Ok(false);
        }
    }
    Ok(true)
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

fn exact_sep_prefix(
    table: TableId,
    exact: Option<&[(FieldId, QValue)]>,
    map: &StorageMap,
) -> Result<Option<Vec<QValue>>, ExecError> {
    let info = map
        .try_table(table)
        .ok_or_else(|| fatal("missing table descriptor"))?;
    if info.separators.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let Some(filters) = exact else {
        return Ok(None);
    };
    let mut prefix = Vec::with_capacity(info.separators.len());
    for sep in &info.separators {
        let Some((_, val)) = filters.iter().find(|(f, _)| *f == sep.field) else {
            return Ok(None);
        };
        prefix.push(val.clone());
    }
    Ok(Some(prefix))
}

type PeriodRangeBounds = (Option<Vec<QValue>>, Option<Vec<QValue>>);

fn period_range_bounds(
    table: TableId,
    exact: Option<&[(FieldId, QValue)]>,
    dates: [Option<BslDate>; 2],
    map: &StorageMap,
) -> Result<PeriodRangeBounds, ExecError> {
    let Some(prefix) = exact_sep_prefix(table, exact, map)? else {
        return Ok((None, None));
    };
    let lo = match dates[0] {
        Some(start) => {
            let mut key = prefix.clone();
            key.push(QValue::Date(start));
            Some(key)
        }
        None if !prefix.is_empty() => Some(prefix.clone()),
        None => None,
    };
    let hi = match dates[1] {
        Some(end) => {
            let mut key = prefix;
            key.push(QValue::Date(end));
            Some(key)
        }
        None if !prefix.is_empty() => Some(prefix),
        None => None,
    };
    Ok((lo, hi))
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

fn check_unsep_value(
    slot: u16,
    val: &QValue,
    seen_areas: &mut [Option<QValue>],
    area_value_charges: &mut Vec<OwnedCharge>,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    let idx = usize::from(slot);
    let entry = seen_areas
        .get_mut(idx)
        .ok_or_else(|| fatal("area slot out of bounds"))?;
    match entry {
        Some(prev) => {
            if prev != val {
                return Err(areas_refused());
            }
        }
        None => {
            let charge = cx.mem.hold_owned(crate::model::value_model(val))?;
            *entry = Some(val.clone());
            area_value_charges.push(charge);
        }
    }
    Ok(())
}

fn validate_unsep_tuple(
    info: &TableInfo,
    seps: &[(u16, FieldId)],
    tuple: &[QValue],
    seen_areas: &mut [Option<QValue>],
    area_value_charges: &mut Vec<OwnedCharge>,
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    for (sep_idx, sep) in info.separators.iter().enumerate() {
        let Some((slot, _)) = seps.iter().find(|(_, f)| *f == sep.field) else {
            return Err(fatal("missing unseparated slot mapping"));
        };
        let val = tuple
            .get(sep_idx)
            .ok_or_else(|| fatal("data_areas tuple shorter than separators"))?;
        check_unsep_value(*slot, val, seen_areas, area_value_charges, cx)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn open_period_range<'a>(
    table: TableId,
    index: IndexId,
    is_ed: bool,
    step: &AccountingTurnoversL,
    fields: &[FieldId],
    seen_areas: &mut [Option<QValue>],
    area_value_charges: &mut Vec<OwnedCharge>,
    cx: &Cx<'a>,
) -> Result<(RowIter<'a>, bool), ExecError> {
    let info = cx
        .src
        .map()
        .try_table(table)
        .ok_or_else(|| fatal("missing table descriptor"))?;
    if info.separators.is_empty() {
        let (lo_vec, hi_vec) = period_range_bounds(table, None, step.dates, cx.src.map())?;
        let lo = match &lo_vec {
            Some(v) => Bound::Included(v.as_slice()),
            None => Bound::Unbounded,
        };
        let hi = match &hi_vec {
            Some(v) => Bound::Included(v.as_slice()),
            None => Bound::Unbounded,
        };
        return Ok((
            cx.src.range_fields(table, index, lo, hi, false, fields)?,
            false,
        ));
    }
    match &step.areas {
        Some(TurnoversAreasL::Exact { main, ed, .. }) => {
            let exact = if is_ed {
                ed.as_slice()
            } else {
                main.as_slice()
            };
            let (lo_vec, hi_vec) =
                period_range_bounds(table, Some(exact), step.dates, cx.src.map())?;
            let lo = match &lo_vec {
                Some(v) => Bound::Included(v.as_slice()),
                None => Bound::Unbounded,
            };
            let hi = match &hi_vec {
                Some(v) => Bound::Included(v.as_slice()),
                None => Bound::Unbounded,
            };
            Ok((
                cx.src.range_fields(table, index, lo, hi, false, fields)?,
                false,
            ))
        }
        Some(TurnoversAreasL::Unseparated { main, ed, .. }) => {
            let unsep = if is_ed {
                ed.as_slice()
            } else {
                main.as_slice()
            };
            let sep_len = info.separators.len();
            let has_date_bound = step.dates[0].is_some() || step.dates[1].is_some();
            let can_skip = has_date_bound
                && sep_len > 0
                && unsep.len() == sep_len
                && index_has_sep_prefix(info, index)
                && info
                    .separators
                    .iter()
                    .all(|sep| unsep.iter().any(|(_, f)| *f == sep.field));
            if can_skip
                && let Some(areas) = super::access::data_areas(cx, table, index, sep_len, false)?
            {
                if areas.is_empty() {
                    return Ok((Box::new(std::iter::empty()), true));
                }
                for area in &areas {
                    validate_unsep_tuple(info, unsep, area, seen_areas, area_value_charges, cx)?;
                }
                let Some(prefix) = areas.front() else {
                    return Ok((Box::new(std::iter::empty()), true));
                };
                let mut lo_vec = prefix.clone();
                if let Some(start) = step.dates[0] {
                    lo_vec.push(QValue::Date(start));
                }
                let mut hi_vec = prefix.clone();
                if let Some(end) = step.dates[1] {
                    hi_vec.push(QValue::Date(end));
                }
                return Ok((
                    cx.src.range_fields(
                        table,
                        index,
                        Bound::Included(lo_vec.as_slice()),
                        Bound::Included(hi_vec.as_slice()),
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
        None => Ok((
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

fn push_charged_id(
    vec: &mut Vec<[u8; 16]>,
    charge: &mut Option<OwnedCharge>,
    id: [u8; 16],
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    if vec.len() == vec.capacity() {
        let next_cap = vec
            .capacity()
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(crate::model::invalid_size)?;
        let next_bytes = crate::model::size_vec(next_cap, std::mem::size_of::<[u8; 16]>())?;
        let next_charge = cx.mem.hold_owned(next_bytes)?;
        let mut next = Vec::with_capacity(next_cap);
        next.extend_from_slice(vec);
        *vec = next;
        *charge = Some(next_charge);
    }
    vec.push(id);
    Ok(())
}

fn push_charged_edge(
    vec: &mut Vec<([u8; 16], [u8; 16])>,
    charge: &mut Option<OwnedCharge>,
    edge: ([u8; 16], [u8; 16]),
    cx: &Cx<'_>,
) -> Result<(), ExecError> {
    if vec.len() == vec.capacity() {
        let next_cap = vec
            .capacity()
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(crate::model::invalid_size)?;
        let next_bytes =
            crate::model::size_vec(next_cap, std::mem::size_of::<([u8; 16], [u8; 16])>())?;
        let next_charge = cx.mem.hold_owned(next_bytes)?;
        let mut next = Vec::with_capacity(next_cap);
        next.extend_from_slice(vec);
        *vec = next;
        *charge = Some(next_charge);
    }
    vec.push(edge);
    Ok(())
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
    fields: Vec<FieldId>,
    fields_charge: Option<OwnedCharge>,
    ext_fields: Vec<FieldId>,
    ext_fields_charge: Option<OwnedCharge>,
    ed_fields: Vec<FieldId>,
    ed_fields_charge: Option<OwnedCharge>,
    seen_areas: Vec<Option<QValue>>,
    seen_areas_charge: Option<OwnedCharge>,
    area_value_charges: Vec<OwnedCharge>,
    ext_present: Vec<([u8; 16], u8, u64)>,
    ext_charge: Option<OwnedCharge>,
    ext_kinds: Vec<([u8; 16], u8, [u8; 16], u64)>,
    ext_kinds_charge: Option<OwnedCharge>,
    needed_kinds: Vec<[u8; 16]>,
    needed_kinds_charge: Option<OwnedCharge>,
    ext_loaded: bool,
    allowed_accounts: Option<Vec<[u8; 16]>>,
    allowed_accounts_charge: Option<OwnedCharge>,
    allowed_loaded: bool,
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
    iter: Option<RowIter<'a>>,
    pending: Option<(Row, OwnedCharge)>,
    side: usize,
    mask_step: usize,
    done: bool,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(
        step: &AccountingTurnoversL,
        initial_areas: Option<Vec<Option<QValue>>>,
        cx: &Cx<'_>,
    ) -> Result<Self, ExecError> {
        let acc_rg_ed = step
            .ext_dims
            .as_ref()
            .and_then(|ext| ext.acc_rg_ed.as_deref());
        if let Some(ext) = &step.ext_dims {
            let expected_inline = acc_rg_ed.is_none();
            if ext
                .slots
                .iter()
                .any(|slot| slot.values.is_some() != expected_inline)
            {
                return Err(fatal("inconsistent ExtDimension storage mode"));
            }
        }
        let ed_mode = match acc_rg_ed {
            None => EdMode::Inline,
            Some(ed) => match (ed.main_by_period, ed.ed_by_period) {
                (Some(main_ix), Some(ed_ix)) => EdMode::ByPeriod { main_ix, ed_ix },
                _ => {
                    let unseparated_ed = matches!(
                        &step.areas,
                        Some(TurnoversAreasL::Unseparated { ed: seps, .. }) if !seps.is_empty()
                    );
                    if !unseparated_ed && let Some(ed_ix) = ed.ed_by_recorder {
                        EdMode::ByRecorder {
                            ed_ix,
                            has_correspond: ed.ed_by_recorder_has_correspond,
                        }
                    } else {
                        EdMode::FullScan
                    }
                }
            },
        };

        let inline_ext_slots = if acc_rg_ed.is_none() {
            step.ext_dims.as_ref().map_or(0, |ext| ext.slots.len())
        } else {
            0
        };
        let area_main_count = match &step.areas {
            Some(TurnoversAreasL::Exact { main, .. }) => main.len(),
            Some(TurnoversAreasL::Unseparated { main, .. }) => main.len(),
            None => 0,
        };
        let ed_main_keys = if acc_rg_ed.is_some() { 3 } else { 0 };
        let capacity = step
            .resources
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(3))
            .and_then(|count| count.checked_add(usize::from(step.period.is_some())))
            .and_then(|count| count.checked_add(usize::from(step.recorder.is_some())))
            .and_then(|count| count.checked_add(usize::from(step.line.is_some())))
            .and_then(|count| count.checked_add(ed_main_keys))
            .and_then(|count| {
                step.dims
                    .len()
                    .checked_mul(2)
                    .and_then(|n| count.checked_add(n))
            })
            .and_then(|count| {
                inline_ext_slots
                    .checked_mul(2)
                    .and_then(|n| count.checked_add(n))
            })
            .and_then(|count| count.checked_add(area_main_count))
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
        for opt in [
            step.period,
            step.recorder.as_ref().map(|r| r.field),
            step.line,
        ]
        .into_iter()
        .flatten()
        {
            if !fields.contains(&opt) {
                fields.push(opt);
            }
        }
        if let Some(ed) = acc_rg_ed {
            for f in [ed.main_period, ed.main_recorder, ed.main_line] {
                if !fields.contains(&f) {
                    fields.push(f);
                }
            }
        }
        fields.extend(step.inputs.accounts);
        for dim in &step.dims {
            for field in dim.main {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        if let Some(ext) = &step.ext_dims {
            for slot in &ext.slots {
                if let Some(values) = slot.values {
                    for field in values {
                        if !fields.contains(&field) {
                            fields.push(field);
                        }
                    }
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
        match &step.areas {
            Some(TurnoversAreasL::Exact { main, .. }) => {
                for (field, _) in main {
                    if !fields.contains(field) {
                        fields.push(*field);
                    }
                }
            }
            Some(TurnoversAreasL::Unseparated { main, .. }) => {
                for (_, field) in main {
                    if !fields.contains(field) {
                        fields.push(*field);
                    }
                }
            }
            None => {}
        }
        fields.extend(&step.policy_fields);

        let area_ext_count = match (&step.ext_dims, &step.areas) {
            (Some(_), Some(TurnoversAreasL::Exact { ext, .. })) => ext.len(),
            (Some(_), Some(TurnoversAreasL::Unseparated { ext, .. })) => ext.len(),
            _ => 0,
        };
        let (ext_fields, ext_fields_charge) = if let Some(ext) = &step.ext_dims
            && (area_ext_count > 0 || acc_rg_ed.is_some() || !ext.ext_flag_fields.is_empty())
        {
            let ext_cap = area_ext_count
                .checked_add(2 + usize::from(acc_rg_ed.is_some()))
                .and_then(|n| n.checked_add(ext.ext_flag_fields.len()))
                .ok_or_else(|| fatal("ext field count overflow"))?;
            let ext_bytes = ext_cap
                .checked_mul(std::mem::size_of::<FieldId>())
                .ok_or_else(|| fatal("ext field allocation overflow"))?;
            let charge = cx.mem.hold_owned(crate::model::block(
                u64::try_from(ext_bytes).map_err(|_| fatal("ext field allocation overflow"))?,
            ))?;
            let mut list = Vec::with_capacity(ext_cap);
            list.push(ext.owner);
            if !list.contains(&ext.ordinal) {
                list.push(ext.ordinal);
            }
            if let Some(ed) = acc_rg_ed
                && !list.contains(&ed.section_dim_kind)
            {
                list.push(ed.section_dim_kind);
            }
            for &flag_field in &ext.ext_flag_fields {
                if !list.contains(&flag_field) {
                    list.push(flag_field);
                }
            }
            match &step.areas {
                Some(TurnoversAreasL::Exact { ext: seps, .. }) => {
                    for (field, _) in seps {
                        if !list.contains(field) {
                            list.push(*field);
                        }
                    }
                }
                Some(TurnoversAreasL::Unseparated { ext: seps, .. }) => {
                    for (_, field) in seps {
                        if !list.contains(field) {
                            list.push(*field);
                        }
                    }
                }
                None => {}
            }
            (list, Some(charge))
        } else {
            (Vec::new(), None)
        };

        let (ed_fields, ed_fields_charge) = if let Some(ed) = acc_rg_ed {
            let area_ed_count = match &step.areas {
                Some(TurnoversAreasL::Exact { ed: seps, .. }) => seps.len(),
                Some(TurnoversAreasL::Unseparated { ed: seps, .. }) => seps.len(),
                None => 0,
            };
            let ed_cap = area_ed_count
                .checked_add(6)
                .ok_or_else(|| fatal("AccRgED field count overflow"))?;
            let ed_bytes = ed_cap
                .checked_mul(std::mem::size_of::<FieldId>())
                .ok_or_else(|| fatal("AccRgED field allocation overflow"))?;
            let charge = cx.mem.hold_owned(crate::model::block(
                u64::try_from(ed_bytes).map_err(|_| fatal("AccRgED field allocation overflow"))?,
            ))?;
            let mut list = Vec::with_capacity(ed_cap);
            for field in [
                ed.ed_period,
                ed.ed_recorder,
                ed.ed_line,
                ed.ed_correspond,
                ed.ed_kind,
                ed.ed_value,
            ] {
                if !list.contains(&field) {
                    list.push(field);
                }
            }
            match &step.areas {
                Some(TurnoversAreasL::Exact { ed: seps, .. }) => {
                    for (field, _) in seps {
                        if !list.contains(field) {
                            list.push(*field);
                        }
                    }
                }
                Some(TurnoversAreasL::Unseparated { ed: seps, .. }) => {
                    for (_, field) in seps {
                        if !list.contains(field) {
                            list.push(*field);
                        }
                    }
                }
                None => {}
            }
            (list, Some(charge))
        } else {
            (Vec::new(), None)
        };

        let (seen_areas, seen_areas_charge, area_value_charges) = match (&step.areas, initial_areas)
        {
            (Some(TurnoversAreasL::Unseparated { slots, .. }), init) => {
                let slot_count = usize::from(*slots);
                let mut seen = match init {
                    Some(v) if v.len() == slot_count => v,
                    None => vec![None; slot_count],
                    Some(_) => return Err(fatal("initial area slot count mismatch")),
                };
                let vec_bytes = crate::model::size_add(
                    crate::model::size_vec(slot_count, std::mem::size_of::<Option<QValue>>())?,
                    crate::model::size_vec(slot_count, std::mem::size_of::<OwnedCharge>())?,
                )?;
                let seen_charge = cx.mem.hold_owned(vec_bytes)?;
                let mut charges = Vec::with_capacity(slot_count);
                for val in seen.iter().flatten() {
                    charges.push(cx.mem.hold_owned(crate::model::value_model(val))?);
                }
                if seen.capacity() > slot_count {
                    let mut exact = Vec::with_capacity(slot_count);
                    exact.append(&mut seen);
                    seen = exact;
                }
                (seen, Some(seen_charge), charges)
            }
            _ => (Vec::new(), None, Vec::new()),
        };

        let ext_loaded = step.ext_dims.is_none();
        Ok(Self {
            fields,
            fields_charge: Some(fields_charge),
            ext_fields,
            ext_fields_charge,
            ed_fields,
            ed_fields_charge,
            seen_areas,
            seen_areas_charge,
            area_value_charges,
            ext_present: Vec::new(),
            ext_charge: None,
            ext_kinds: Vec::new(),
            ext_kinds_charge: None,
            needed_kinds: Vec::new(),
            needed_kinds_charge: None,
            ext_loaded,
            allowed_accounts: None,
            allowed_accounts_charge: None,
            allowed_loaded: false,
            ed_mode,
            ed_iter: None,
            ed_opened: false,
            ed_areas_verified: false,
            ed_exhausted: false,
            ed_last_period: None,
            ed_peek: None,
            ed_bucket_period: None,
            ed_bucket: Vec::new(),
            ed_bucket_charge: None,
            ed_loaded_full: false,
            main_last_period: None,
            iter: None,
            pending: None,
            side: 0,
            mask_step: 0,
            done: false,
        })
    }

    fn check_unseparated_area(
        &mut self,
        fields: &[(u16, FieldId)],
        row: &Row,
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        for (slot, field) in fields {
            let val = value(row, *field)?;
            check_unsep_value(
                *slot,
                val,
                &mut self.seen_areas,
                &mut self.area_value_charges,
                cx,
            )?;
        }
        Ok(())
    }

    fn verify_unsep_table_areas(
        &mut self,
        table: TableId,
        unsep: &[(u16, FieldId)],
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if unsep.is_empty() {
            return Ok(());
        }
        let info = cx
            .src
            .map()
            .try_table(table)
            .ok_or_else(|| fatal("missing table descriptor"))?;
        let sep_len = info.separators.len();
        if sep_len > 0
            && unsep.len() == sep_len
            && info
                .separators
                .iter()
                .all(|sep| unsep.iter().any(|(_, f)| *f == sep.field))
            && let Some(ix) = find_sep_prefix_index(info)
            && let Some(areas) = super::access::data_areas(cx, table, ix, sep_len, false)?
        {
            for area in &areas {
                validate_unsep_tuple(
                    info,
                    unsep,
                    area,
                    &mut self.seen_areas,
                    &mut self.area_value_charges,
                    cx,
                )?;
            }
            return Ok(());
        }
        let fields: Vec<FieldId> = unsep.iter().map(|(_, f)| *f).collect();
        let mut scan = cx.src.scan_fields(table, &fields)?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            self.check_unseparated_area(unsep, &row, cx)?;
            cx.mem.release_scratch();
        }
        Ok(())
    }

    fn verify_unseparated_areas_on_empty(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let Some(TurnoversAreasL::Unseparated { main, ext, ed, .. }) = &step.areas else {
            return Ok(());
        };
        self.verify_unsep_table_areas(step.inputs.main, main, cx)?;
        if let Some(ext_cfg) = &step.ext_dims {
            self.verify_unsep_table_areas(ext_cfg.section, ext, cx)?;
            if let Some(ed_cfg) = ext_cfg.acc_rg_ed.as_deref() {
                self.verify_unsep_table_areas(ed_cfg.table, ed, cx)?;
            }
        }
        Ok(())
    }

    fn load_allowed_accounts(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.allowed_loaded {
            return Ok(());
        }
        self.allowed_loaded = true;
        if step.account_filter.is_none() {
            return Ok(());
        }
        if step.is_account_filter_empty() {
            self.allowed_accounts = Some(Vec::new());
            return Ok(());
        }
        if let Some(ids) = step.account_filter_ids() {
            let bytes = crate::model::size_vec(ids.len(), std::mem::size_of::<[u8; 16]>())?;
            let charge = cx.mem.hold_owned(bytes)?;
            let mut out = Vec::with_capacity(ids.len());
            out.extend_from_slice(ids);
            self.allowed_accounts = Some(out);
            self.allowed_accounts_charge = Some(charge);
            return Ok(());
        }
        let Some((chart, id_field, parent_field, seeds)) = step.account_filter_hierarchy() else {
            return Ok(());
        };
        check_cancel(cx.env)?;
        let ref_table = step.inputs.ref_table;
        let mut allowed: Vec<[u8; 16]> = Vec::new();
        let mut allowed_charge: Option<OwnedCharge> = None;

        if seeds.contains(&[0; 16]) {
            let mut scan = cx.src.scan_fields(chart, &[id_field])?;
            loop {
                check_cancel(cx.env)?;
                cx.tick()?;
                let Some(row) = scan.next().transpose()? else {
                    break;
                };
                cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
                if let QValue::Ref { table, id } = value(&row, id_field)?
                    && *table == ref_table
                    && *id != [0; 16]
                {
                    let id = *id;
                    cx.mem.release_scratch();
                    push_charged_id(&mut allowed, &mut allowed_charge, id, cx)?;
                } else {
                    cx.mem.release_scratch();
                }
            }
            drop(scan);
            allowed.sort_unstable();
            allowed.dedup();
            self.allowed_accounts = Some(allowed);
            self.allowed_accounts_charge = allowed_charge;
            return Ok(());
        }

        let mut queue: Vec<[u8; 16]> = Vec::new();
        let mut queue_charge: Option<OwnedCharge> = None;
        for &seed in seeds {
            check_cancel(cx.env)?;
            cx.tick()?;
            let seed_val = QValue::Ref {
                table: ref_table,
                id: seed,
            };
            if cx
                .src
                .get_by_id_fields(chart, &seed_val, &[id_field])?
                .is_some()
            {
                push_charged_id(&mut allowed, &mut allowed_charge, seed, cx)?;
                push_charged_id(&mut queue, &mut queue_charge, seed, cx)?;
            }
        }
        if queue.is_empty() {
            self.allowed_accounts = Some(Vec::new());
            return Ok(());
        }
        allowed.sort_unstable();

        let parent_index = cx.src.map().try_table(chart).and_then(|info| {
            info.indexes
                .iter()
                .position(|ix| {
                    ix.prefix == 0 && !ix.absent && ix.fields.first() == Some(&parent_field)
                })
                .and_then(|pos| u16::try_from(pos).ok().map(IndexId::Named))
        });

        if let Some(parent_ix) = parent_index {
            let mut head = 0usize;
            while let Some(&parent_id) = queue.get(head) {
                head += 1;
                check_cancel(cx.env)?;
                cx.tick()?;
                let key = [QValue::Ref {
                    table: ref_table,
                    id: parent_id,
                }];
                let mut rows = cx.src.range_fields(
                    chart,
                    parent_ix,
                    Bound::Included(key.as_slice()),
                    Bound::Included(key.as_slice()),
                    false,
                    &[id_field],
                )?;
                loop {
                    check_cancel(cx.env)?;
                    cx.tick()?;
                    let Some(row) = rows.next().transpose()? else {
                        break;
                    };
                    cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
                    let child = match value(&row, id_field)? {
                        QValue::Ref { table, id } if *table == ref_table && *id != [0; 16] => {
                            Some(*id)
                        }
                        _ => None,
                    };
                    cx.mem.release_scratch();
                    if let Some(child_id) = child
                        && let Err(pos) = allowed.binary_search(&child_id)
                    {
                        push_charged_id(&mut allowed, &mut allowed_charge, child_id, cx)?;
                        let len = allowed.len();
                        if pos + 1 < len
                            && let Some(slice) = allowed.get_mut(pos..len)
                        {
                            slice.rotate_right(1);
                        }
                        push_charged_id(&mut queue, &mut queue_charge, child_id, cx)?;
                    }
                }
            }
        } else {
            let mut edges: Vec<([u8; 16], [u8; 16])> = Vec::new();
            let mut edges_charge: Option<OwnedCharge> = None;
            let mut scan = cx.src.scan_fields(chart, &[id_field, parent_field])?;
            loop {
                check_cancel(cx.env)?;
                cx.tick()?;
                let Some(row) = scan.next().transpose()? else {
                    break;
                };
                cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
                let pair = match (value(&row, parent_field)?, value(&row, id_field)?) {
                    (QValue::Ref { table: pt, id: pid }, QValue::Ref { table: ct, id: cid })
                        if *pt == ref_table && *ct == ref_table && *cid != [0; 16] =>
                    {
                        Some((*pid, *cid))
                    }
                    _ => None,
                };
                cx.mem.release_scratch();
                if let Some(edge) = pair {
                    push_charged_edge(&mut edges, &mut edges_charge, edge, cx)?;
                }
            }
            drop(scan);
            edges.sort_unstable_by_key(|(p, _)| *p);
            let mut head = 0usize;
            while let Some(&parent_id) = queue.get(head) {
                head += 1;
                check_cancel(cx.env)?;
                cx.tick()?;
                let start = edges.partition_point(|(p, _)| *p < parent_id);
                for &(p, child_id) in edges.get(start..).into_iter().flatten() {
                    if p != parent_id {
                        break;
                    }
                    if let Err(pos) = allowed.binary_search(&child_id) {
                        push_charged_id(&mut allowed, &mut allowed_charge, child_id, cx)?;
                        let len = allowed.len();
                        if pos + 1 < len
                            && let Some(slice) = allowed.get_mut(pos..len)
                        {
                            slice.rotate_right(1);
                        }
                        push_charged_id(&mut queue, &mut queue_charge, child_id, cx)?;
                    }
                }
            }
            drop(edges);
            drop(edges_charge);
        }
        drop(queue);
        drop(queue_charge);
        self.allowed_accounts = Some(allowed);
        self.allowed_accounts_charge = allowed_charge;
        Ok(())
    }

    fn clear_ed_bucket(&mut self) {
        self.ed_bucket = Vec::new();
        drop(self.ed_bucket_charge.take());
        self.ed_bucket_period = None;
    }

    fn push_ed_entry(&mut self, entry: EdEntry, cx: &Cx<'_>) -> Result<(), ExecError> {
        if self.ed_bucket.len() == self.ed_bucket.capacity() {
            let next_cap = self
                .ed_bucket
                .capacity()
                .checked_mul(2)
                .and_then(|n| n.checked_add(1))
                .ok_or_else(crate::model::invalid_size)?;
            let next_bytes = crate::model::size_vec(next_cap, std::mem::size_of::<EdEntry>())?;
            let next_charge = cx.mem.hold_owned(next_bytes)?;
            let mut next = Vec::with_capacity(next_cap);
            next.append(&mut self.ed_bucket);
            self.ed_bucket = next;
            self.ed_bucket_charge = Some(next_charge);
        }
        self.ed_bucket.push(entry);
        Ok(())
    }

    fn finalize_ed_bucket(&mut self) -> Result<(), ExecError> {
        self.ed_bucket
            .sort_unstable_by(|a, b| a.key.cmp(&b.key).then_with(|| a.kind.cmp(&b.kind)));
        if self
            .ed_bucket
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a.key == b.key && a.kind == b.kind))
        {
            return Err(fatal("duplicate AccRgED entry"));
        }
        Ok(())
    }

    fn decode_and_push_ed_row(
        &mut self,
        row: &Row,
        ed: &TurnoversAccRgEdL,
        expected_side: Option<u8>,
        cx: &Cx<'_>,
    ) -> Result<(), ExecError> {
        let QValue::Ref {
            table: rec_table,
            id: rec_id,
        } = value(row, ed.ed_recorder)?
        else {
            return Err(fatal("AccRgED Recorder is not a reference"));
        };
        if ed.recorder_tags.binary_search(rec_table).is_err() {
            return Err(fatal("AccRgED Recorder reference target mismatch"));
        }
        let line_no = parse_line_no(value(row, ed.ed_line)?, cx)?;
        let corr = parse_correspond(value(row, ed.ed_correspond)?)?;
        let QValue::Ref {
            table: kind_table,
            id: kind_id,
        } = value(row, ed.ed_kind)?
        else {
            return Err(fatal("AccRgED Kind is not a reference"));
        };
        if *kind_table != ed.kind_ref_table {
            return Err(fatal("AccRgED Kind reference target mismatch"));
        }
        if expected_side.is_some_and(|want| want != corr) {
            return Ok(());
        }
        if self.needed_kinds.binary_search(kind_id).is_err() {
            return Ok(());
        }
        let val = value(row, ed.ed_value)?;
        let val_bytes = crate::model::value_model(val);
        let val_charge = if val_bytes != 0 {
            Some(cx.mem.hold_owned(val_bytes)?)
        } else {
            None
        };
        self.push_ed_entry(
            EdEntry {
                key: (*rec_table, *rec_id, line_no, corr),
                kind: *kind_id,
                value: val.clone(),
                _val_charge: val_charge,
            },
            cx,
        )
    }

    fn load_ext_dims(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let Some(ext) = &step.ext_dims else {
            self.ext_loaded = true;
            return Ok(());
        };
        if ext.slots.len() > 64 || ext.ext_flag_fields.len() > 64 {
            return Err(fatal("subconto flag mask overflow"));
        }
        let acc_rg_ed = ext.acc_rg_ed.as_deref();
        check_cancel(cx.env)?;
        let default_fields = [ext.owner, ext.ordinal];
        let scan_fields: &[FieldId] = if self.ext_fields.is_empty() {
            &default_fields
        } else {
            &self.ext_fields
        };
        let mut scan = cx.src.scan_fields(ext.section, scan_fields)?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            match &step.areas {
                Some(TurnoversAreasL::Exact { ext: filters, .. }) => {
                    if !matches_exact_area(filters, &row)? {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                Some(TurnoversAreasL::Unseparated { ext: seps, .. }) => {
                    self.check_unseparated_area(seps, &row, cx)?;
                }
                None => {}
            }
            let QValue::Number(ordinal) = value(&row, ext.ordinal)? else {
                return Err(fatal("ExtDimensionTypes ordinal is not numeric"));
            };
            let ordinal = ordinal
                .to_i64()
                .filter(|n| (1..=99_999).contains(n))
                .ok_or_else(|| fatal("invalid ExtDimensionTypes ordinal"))?;
            let QValue::Ref { table, id } = value(&row, ext.owner)? else {
                return Err(fatal("ExtDimensionTypes owner is not a reference"));
            };
            if *table != step.inputs.ref_table {
                return Err(fatal("ExtDimensionTypes owner target mismatch"));
            }
            let dim_kind_id = if let Some(ed) = acc_rg_ed {
                let QValue::Ref {
                    table: dk_table,
                    id: dk_id,
                } = value(&row, ed.section_dim_kind)?
                else {
                    return Err(fatal("ExtDimensionTypes DimKind is not a reference"));
                };
                if *dk_table != ed.kind_ref_table {
                    return Err(fatal("ExtDimensionTypes DimKind target mismatch"));
                }
                Some(*dk_id)
            } else {
                None
            };
            let selected = u8::try_from(ordinal)
                .ok()
                .filter(|ord| ext.slots.iter().any(|slot| slot.ordinal == *ord));
            let owner_id = *id;
            let owner_allowed = self
                .allowed_accounts
                .as_ref()
                .is_none_or(|allowed| allowed.binary_search(&owner_id).is_ok());
            let mut flag_bits: u64 = 0;
            if selected.is_some() && owner_allowed {
                for (f_idx, &flag_field) in ext.ext_flag_fields.iter().enumerate() {
                    let QValue::Bool(flag_val) = value(&row, flag_field)? else {
                        return Err(fatal(
                            "ExtDimensionTypes ExtDimensionAccountingFlag is not Boolean",
                        ));
                    };
                    if *flag_val {
                        flag_bits |= 1u64 << f_idx;
                    }
                }
            }
            cx.mem.release_scratch();
            if let Some(ord) = selected.filter(|_| owner_allowed) {
                if let Some(dk_id) = dim_kind_id {
                    if self.ext_kinds.len() == self.ext_kinds.capacity() {
                        let next_cap = self
                            .ext_kinds
                            .capacity()
                            .checked_mul(2)
                            .and_then(|n| n.checked_add(1))
                            .ok_or_else(crate::model::invalid_size)?;
                        let next_bytes = crate::model::size_vec(
                            next_cap,
                            std::mem::size_of::<([u8; 16], u8, [u8; 16], u64)>(),
                        )?;
                        let next_charge = cx.mem.hold_owned(next_bytes)?;
                        let mut next = Vec::with_capacity(next_cap);
                        next.extend_from_slice(&self.ext_kinds);
                        self.ext_kinds = next;
                        self.ext_kinds_charge = Some(next_charge);
                    }
                    self.ext_kinds.push((owner_id, ord, dk_id, flag_bits));
                } else {
                    if self.ext_present.len() == self.ext_present.capacity() {
                        let next_cap = self
                            .ext_present
                            .capacity()
                            .checked_mul(2)
                            .and_then(|n| n.checked_add(1))
                            .ok_or_else(crate::model::invalid_size)?;
                        let next_bytes = crate::model::size_vec(
                            next_cap,
                            std::mem::size_of::<([u8; 16], u8, u64)>(),
                        )?;
                        let next_charge = cx.mem.hold_owned(next_bytes)?;
                        let mut next = Vec::with_capacity(next_cap);
                        next.extend_from_slice(&self.ext_present);
                        self.ext_present = next;
                        self.ext_charge = Some(next_charge);
                    }
                    self.ext_present.push((owner_id, ord, flag_bits));
                }
            }
        }
        drop(scan);
        if acc_rg_ed.is_some() {
            self.ext_kinds.sort_unstable_by_key(|a| (a.0, a.1));
            if self
                .ext_kinds
                .windows(2)
                .any(|pair| matches!(pair, [a, b] if a.0 == b.0 && a.1 == b.1))
            {
                return Err(fatal("duplicate ExtDimensionTypes ordinal"));
            }
            if !self.ext_kinds.is_empty() {
                let scratch =
                    crate::model::size_vec(self.ext_kinds.len(), std::mem::size_of::<[u8; 16]>())?;
                cx.mem.grow_scratch(scratch)?;
                let mut kinds: Vec<[u8; 16]> = Vec::with_capacity(self.ext_kinds.len());
                kinds.extend(self.ext_kinds.iter().map(|&(_, _, k, _)| k));
                kinds.sort_unstable();
                kinds.dedup();
                let bytes = crate::model::size_vec(kinds.len(), std::mem::size_of::<[u8; 16]>())?;
                let charge = cx.mem.hold_owned(bytes)?;
                let mut exact = Vec::with_capacity(kinds.len());
                exact.extend_from_slice(&kinds);
                drop(kinds);
                cx.mem.release_scratch();
                self.needed_kinds = exact;
                self.needed_kinds_charge = Some(charge);
            }
        } else {
            self.ext_present.sort_unstable_by_key(|a| (a.0, a.1));
            if self
                .ext_present
                .windows(2)
                .any(|pair| matches!(pair, [a, b] if a.0 == b.0 && a.1 == b.1))
            {
                return Err(fatal("duplicate ExtDimensionTypes ordinal"));
            }
        }
        self.ext_loaded = true;
        Ok(())
    }

    fn open_ed_by_period(
        &mut self,
        ed_ix: IndexId,
        step: &AccountingTurnoversL,
        ed: &TurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_opened {
            return Ok(());
        }
        let (iter, verified) = open_period_range(
            ed.table,
            ed_ix,
            true,
            step,
            &self.ed_fields,
            &mut self.seen_areas,
            &mut self.area_value_charges,
            cx,
        )?;
        self.ed_iter = Some(iter);
        self.ed_opened = true;
        self.ed_areas_verified = verified;
        Ok(())
    }

    fn advance_ed_bucket(
        &mut self,
        ed_ix: IndexId,
        target_period: BslDate,
        step: &AccountingTurnoversL,
        ed: &TurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_bucket_period == Some(target_period) {
            return Ok(());
        }
        self.clear_ed_bucket();
        if !self.ed_opened {
            self.open_ed_by_period(ed_ix, step, ed, cx)?;
        }
        if let Some((peek_period, _, _)) = &self.ed_peek {
            if *peek_period > target_period {
                self.ed_bucket_period = Some(target_period);
                return Ok(());
            }
            if let Some((peek_period, peek_row, _peek_charge)) = self.ed_peek.take()
                && peek_period == target_period
            {
                self.decode_and_push_ed_row(&peek_row, ed, None, cx)?;
            }
        }
        while !self.ed_exhausted {
            check_cancel(cx.env)?;
            cx.tick()?;
            let next_row = match self.ed_iter.as_mut() {
                Some(iter) => iter.next().transpose()?,
                None => None,
            };
            let Some(row) = next_row else {
                self.ed_exhausted = true;
                self.ed_iter = None;
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            match &step.areas {
                Some(TurnoversAreasL::Exact { ed: filters, .. }) => {
                    if !matches_exact_area(filters, &row)? {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                Some(TurnoversAreasL::Unseparated { ed: seps, .. }) => {
                    self.check_unseparated_area(seps, &row, cx)?;
                }
                None => {}
            }
            let QValue::Date(row_period) = value(&row, ed.ed_period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            let row_period = *row_period;
            if self.ed_last_period.is_some_and(|prev| row_period < prev) {
                return Err(fatal("AccRgED ByPeriod out of order"));
            }
            self.ed_last_period = Some(row_period);
            if row_period < target_period {
                cx.mem.release_scratch();
                continue;
            }
            if row_period > target_period {
                let charge = cx.mem.hold_owned(row_cap_bytes(&row))?;
                cx.mem.release_scratch();
                self.ed_peek = Some((row_period, row, charge));
                break;
            }
            self.decode_and_push_ed_row(&row, ed, None, cx)?;
            cx.mem.release_scratch();
        }
        self.finalize_ed_bucket()?;
        self.ed_bucket_period = Some(target_period);
        Ok(())
    }

    fn drain_unseparated_ed(
        &mut self,
        ed_ix: IndexId,
        step: &AccountingTurnoversL,
        ed: &TurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        let Some(TurnoversAreasL::Unseparated { ed: seps, .. }) = &step.areas else {
            return Ok(());
        };
        if seps.is_empty() {
            return Ok(());
        }
        self.ed_peek = None;
        if !self.ed_opened {
            self.open_ed_by_period(ed_ix, step, ed, cx)?;
        }
        if self.ed_areas_verified {
            return Ok(());
        }
        while !self.ed_exhausted {
            check_cancel(cx.env)?;
            cx.tick()?;
            let next_row = match self.ed_iter.as_mut() {
                Some(iter) => iter.next().transpose()?,
                None => None,
            };
            let Some(row) = next_row else {
                self.ed_exhausted = true;
                self.ed_iter = None;
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            self.check_unseparated_area(seps, &row, cx)?;
            cx.mem.release_scratch();
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
        step: &AccountingTurnoversL,
        ed: &TurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        self.clear_ed_bucket();
        let exact_ed = match &step.areas {
            Some(TurnoversAreasL::Exact { ed: filters, .. }) => Some(filters.as_slice()),
            _ => None,
        };
        let mut key = exact_sep_prefix(ed.table, exact_ed, cx.src.map())?
            .ok_or_else(|| fatal("ByRecorder fallback requires known separator prefix"))?;
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
            if let Some(TurnoversAreasL::Exact { ed: filters, .. }) = &step.areas
                && !matches_exact_area(filters, &row)?
            {
                cx.mem.release_scratch();
                continue;
            }
            if !matches!(value(&row, ed.ed_period)?, QValue::Date(_)) {
                return Err(fatal("AccRgED Period is not Date"));
            }
            self.decode_and_push_ed_row(&row, ed, Some(side), cx)?;
            cx.mem.release_scratch();
        }
        drop(iter);
        self.finalize_ed_bucket()
    }

    fn load_ed_full_scan(
        &mut self,
        step: &AccountingTurnoversL,
        ed: &TurnoversAccRgEdL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        if self.ed_loaded_full {
            return Ok(());
        }
        check_cancel(cx.env)?;
        let mut scan = cx.src.scan_fields(ed.table, &self.ed_fields)?;
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            let Some(row) = scan.next().transpose()? else {
                break;
            };
            check_cancel(cx.env)?;
            cx.mem.grow_scratch(crate::model::row_cap_model(&row))?;
            match &step.areas {
                Some(TurnoversAreasL::Exact { ed: filters, .. }) => {
                    if !matches_exact_area(filters, &row)? {
                        cx.mem.release_scratch();
                        continue;
                    }
                }
                Some(TurnoversAreasL::Unseparated { ed: seps, .. }) => {
                    self.check_unseparated_area(seps, &row, cx)?;
                }
                None => {}
            }
            let QValue::Date(row_period) = value(&row, ed.ed_period)? else {
                return Err(fatal("AccRgED Period is not Date"));
            };
            if step.dates[0].is_some_and(|start| *row_period < start)
                || step.dates[1].is_some_and(|end| *row_period > end)
            {
                cx.mem.release_scratch();
                continue;
            }
            self.decode_and_push_ed_row(&row, ed, None, cx)?;
            cx.mem.release_scratch();
        }
        drop(scan);
        self.finalize_ed_bucket()?;
        self.ed_loaded_full = true;
        Ok(())
    }

    fn open_main_iter(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<(), ExecError> {
        match self.ed_mode {
            EdMode::ByPeriod { main_ix, .. } => {
                let (iter, _) = open_period_range(
                    step.inputs.main,
                    main_ix,
                    false,
                    step,
                    &self.fields,
                    &mut self.seen_areas,
                    &mut self.area_value_charges,
                    cx,
                )?;
                self.iter = Some(iter);
            }
            EdMode::Inline | EdMode::ByRecorder { .. } | EdMode::FullScan => {
                let want_range = step.dates[0].is_some()
                    || step.dates[1].is_some()
                    || matches!(step.areas, Some(TurnoversAreasL::Exact { .. }));
                let main_ix = if want_range
                    && let Some(period) = step.period
                    && let Some(info) = cx.src.map().try_table(step.inputs.main)
                {
                    find_by_period_index(info, period)
                } else {
                    None
                };
                if let Some(main_ix) = main_ix {
                    let (iter, _) = open_period_range(
                        step.inputs.main,
                        main_ix,
                        false,
                        step,
                        &self.fields,
                        &mut self.seen_areas,
                        &mut self.area_value_charges,
                        cx,
                    )?;
                    self.iter = Some(iter);
                } else {
                    self.iter = Some(cx.src.scan_fields(step.inputs.main, &self.fields)?);
                }
            }
        }
        Ok(())
    }

    fn resource_slot_mask(
        &self,
        ext: &query_ir::lower::TurnoversExtDimsL,
        is_ed: bool,
        account_id: [u8; 16],
        ext_flag: Option<u16>,
    ) -> u64 {
        let mut mask = 0u64;
        for (slot_idx, slot) in ext.slots.iter().enumerate() {
            let flag_bits = if is_ed {
                self.ext_kinds
                    .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _, _)| (a, o))
                    .ok()
                    .and_then(|pos| self.ext_kinds.get(pos).map(|e| e.3))
            } else {
                self.ext_present
                    .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _)| (a, o))
                    .ok()
                    .and_then(|pos| self.ext_present.get(pos).map(|e| e.2))
            };
            let Some(bits) = flag_bits else {
                continue;
            };
            let enabled = match ext_flag {
                None => true,
                Some(f) => ((bits >> f) & 1) != 0,
            };
            if enabled {
                mask |= 1u64 << slot_idx;
            }
        }
        mask
    }

    pub(super) fn next(
        &mut self,
        step: &AccountingTurnoversL,
        cx: &mut Cx<'a>,
    ) -> Result<Option<Row>, ExecError> {
        if self.done {
            return Ok(None);
        }
        if !self.allowed_loaded {
            self.load_allowed_accounts(step, cx)?;
        }
        if self.allowed_accounts.as_ref().is_some_and(Vec::is_empty) {
            self.verify_unseparated_areas_on_empty(step, cx)?;
            self.close();
            return Ok(None);
        }
        if !self.ext_loaded {
            self.load_ext_dims(step, cx)?;
        }
        let acc_rg_ed = step
            .ext_dims
            .as_ref()
            .and_then(|ext| ext.acc_rg_ed.as_deref());
        if self.ed_mode == EdMode::FullScan
            && !self.ed_loaded_full
            && let Some(ed) = acc_rg_ed
        {
            self.load_ed_full_scan(step, ed, cx)?;
        }
        loop {
            check_cancel(cx.env)?;
            cx.tick()?;
            if self.done {
                return Ok(None);
            }
            if self.pending.is_none() {
                if self.iter.is_none() {
                    self.open_main_iter(step, cx)?;
                }
                let Some(iter) = self.iter.as_mut() else {
                    return Err(fatal("missing main cursor"));
                };
                let Some(row) = iter.next() else {
                    if let (EdMode::ByPeriod { ed_ix, .. }, Some(ed)) = (self.ed_mode, acc_rg_ed) {
                        self.drain_unseparated_ed(ed_ix, step, ed, cx)?;
                    }
                    self.close();
                    return Ok(None);
                };
                let row = row?;
                check_cancel(cx.env)?;
                match &step.areas {
                    Some(TurnoversAreasL::Exact { main, .. }) => {
                        if !matches_exact_area(main, &row)? {
                            continue;
                        }
                    }
                    Some(TurnoversAreasL::Unseparated { main, .. }) => {
                        self.check_unseparated_area(main, &row, cx)?;
                    }
                    None => {}
                }
                match value(&row, step.inputs.active)? {
                    QValue::Bool(false) => continue,
                    QValue::Bool(true) => {}
                    _ => return Err(fatal("Active is not Boolean")),
                }
                let period_field = step.period.or_else(|| acc_rg_ed.map(|ed| ed.main_period));
                if let Some(field) = period_field {
                    let QValue::Date(period) = value(&row, field)? else {
                        return Err(fatal("Period is not Date"));
                    };
                    if matches!(self.ed_mode, EdMode::ByPeriod { .. }) {
                        if self.main_last_period.is_some_and(|prev| *period < prev) {
                            return Err(fatal("main ByPeriod out of order"));
                        }
                        self.main_last_period = Some(*period);
                    }
                    if step.dates[0].is_some_and(|start| *period < start)
                        || step.dates[1].is_some_and(|end| *period > end)
                    {
                        continue;
                    }
                }
                if let Some(allowed) = &self.allowed_accounts {
                    let mut any_allowed = false;
                    for field in step.inputs.accounts {
                        if let QValue::Ref { table, id } = value(&row, field)?
                            && *table == step.inputs.ref_table
                            && *id != [0; 16]
                            && allowed.binary_search(id).is_ok()
                        {
                            any_allowed = true;
                            break;
                        }
                    }
                    if !any_allowed {
                        continue;
                    }
                }
                if let Some(rec) = &step.recorder {
                    match value(&row, rec.field)? {
                        QValue::Ref { table, .. }
                            if rec.reference_tags.binary_search(table).is_ok() => {}
                        _ => return Err(fatal("stored Recorder reference target mismatch")),
                    }
                } else if let Some(ed) = acc_rg_ed {
                    match value(&row, ed.main_recorder)? {
                        QValue::Ref { table, .. }
                            if ed.recorder_tags.binary_search(table).is_ok() => {}
                        _ => return Err(fatal("stored Recorder reference target mismatch")),
                    }
                }
                if let Some(field) = step.line {
                    let QValue::Number(line) = value(&row, field)? else {
                        return Err(fatal("LineNo is not Number"));
                    };
                    crate::num::guard(cx.env, line)?;
                }
                if let Some(ed) = acc_rg_ed {
                    parse_line_no(value(&row, ed.main_line)?, cx)?;
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
                self.mask_step = 0;
            }
            if self.side >= 2 {
                self.pending = None;
                self.mask_step = 0;
                continue;
            }
            let side = self.side;
            let first_mask_for_side = self.mask_step == 0;
            let (account_id, ed_side_key) = {
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
                let QValue::Ref { id: account_id, .. } = key else {
                    return Err(fatal("stored account reference target mismatch"));
                };
                if *account_id == [0; 16]
                    || self
                        .allowed_accounts
                        .as_ref()
                        .is_some_and(|allowed| allowed.binary_search(account_id).is_err())
                {
                    self.side += 1;
                    self.mask_step = 0;
                    continue;
                }
                let ed_side_key = if let Some(ed) = acc_rg_ed {
                    let QValue::Date(period) = value(row, ed.main_period)? else {
                        return Err(fatal("Period is not Date"));
                    };
                    let QValue::Ref {
                        table: rec_table,
                        id: rec_id,
                    } = value(row, ed.main_recorder)?
                    else {
                        return Err(fatal("stored Recorder reference target mismatch"));
                    };
                    let line_no = parse_line_no(value(row, ed.main_line)?, cx)?;
                    Some((*period, (*rec_table, *rec_id, line_no, side as u8)))
                } else {
                    None
                };
                (*account_id, ed_side_key)
            };

            if first_mask_for_side
                && let (Some(ext), Some(ed), Some((target_period, ed_key))) =
                    (&step.ext_dims, acc_rg_ed, ed_side_key)
            {
                let any_slot_for_account = ext.slots.iter().any(|slot| {
                    self.ext_kinds
                        .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _, _)| (a, o))
                        .is_ok()
                });
                if any_slot_for_account {
                    match self.ed_mode {
                        EdMode::ByPeriod { ed_ix, .. } => {
                            self.advance_ed_bucket(ed_ix, target_period, step, ed, cx)?;
                        }
                        EdMode::ByRecorder {
                            ed_ix,
                            has_correspond,
                        } => {
                            self.load_ed_by_recorder(
                                ed_ix,
                                has_correspond,
                                ed_key.0,
                                ed_key.1,
                                ed_key.2,
                                ed_key.3,
                                step,
                                ed,
                                cx,
                            )?;
                        }
                        EdMode::FullScan | EdMode::Inline => {}
                    }
                }
            }

            let has_ext_flags = step
                .ext_dims
                .as_ref()
                .is_some_and(|ext| !ext.ext_flag_fields.is_empty());
            let (cur_mask, all_resources) = if let (Some(ext), true) =
                (&step.ext_dims, has_ext_flags && !step.resources.is_empty())
            {
                let is_ed = acc_rg_ed.is_some();
                let cur_res = step
                    .resources
                    .get(self.mask_step)
                    .ok_or_else(|| fatal("resource mask step offset"))?;
                let cur_mask = self.resource_slot_mask(ext, is_ed, account_id, cur_res.ext_flag);
                let mut next_step = None;
                for r in (self.mask_step + 1)..step.resources.len() {
                    let cand_res = step
                        .resources
                        .get(r)
                        .ok_or_else(|| fatal("resource mask offset"))?;
                    let cand_mask =
                        self.resource_slot_mask(ext, is_ed, account_id, cand_res.ext_flag);
                    let mut seen = false;
                    for p in 0..r {
                        let prev_res = step
                            .resources
                            .get(p)
                            .ok_or_else(|| fatal("resource mask offset"))?;
                        if self.resource_slot_mask(ext, is_ed, account_id, prev_res.ext_flag)
                            == cand_mask
                        {
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
            } else {
                self.side += 1;
                self.mask_step = 0;
                (u64::MAX, true)
            };

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
            let ext_count = step.ext_dims.as_ref().map_or(0, |ext| ext.slots.len());
            let nk = usize::from(step.account) + step.dims.len() + ext_count;
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
            if let Some(field) = step.line {
                charge =
                    crate::model::size_add(charge, crate::model::value_model(value(row, field)?))?;
            }
            for dim in &step.dims {
                let field = dim
                    .main
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("dimension side offset"))?;
                let val = value(row, field)?;
                validate_typed_value(val, &dim.value_check, cx, "dimension")?;
                charge = crate::model::size_add(charge, crate::model::value_model(val))?;
            }
            if let Some(ext) = &step.ext_dims {
                if let Some((_, ed_key)) = ed_side_key {
                    for (slot_idx, slot) in ext.slots.iter().enumerate() {
                        if let Ok(pos) = self
                            .ext_kinds
                            .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _, _)| {
                                (a, o)
                            })
                            && let Some(&(_, _, kind_id, _)) = self.ext_kinds.get(pos)
                            && let Ok(ed_pos) = self
                                .ed_bucket
                                .binary_search_by_key(&(ed_key, kind_id), |e| (e.key, e.kind))
                            && let Some(entry) = self.ed_bucket.get(ed_pos)
                        {
                            validate_ext_value(&entry.value, &slot.value_check, cx)?;
                            if (cur_mask & (1u64 << slot_idx)) != 0 {
                                charge = crate::model::size_add(
                                    charge,
                                    crate::model::value_model(&entry.value),
                                )?;
                            }
                        }
                    }
                } else {
                    for (slot_idx, slot) in ext.slots.iter().enumerate() {
                        if self
                            .ext_present
                            .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _)| (a, o))
                            .is_ok()
                        {
                            let field = slot
                                .values
                                .and_then(|values| values.get(side).copied())
                                .ok_or_else(|| fatal("ExtDimension side offset"))?;
                            let val = value(row, field)?;
                            validate_ext_value(val, &slot.value_check, cx)?;
                            if (cur_mask & (1u64 << slot_idx)) != 0 {
                                charge =
                                    crate::model::size_add(charge, crate::model::value_model(val))?;
                            }
                        }
                    }
                }
            }
            for resource in &step.resources {
                let res_active = all_resources
                    || step.ext_dims.as_ref().is_none_or(|ext| {
                        self.resource_slot_mask(
                            ext,
                            acc_rg_ed.is_some(),
                            account_id,
                            resource.ext_flag,
                        ) == cur_mask
                    });
                if res_active && stored_number(row, resource, side)?.is_some() {
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
            for slot in out.iter_mut().take(raw::DIMS + nk) {
                *slot = QValue::Null;
            }
            if let Some(field) = step.period {
                *out.get_mut(raw::PERIOD)
                    .ok_or_else(|| fatal("raw period offset"))? = value(row, field)?.clone();
            }
            if let Some(rec) = &step.recorder {
                *out.get_mut(raw::RECORDER)
                    .ok_or_else(|| fatal("raw recorder offset"))? = value(row, rec.field)?.clone();
            }
            if let Some(field) = step.line {
                *out.get_mut(raw::LINE)
                    .ok_or_else(|| fatal("raw line offset"))? = value(row, field)?.clone();
            }
            if step.account {
                *out.get_mut(raw::DIMS)
                    .ok_or_else(|| fatal("raw account offset"))? = key.clone();
            }
            let dim_base = raw::DIMS + usize::from(step.account);
            for (index, dim) in step.dims.iter().enumerate() {
                let field = dim
                    .main
                    .get(side)
                    .copied()
                    .ok_or_else(|| fatal("dimension side offset"))?;
                *out.get_mut(dim_base + index)
                    .ok_or_else(|| fatal("raw dimension offset"))? = value(row, field)?.clone();
            }
            if let Some(ext) = &step.ext_dims {
                let ext_base = dim_base + step.dims.len();
                if let Some((_, ed_key)) = ed_side_key {
                    for (index, slot) in ext.slots.iter().enumerate() {
                        if (cur_mask & (1u64 << index)) == 0 {
                            continue;
                        }
                        if let Ok(pos) = self
                            .ext_kinds
                            .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _, _)| {
                                (a, o)
                            })
                            && let Some(&(_, _, kind_id, _)) = self.ext_kinds.get(pos)
                            && let Ok(ed_pos) = self
                                .ed_bucket
                                .binary_search_by_key(&(ed_key, kind_id), |e| (e.key, e.kind))
                            && let Some(entry) = self.ed_bucket.get(ed_pos)
                        {
                            *out.get_mut(ext_base + index)
                                .ok_or_else(|| fatal("raw ExtDimension offset"))? =
                                entry.value.clone();
                        }
                    }
                } else {
                    for (index, slot) in ext.slots.iter().enumerate() {
                        if (cur_mask & (1u64 << index)) == 0 {
                            continue;
                        }
                        if self
                            .ext_present
                            .binary_search_by_key(&(account_id, slot.ordinal), |&(a, o, _)| (a, o))
                            .is_ok()
                        {
                            let field = slot
                                .values
                                .and_then(|values| values.get(side).copied())
                                .ok_or_else(|| fatal("ExtDimension side offset"))?;
                            *out.get_mut(ext_base + index)
                                .ok_or_else(|| fatal("raw ExtDimension offset"))? =
                                value(row, field)?.clone();
                        }
                    }
                }
            }
            for (r, resource) in step.resources.iter().enumerate() {
                let res_active = all_resources
                    || step.ext_dims.as_ref().is_none_or(|ext| {
                        self.resource_slot_mask(
                            ext,
                            acc_rg_ed.is_some(),
                            account_id,
                            resource.ext_flag,
                        ) == cur_mask
                    });
                if !res_active {
                    continue;
                }
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
                crate::ordinary_calibration_trace::Backing::of(&self.ext_present),
            ],
        );
        self.iter = None;
        self.pending = None;
        self.mask_step = 0;
        self.ed_iter = None;
        self.ed_peek = None;
        self.ed_opened = false;
        self.ed_areas_verified = false;
        self.ed_exhausted = false;
        self.ed_last_period = None;
        self.ed_loaded_full = false;
        self.main_last_period = None;
        self.clear_ed_bucket();
        self.fields = Vec::new();
        drop(self.fields_charge.take());
        self.ext_fields = Vec::new();
        drop(self.ext_fields_charge.take());
        self.ed_fields = Vec::new();
        drop(self.ed_fields_charge.take());
        self.seen_areas = Vec::new();
        self.area_value_charges = Vec::new();
        drop(self.seen_areas_charge.take());
        self.ext_present = Vec::new();
        drop(self.ext_charge.take());
        self.ext_kinds = Vec::new();
        drop(self.ext_kinds_charge.take());
        self.needed_kinds = Vec::new();
        drop(self.needed_kinds_charge.take());
        self.allowed_accounts = None;
        drop(self.allowed_accounts_charge.take());
        self.allowed_loaded = false;
        self.ext_loaded = false;
        self.done = true;
        #[cfg(test)]
        crate::ordinary_calibration_trace::mark(
            "ordinary-cursor-closed",
            0,
            [Default::default(); 3],
        );
    }
}
