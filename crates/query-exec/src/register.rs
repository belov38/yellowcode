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

fn settings_row(
    src: &dyn DataSource,
    t: TableId,
    md: Uuid16,
    kind: RegisterKind,
) -> Result<Option<Row>, ExecError> {
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
            None if kind == RegisterKind::Information
                && info.field_by_std(StdField::RegId).is_none() =>
            {
                true
            }
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
    read_register_settings_with_observation(src, md, kind, None)
}

pub(crate) fn read_register_settings_with_observation(
    src: &dyn DataSource,
    md: Uuid16,
    kind: RegisterKind,
    observation: Option<&crate::observe::ObservationSession>,
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
            if let Some(obs) = observation {
                obs.table(t);
            }
            if let Some(row) = settings_row(src, t, md, kind)? {
                apply_accumulation_row(map.table(t), &row, &mut s);
            }
            if let Some(t) = service(map, md, role::ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS) {
                let info = map.table(t);
                if let Some(obs) = observation {
                    obs.table(t);
                }
                if let Some(row) = settings_row(src, t, md, kind)? {
                    s.agg_mode = boolean(get(info, &row, StdField::AggMode)).unwrap_or(false);
                }
            }
        }
        RegisterKind::Information => {
            if let Some(t) = service(map, md, role::INFORMATION_REGISTER_TOTALS_OPTIONS) {
                let info = map.table(t);
                if let Some(obs) = observation {
                    obs.table(t);
                }
                if let Some(row) = settings_row(src, t, md, kind)? {
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

pub fn accumulation_registers(map: &StorageMap) -> Vec<Uuid16> {
    let mut out: Vec<Uuid16> = map
        .tables()
        .iter()
        .filter(|t| t.purpose == role::ACCUMULATION_REGISTER_TOTALS_OPTIONS)
        .filter_map(|t| t.md)
        .collect();
    out.sort_by_key(|m| *m.as_bytes());
    out.dedup();
    out
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
    inner: Inner<'a>,
    by_table: BTreeMap<TableId, Vec<(Uuid16, RegisterSettings)>>,
}

enum Inner<'a> {
    Borrowed(&'a dyn DataSource),
    Owned(Box<dyn DataSource + 'a>),
}

impl<'a> SettingsOverride<'a> {
    pub fn new(inner: &'a dyn DataSource) -> SettingsOverride<'a> {
        SettingsOverride {
            inner: Inner::Borrowed(inner),
            by_table: BTreeMap::new(),
        }
    }

    pub fn owning(inner: Box<dyn DataSource + 'a>) -> SettingsOverride<'a> {
        SettingsOverride {
            inner: Inner::Owned(inner),
            by_table: BTreeMap::new(),
        }
    }

    fn src(&self) -> &dyn DataSource {
        match &self.inner {
            Inner::Borrowed(s) => *s,
            Inner::Owned(s) => s.as_ref(),
        }
    }

    pub fn movements_everywhere(&mut self) -> &mut Self {
        for md in accumulation_registers(self.src().map()) {
            let stored = read_register_settings(self.src(), md, RegisterKind::Accumulation)
                .map(|s| s.use_totals)
                .unwrap_or(true);
            self.set(
                md,
                RegisterSettings {
                    use_totals: stored,
                    ..RegisterSettings::movements_only()
                },
            );
        }
        self
    }

    pub fn set(&mut self, md: Uuid16, s: RegisterSettings) -> &mut Self {
        for t in settings_tables(self.src().map(), md) {
            let rows = self.by_table.entry(t).or_default();
            rows.retain(|(m, _)| *m != md);
            rows.push((md, s.clone()));
        }
        self
    }
}

impl DataSource for SettingsOverride<'_> {
    fn map(&self) -> &StorageMap {
        self.src().map()
    }

    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError> {
        let Some(over) = self.by_table.get(&t) else {
            return self.src().scan(t);
        };
        let info = self
            .src()
            .map()
            .try_table(t)
            .ok_or_else(|| fatal("register settings: unknown table"))?;
        let ids: Vec<Vec<u8>> = over.iter().map(|(md, _)| reg_id(*md).to_vec()).collect();
        let mut rows: Vec<Result<Row, ExecError>> = Vec::new();
        for row in self.src().scan(t)? {
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
        self.src().range(t, i, lo, hi, desc)
    }

    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError> {
        self.src().get_by_id(t, id)
    }

    fn row_count_estimate(&self, t: TableId) -> u64 {
        self.src().row_count_estimate(t)
    }

    fn scan_fields(&self, t: TableId, fields: &[FieldId]) -> Result<RowIter<'_>, ExecError> {
        if self.by_table.contains_key(&t) {
            return self.scan(t);
        }
        self.src().scan_fields(t, fields)
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
        self.src().range_fields(t, i, lo, hi, desc, fields)
    }

    fn get_by_id_fields(
        &self,
        t: TableId,
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        self.src().get_by_id_fields(t, id, fields)
    }

    fn index_prefixes(
        &self,
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    ) -> Result<Option<IndexPrefixes>, ExecError> {
        self.src().index_prefixes(t, i, n, limit)
    }

    fn get_by_id_in(
        &self,
        t: TableId,
        area: &[QValue],
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        self.src().get_by_id_in(t, area, id, fields)
    }
}

pub fn resolve_pending_view_failure(
    src: &dyn DataSource,
    register: Uuid16,
    original: query_ir::CompileError,
) -> Result<query_ir::CompileError, ExecError> {
    resolve_pending_view_failure_with_observation(src, register, original, None)
}

pub(crate) fn resolve_pending_view_failure_with_observation(
    src: &dyn DataSource,
    register: Uuid16,
    mut original: query_ir::CompileError,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<query_ir::CompileError, ExecError> {
    let settings = read_register_settings_with_observation(
        src,
        register,
        RegisterKind::Accumulation,
        observation,
    )?;
    if !settings.use_totals {
        original.cause = Some(Box::new(totals_off_cause()));
    }
    if let Some(obs) = observation {
        obs.terminal();
    }
    Ok(original)
}

fn totals_off_cause() -> query_ir::CompileError {
    query_syntax::QueryError::new(query_syntax::MsgId::USE_TOTALS_SWITCH_OFF, None)
}

struct TotalsUseStep {
    register: Uuid16,
    kind: RegisterKind,
    view: Option<query_syntax::Span>,
}

fn steps_of(r: &query_ir::RelL, out: &mut Vec<TotalsUseStep>) {
    use query_ir::{PExpr, RelL};
    let expr = |e: &PExpr, out: &mut Vec<TotalsUseStep>| {
        let mut subs: Vec<std::sync::Arc<RelL>> = Vec::new();
        crate::plan::walk(e, &mut |x| match x {
            PExpr::InQuery { q, .. } => subs.push(q.clone()),
            PExpr::SectionAgg { rel, .. } => subs.push(rel.clone()),
            _ => {}
        });
        for s in subs {
            steps_of(&s, out);
        }
    };
    match r {
        RelL::RegisterRows(s) => out.push(TotalsUseStep {
            register: s.register,
            kind: RegisterKind::Accumulation,
            view: s.view,
        }),
        RelL::Slice {
            input,
            totals,
            step,
        } => {
            out.push(TotalsUseStep {
                register: step.register,
                kind: RegisterKind::Information,
                view: step.view,
            });
            steps_of(input, out);
            if let Some(t) = totals {
                steps_of(t, out);
            }
            for e in step.bound.iter() {
                expr(e, out);
            }
        }
        RelL::Empty | RelL::Scan { .. } | RelL::TempScan { .. } | RelL::Values { .. } => {}
        RelL::HierarchyClosure { seeds, .. } => match seeds {
            query_ir::lower::HierSeeds::Values(values) => values.iter().for_each(|e| expr(e, out)),
            query_ir::lower::HierSeeds::Rel(rel) => steps_of(rel, out),
        },
        RelL::Join { l, r, on, .. } => {
            steps_of(l, out);
            steps_of(r, out);
            expr(on, out);
        }
        RelL::Filter { input, pred } => {
            steps_of(input, out);
            expr(pred, out);
        }
        RelL::Project { input, exprs } => {
            steps_of(input, out);
            exprs.iter().for_each(|e| expr(e, out));
        }
        RelL::RunningBalance { input, step } => {
            steps_of(input, out);
            expr(&step.period, out);
            step.period_fields.iter().for_each(|e| expr(e, out));
            step.bounds.iter().flatten().for_each(|e| expr(e, out));
        }
        RelL::Aggregate {
            input, keys, aggs, ..
        } => {
            steps_of(input, out);
            keys.iter().for_each(|e| expr(e, out));
            for agg in aggs {
                if let Some(e) = &agg.arg {
                    expr(e, out);
                }
            }
        }
        RelL::Sort { input, keys } | RelL::HierarchyOrder { input, keys, .. } => {
            steps_of(input, out);
            for (e, _) in keys {
                expr(e, out);
            }
        }
        RelL::CheckAccess { input, .. } | RelL::Distinct(input) | RelL::Top { input, .. } => {
            steps_of(input, out)
        }
        RelL::SemiJoin { input, q, l, .. } => {
            l.iter().for_each(|e| expr(e, out));
            steps_of(input, out);
            steps_of(q, out);
        }
        RelL::Union { parts, .. } => parts.iter().for_each(|p| steps_of(p, out)),
    }
}

pub fn check_totals_use(
    stmt: &query_ir::lower::StmtL,
    src: &dyn DataSource,
    text: &str,
) -> Result<Option<query_ir::CompileError>, ExecError> {
    check_totals_use_impl(stmt, src, text, None)
}

fn check_totals_use_impl(
    stmt: &query_ir::lower::StmtL,
    src: &dyn DataSource,
    text: &str,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<Option<query_ir::CompileError>, ExecError> {
    let mut steps = Vec::new();
    steps_of(&stmt.root, &mut steps);
    for s in steps {
        if s.kind != RegisterKind::Accumulation {
            continue;
        }
        let settings = read_register_settings_with_observation(
            src,
            s.register,
            RegisterKind::Accumulation,
            observation,
        )?;
        if settings.use_totals {
            continue;
        }
        let cause = totals_off_cause();
        let Some(span) = s.view else {
            return Ok(Some(cause));
        };
        let map = query_syntax::SourceMap::new(text);
        let path = text
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default();
        return Ok(Some(
            query_syntax::QueryError::new(
                query_syntax::MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW,
                Some(map.at(text, span.start)),
            )
            .with_quoted(query_syntax::MsgArg::text(path))
            .with_cause(cause),
        ));
    }
    Ok(None)
}

pub(crate) fn check_slice_use(
    stmt: &query_ir::lower::StmtL,
    src: &dyn DataSource,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<(), ExecError> {
    let mut steps = Vec::new();
    steps_of(&stmt.root, &mut steps);
    for nested in &stmt.nested {
        if let Some(rows) = &nested.rows {
            steps_of(&rows.rel, &mut steps);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for step in steps {
        if step.kind != RegisterKind::Information || !seen.insert(step.register) {
            continue;
        }
        let settings =
            read_register_settings_with_observation(src, step.register, step.kind, observation)?;
        if settings.slice_using == Some(false) {
            if let Some(obs) = observation {
                obs.terminal();
            }
            return Err(crate::ops::register::totals_off());
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn check_totals_use_observed(
    stmt: &query_ir::lower::StmtL,
    src: &dyn DataSource,
    text: &str,
    obs: &crate::observe::ObservationSession,
) -> Result<Option<query_ir::CompileError>, ExecError> {
    obs.begin_precheck();
    let result = check_totals_use_impl(stmt, src, text, Some(obs));
    match &result {
        Ok(Some(_)) => obs.terminal(),
        Err(_) => obs.finish(false),
        Ok(None) => {}
    }
    result
}
