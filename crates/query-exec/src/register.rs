use std::collections::BTreeMap;
use std::ops::Bound;

use bsl_prim::{BslDate, Number};
use ibschema::names::{StdField, purpose, role};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use ibschema::values::Uuid16;
use query_ir::{ExecError, QValue};

use crate::source::{DataSource, IndexPrefixes, Row, RowIter};

fn current_totals_period() -> BslDate {
    BslDate::from_parts(3999, 11, 1, 0, 0, 0).unwrap_or(BslDate::EMPTY)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterSettings {
    pub use_totals: bool,
    pub calculated: Option<BslDate>,
    pub current: bool,
    pub use_splitter: bool,
    pub min_period: BslDate,
    pub min_calculated: Option<BslDate>,
    pub agg_mode: bool,
    pub slice_using: Option<bool>,
}

impl RegisterSettings {
    pub fn defaults() -> RegisterSettings {
        RegisterSettings {
            use_totals: true,
            calculated: None,
            current: true,
            use_splitter: false,
            min_period: current_totals_period(),
            min_calculated: None,
            agg_mode: false,
            slice_using: None,
        }
    }

    pub fn movements_only() -> RegisterSettings {
        RegisterSettings {
            current: false,
            calculated: None,
            agg_mode: true,
            slice_using: Some(false),
            ..RegisterSettings::defaults()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterKind {
    Accumulation,
    Information,
}

fn fatal(s: impl Into<String>) -> ExecError {
    ExecError::Fatal(s.into())
}

fn service(map: &StorageMap, md: Uuid16, role: &str) -> Option<TableId> {
    map.object(md)?
        .service
        .iter()
        .find(|(p, _)| p == role)
        .map(|(_, t)| *t)
}

pub fn reg_id(md: Uuid16) -> [u8; 16] {
    md.to_bytes_le()
}

fn get<'r>(info: &TableInfo, row: &'r [QValue], std: StdField) -> Option<&'r QValue> {
    let f = info.field_by_std(std)?;
    row.get(usize::from(f.0))
}

fn date(v: Option<&QValue>) -> Option<BslDate> {
    match v {
        Some(QValue::Date(d)) if *d != BslDate::EMPTY => Some(*d),
        _ => None,
    }
}

fn boolean(v: Option<&QValue>) -> Option<bool> {
    match v {
        Some(QValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn is_one(v: Option<&QValue>) -> bool {
    matches!(v, Some(QValue::Number(n)) if *n == Number::from(1))
}

fn is_empty_value(v: &QValue) -> bool {
    match v {
        QValue::Null | QValue::Undefined => true,
        QValue::Number(n) => *n == Number::zero(),
        QValue::String(s) => s.is_empty(),
        QValue::Bool(b) => !*b,
        QValue::Binary(b) => b.iter().all(|x| *x == 0),
        _ => false,
    }
}

fn settings_row(src: &dyn DataSource, t: TableId, md: Uuid16) -> Result<Option<Row>, ExecError> {
    let info = src
        .map()
        .try_table(t)
        .ok_or_else(|| fatal("register settings: unknown table"))?;
    let want = reg_id(md);
    let mut found: Vec<Row> = Vec::new();
    for row in src.scan(t)? {
        let row = row?;
        let hit = match get(info, &row, StdField::RegId) {
            Some(QValue::Binary(b)) => b.as_ref() == want.as_slice(),
            _ => false,
        };
        if hit {
            found.push(row);
        }
    }
    if found.len() <= 1 {
        return Ok(found.pop());
    }
    let shared = |row: &Row| {
        info.separators
            .iter()
            .all(|sep| row.get(usize::from(sep.field.0)).is_none_or(is_empty_value))
    };
    found
        .into_iter()
        .find(shared)
        .map(Some)
        .ok_or_else(|| {
            fatal(format!(
                "register settings: several rows of {} and none of the shared area (data areas: stage Q7)",
                info.logical
            ))
        })
}

fn apply_accumulation_row(info: &TableInfo, row: &[QValue], s: &mut RegisterSettings) {
    s.use_totals = !is_one(get(info, row, StdField::UseTotals));
    s.calculated = date(get(info, row, StdField::Period));
    s.current = boolean(get(info, row, StdField::ActualPeriod)).unwrap_or(true);
    s.use_splitter = boolean(get(info, row, StdField::UseSplitter)).unwrap_or(false);
    s.min_period = date(get(info, row, StdField::MinPeriod)).unwrap_or(s.min_period);
    s.min_calculated = date(get(info, row, StdField::MinCalculatedPeriod));
}

pub fn read_register_settings(
    src: &dyn DataSource,
    md: Uuid16,
    kind: RegisterKind,
) -> Result<RegisterSettings, ExecError> {
    let map = src.map();
    if map.object(md).is_none() {
        return Err(fatal(format!(
            "register settings: register {md} is not in the storage map"
        )));
    }
    let mut s = RegisterSettings::defaults();
    match kind {
        RegisterKind::Accumulation => {
            let t =
                service(map, md, role::ACCUMULATION_REGISTER_TOTALS_OPTIONS).ok_or_else(|| {
                    fatal(format!(
                        "register settings: no {} table for register {md}",
                        purpose::ACCUM_RG_OPT
                    ))
                })?;
            if let Some(row) = settings_row(src, t, md)? {
                apply_accumulation_row(map.table(t), &row, &mut s);
            }
            if let Some(t) = service(map, md, role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS) {
                let info = map.table(t);
                if let Some(row) = settings_row(src, t, md)? {
                    s.agg_mode = boolean(get(info, &row, StdField::AggMode)).unwrap_or(false);
                }
            }
        }
        RegisterKind::Information => {
            if let Some(t) = service(map, md, role::INFORMATION_REGISTER_TOTALS_OPTIONS) {
                let info = map.table(t);
                if let Some(row) = settings_row(src, t, md)? {
                    s.slice_using = boolean(get(info, &row, StdField::SliceUsing));
                }
            }
        }
    }
    Ok(s)
}

pub fn settings_tables(map: &StorageMap, md: Uuid16) -> Vec<TableId> {
    [
        role::ACCUMULATION_REGISTER_TOTALS_OPTIONS,
        role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS,
        role::INFORMATION_REGISTER_TOTALS_OPTIONS,
    ]
    .into_iter()
    .filter_map(|p| service(map, md, p))
    .collect()
}

fn encode_row(info: &TableInfo, md: Uuid16, s: &RegisterSettings) -> Row {
    let none = BslDate::EMPTY;
    info.fields
        .iter()
        .map(|f| match f.std {
            Some(StdField::RegId) => QValue::Binary(reg_id(md).as_slice().into()),
            Some(StdField::Period) => QValue::Date(s.calculated.unwrap_or(none)),
            Some(StdField::ActualPeriod) => QValue::Bool(s.current),
            Some(StdField::Periodicity) => QValue::Number(Number::from(9)),
            Some(StdField::RepetitionFactor) => QValue::Number(Number::from(1)),
            Some(StdField::UseTotals) => QValue::Number(Number::from(i32::from(!s.use_totals))),
            Some(StdField::MinPeriod) => QValue::Date(s.min_period),
            Some(StdField::UseSplitter) => QValue::Bool(s.use_splitter),
            Some(StdField::MinCalculatedPeriod) => QValue::Date(s.min_calculated.unwrap_or(none)),
            Some(StdField::AggMode) => QValue::Bool(s.agg_mode),
            Some(StdField::SliceUsing) => QValue::Bool(s.slice_using.unwrap_or(false)),
            _ => QValue::Null,
        })
        .collect()
}

pub struct SettingsOverride<'a> {
    inner: &'a dyn DataSource,
    by_table: BTreeMap<TableId, Vec<(Uuid16, RegisterSettings)>>,
}

impl<'a> SettingsOverride<'a> {
    pub fn new(inner: &'a dyn DataSource) -> SettingsOverride<'a> {
        SettingsOverride {
            inner,
            by_table: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, md: Uuid16, s: RegisterSettings) -> &mut Self {
        for t in settings_tables(self.inner.map(), md) {
            let rows = self.by_table.entry(t).or_default();
            rows.retain(|(m, _)| *m != md);
            rows.push((md, s.clone()));
        }
        self
    }
}

impl DataSource for SettingsOverride<'_> {
    fn map(&self) -> &StorageMap {
        self.inner.map()
    }

    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError> {
        let Some(over) = self.by_table.get(&t) else {
            return self.inner.scan(t);
        };
        let info = self
            .inner
            .map()
            .try_table(t)
            .ok_or_else(|| fatal("register settings: unknown table"))?;
        let ids: Vec<Vec<u8>> = over.iter().map(|(md, _)| reg_id(*md).to_vec()).collect();
        let mut rows: Vec<Result<Row, ExecError>> = Vec::new();
        for row in self.inner.scan(t)? {
            let row = row?;
            let replaced = matches!(get(info, &row, StdField::RegId),
                Some(QValue::Binary(b)) if ids.iter().any(|id| b.as_ref() == id.as_slice()));
            if !replaced {
                rows.push(Ok(row));
            }
        }
        rows.extend(over.iter().map(|(md, s)| Ok(encode_row(info, *md, s))));
        Ok(Box::new(rows.into_iter()))
    }

    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
    ) -> Result<RowIter<'_>, ExecError> {
        if self.by_table.contains_key(&t) {
            return Err(fatal(
                "register settings override: only scans of a settings table are supported",
            ));
        }
        self.inner.range(t, i, lo, hi, desc)
    }

    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError> {
        self.inner.get_by_id(t, id)
    }

    fn row_count_estimate(&self, t: TableId) -> u64 {
        self.inner.row_count_estimate(t)
    }

    fn scan_fields(&self, t: TableId, fields: &[FieldId]) -> Result<RowIter<'_>, ExecError> {
        if self.by_table.contains_key(&t) {
            return self.scan(t);
        }
        self.inner.scan_fields(t, fields)
    }

    fn range_fields(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
        fields: &[FieldId],
    ) -> Result<RowIter<'_>, ExecError> {
        if self.by_table.contains_key(&t) {
            return self.range(t, i, lo, hi, desc);
        }
        self.inner.range_fields(t, i, lo, hi, desc, fields)
    }

    fn get_by_id_fields(
        &self,
        t: TableId,
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        self.inner.get_by_id_fields(t, id, fields)
    }

    fn index_prefixes(
        &self,
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    ) -> Result<Option<IndexPrefixes>, ExecError> {
        self.inner.index_prefixes(t, i, n, limit)
    }

    fn get_by_id_in(
        &self,
        t: TableId,
        area: &[QValue],
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        self.inner.get_by_id_in(t, area, id, fields)
    }
}
