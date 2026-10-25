use crate::Console;
use crate::cell::{
    Cell, CellKind, Column, RowSizes, cell, cell_write, column_kind, column_model, row_bytes,
    row_write,
};
use memory_budget::{BudgetError, Category, Evictor, LimitReason, Reservation};
use query_ir::QValue;
use query_syntax::Lang as SynLang;
use query_testhost::{
    BatchMemory, CaseOutcome, RunFailure, RunMode, StmtRun, TestBase, parse_params,
    run_text_cancel, run_text_cancel_budgeted,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunRefusal {
    Result(memory_budget::MemoryLimit),
    Allocation,
}

impl std::fmt::Display for RunRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Result(m) => f.write_str(m.public_text()),
            Self::Allocation => f.write_str("MEMORY_ALLOCATION_FAILED"),
        }
    }
}

impl std::error::Error for RunRefusal {}

pub const MAX_ROWS: usize = 100_000;
const MAX_NESTED_ROWS: usize = 10_000;
pub const MAX_BYTES: usize = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub(crate) fn syn(self) -> SynLang {
        match self {
            Lang::Ru => SynLang::Ru,
            Lang::En => SynLang::En,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Batch,
    One,
    Inter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamIn {
    pub name: String,
    pub repr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRequest {
    pub text: String,
    #[serde(default)]
    pub params: Vec<ParamIn>,
    pub mode: Mode,
    pub lang: Lang,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Compile,
    Runtime,
    Params,
    Later,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorOut {
    pub phase: Phase,
    pub chain: Vec<String>,
    pub at: Option<(u32, u32)>,
}

impl ErrorOut {
    pub(crate) fn new(phase: Phase, text: impl Into<String>) -> ErrorOut {
        ErrorOut {
            phase,
            chain: vec![text.into()],
            at: None,
        }
    }

    pub fn at_text(&self) -> Option<String> {
        self.at.map(|(l, c)| format!("{{({l}, {c})}}"))
    }
}

pub(crate) fn later_text(what: &str) -> String {
    match what.rsplit_once(": ") {
        Some((w, stage)) if stage.starts_with('Q') => format!("ждёт {stage}: {w}"),
        _ => format!("ждёт этапа Q: {what}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Stmt {
    Result {
        columns: Vec<Column>,
        total: u64,
        kept: u64,
        truncated: bool,
        ordered: bool,
    },
    Count {
        name: String,
        n: u64,
    },
    Dropped,
}

#[derive(Clone, Debug)]
pub(crate) struct StmtRows {
    pub columns: Vec<Column>,
    pub ordered: bool,
    pub rows: Vec<Vec<Cell>>,
    pub nested: std::collections::HashMap<(usize, usize), crate::refs::NestedOut>,
}

#[derive(Clone, Debug, Default)]
pub struct Rows {
    pub(crate) stmts: Vec<StmtRows>,
}

impl Rows {
    pub fn stmt_count(&self) -> usize {
        self.stmts.len()
    }

    pub fn approx_bytes(&self) -> usize {
        self.stmts
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| row_bytes(r))
            .sum()
    }

    pub fn kept(&self, stmt: usize) -> usize {
        self.stmts.get(stmt).map_or(0, |s| s.rows.len())
    }

    pub fn slice(&self, stmt: usize, from: usize, limit: usize) -> &[Vec<Cell>] {
        let Some(s) = self.stmts.get(stmt) else {
            return &[];
        };
        let start = from.min(s.rows.len());
        let end = start.saturating_add(limit).min(s.rows.len());
        s.rows.get(start..end).unwrap_or(&[])
    }

    pub fn nested(&self, stmt: usize, row: usize, col: usize) -> Option<&crate::refs::NestedOut> {
        self.stmts.get(stmt)?.nested.get(&(row, col))
    }

    pub fn columns(&self, stmt: usize) -> &[Column] {
        self.stmts.get(stmt).map_or(&[], |s| &s.columns)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub statements: Vec<Stmt>,
    pub elapsed_ms: u64,
    pub error: Option<ErrorOut>,
    #[serde(skip)]
    pub rows: Rows,
    #[serde(skip)]
    pub(crate) later: Option<&'static str>,
    #[serde(skip)]
    pub cancelled: bool,
    #[serde(skip)]
    pub(crate) retained: Option<memory_budget::Shared<()>>,
}

impl Report {
    fn failed(started: Instant, error: ErrorOut, later: Option<&'static str>) -> Report {
        Report {
            statements: Vec::new(),
            elapsed_ms: elapsed_ms(started),
            error: Some(error),
            rows: Rows::default(),
            later,
            cancelled: false,
            retained: None,
        }
    }

    pub fn retained_bytes(&self) -> Option<u64> {
        self.retained.as_ref().map(|r| r.reservation().bytes())
    }

    pub fn case_outcome(&self) -> CaseOutcome {
        if let Some(w) = self.later {
            return CaseOutcome::Unsupported(w);
        }
        if let Some(e) = &self.error {
            return CaseOutcome::Err(e.chain.clone());
        }
        let results: Vec<Value> = self
            .rows
            .stmts
            .iter()
            .zip(&self.statements)
            .map(|(s, st)| {
                if matches!(st, Stmt::Dropped) {
                    return Value::Null;
                }
                let columns: Vec<Value> = s
                    .columns
                    .iter()
                    .map(|c| json!({"name": c.name, "type": c.ty, "width": c.width}))
                    .collect();
                let rows: Vec<Value> = s
                    .rows
                    .iter()
                    .map(|r| Value::Array(r.iter().map(|c| Value::String(c.t.clone())).collect()))
                    .collect();
                json!({"columns": columns, "empty": rows.is_empty(), "rows": rows, "ordered": s.ordered})
            })
            .collect();
        CaseOutcome::Ok(json!({ "results": results }))
    }
}

fn elapsed_ms(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

const INTERRUPTED: &str = "выполнение прервано";

impl Console {
    pub(crate) fn request_params(
        &self,
        req: &RunRequest,
    ) -> Result<query_testhost::CaseParams, (ErrorOut, Option<&'static str>)> {
        let pairs: Vec<(&str, &str)> = req
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.repr.as_str()))
            .collect();
        for (name, repr) in &pairs {
            if repr.trim_start().starts_with("ТаблицаЗначений<") {
                let w = "value table";
                let mut err = ErrorOut::new(Phase::Later, later_text(w));
                err.chain.insert(0, format!("параметр {name}"));
                return Err((err, Some(w)));
            }
            if let Err(e) = parse_params(&self.base, &[(name, repr)]) {
                return Err(match e {
                    query_testhost::ParamError::Unsupported(w) => {
                        let mut err = ErrorOut::new(Phase::Later, later_text(w));
                        err.chain.insert(0, format!("параметр {name}"));
                        (err, Some(w))
                    }
                    query_testhost::ParamError::Bad(_) => (
                        ErrorOut::new(Phase::Params, param_error_text(name, &e)),
                        None,
                    ),
                });
            }
        }
        parse_params(&self.base, &pairs)
            .map_err(|e| (ErrorOut::new(Phase::Params, param_error_text("", &e)), None))
    }

    pub fn run_budgeted(
        &self,
        req: &RunRequest,
        cancel: &AtomicBool,
    ) -> Result<Report, RunRefusal> {
        let started = Instant::now();
        let Some(memory) = BatchMemory::new(&self.base) else {
            return Ok(self.run(req, cancel));
        };
        let memory = memory.map_err(RunRefusal::Result)?;
        let bytes: u64 = req
            .params
            .iter()
            .map(|p| (p.name.len() + p.repr.len()) as u64)
            .sum();
        if let Err(m) = memory.params_stage(bytes, req.params.len()) {
            return Ok(Report::failed(
                started,
                ErrorOut::new(Phase::Runtime, m.public_text()),
                None,
            ));
        }
        let params = match self.request_params(req) {
            Ok(p) => p,
            Err((err, later)) => return Ok(Report::failed(started, err, later)),
        };
        let mode = RunMode {
            lang: req.lang.syn(),
            inter: req.mode == Mode::Inter,
        };
        let mut runs =
            match run_text_cancel_budgeted(&self.base, &req.text, &params, mode, cancel, &memory) {
                Ok(r) => r,
                Err(f) => return Ok(self.failure(started, f, cancel)),
            };
        if req.mode == Mode::One {
            let last = runs.pop();
            runs = last.into_iter().collect();
        }
        let report = self.report_budgeted(started, &runs, req.lang, &memory);
        drop(runs);
        drop(memory);
        report
    }

    pub fn run(&self, req: &RunRequest, cancel: &AtomicBool) -> Report {
        let started = Instant::now();
        let params = match self.request_params(req) {
            Ok(p) => p,
            Err((err, later)) => return Report::failed(started, err, later),
        };
        let mode = RunMode {
            lang: req.lang.syn(),
            inter: req.mode == Mode::Inter,
        };
        let mut runs = match run_text_cancel(&self.base, &req.text, &params, mode, cancel) {
            Ok(r) => r,
            Err(f) => return self.failure(started, f, cancel),
        };
        if req.mode == Mode::One {
            let last = runs.pop();
            runs = last.into_iter().collect();
        }
        self.report(started, &runs, req.lang)
    }

    pub(crate) fn failure(&self, started: Instant, f: RunFailure, cancel: &AtomicBool) -> Report {
        if let Some(w) = f.later {
            return Report::failed(started, ErrorOut::new(Phase::Later, later_text(w)), Some(w));
        }
        let interrupted = f.chain.iter().any(|c| c.contains(INTERRUPTED));
        if interrupted && cancel.load(Ordering::Relaxed) {
            let mut r = Report::failed(
                started,
                ErrorOut::new(Phase::Runtime, "Выполнение прервано"),
                None,
            );
            r.cancelled = true;
            return r;
        }
        let compile = f.at.is_some() || f.chain.first().is_some_and(|c| c.starts_with("{("));
        let err = ErrorOut {
            phase: if compile {
                Phase::Compile
            } else {
                Phase::Runtime
            },
            chain: f.chain,
            at: f.at,
        };
        Report::failed(started, err, None)
    }

    fn report(&self, started: Instant, runs: &[StmtRun], lang: Lang) -> Report {
        let base = &*self.base;
        let mut statements = Vec::with_capacity(runs.len());
        let mut data = Vec::with_capacity(runs.len());
        let mut budget = MAX_BYTES;
        for run in runs {
            let columns = columns_of(&run.result.columns, base, lang);
            let n = columns.len();
            let total = run.result.rows.len();
            let mut rows: Vec<Vec<Cell>> = Vec::new();
            let mut truncated = false;
            let mut nested = std::collections::HashMap::new();
            for r in &run.result.rows {
                if rows.len() >= MAX_ROWS || budget == 0 {
                    truncated = true;
                    break;
                }
                let row: Vec<Cell> = r
                    .iter()
                    .take(n)
                    .map(|v| cell(v, base, lang.syn()))
                    .collect();
                budget = budget.saturating_sub(row_bytes(&row));
                for (j, v) in r.iter().take(n).enumerate() {
                    if let QValue::Nested(t) = v {
                        let out = nested_out(t, base, lang.syn());
                        let bytes: usize = out.rows.iter().map(|x| row_bytes(x)).sum();
                        budget = budget.saturating_sub(bytes);
                        nested.insert((rows.len(), j), out);
                    }
                }
                rows.push(row);
            }
            let total = u64::try_from(total).unwrap_or(u64::MAX);
            statements.push(stmt_of(
                run,
                &columns,
                total,
                rows.len(),
                truncated,
                rows.first(),
            ));
            data.push(crate::run::StmtRows {
                columns,
                ordered: run.ordered,
                rows,
                nested,
            });
        }
        Report {
            statements,
            elapsed_ms: elapsed_ms(started),
            error: None,
            rows: Rows { stmts: data },
            later: None,
            cancelled: false,
            retained: None,
        }
    }

    fn report_budgeted(
        &self,
        started: Instant,
        runs: &[StmtRun],
        lang: Lang,
        memory: &BatchMemory,
    ) -> Result<Report, RunRefusal> {
        let base = &*self.base;
        let evictor = memory.evictor();
        let schema_bytes = runs
            .iter()
            .map(|run| schema_bytes(&run.result.columns, base, lang, &memory.memory.budget))
            .try_fold(0u64, |sum, b| b.map(|b| sum + b))
            .map_err(|e| match e {
                BudgetError::Limit(m) => RunRefusal::Result(m),
                _ => RunRefusal::Allocation,
            })?;
        let lease = memory
            .memory
            .budget
            .try_reserve_or_evict(Category::Result, schema_bytes, evictor)
            .map_err(RunRefusal::Result)?;
        let columns: Vec<Vec<Column>> = runs
            .iter()
            .map(|run| columns_of(&run.result.columns, base, lang))
            .collect();
        debug_assert_eq!(
            schema_bytes,
            columns
                .iter()
                .map(|cs| 2 * (24 + column_model(cs)))
                .sum::<u64>()
        );
        let mut statements = Vec::with_capacity(runs.len());
        let mut data = Vec::with_capacity(runs.len());
        let mut sizes = RowSizes::new(&memory.memory.budget);
        let mut nested_sizes = RowSizes::new(&memory.memory.budget);
        for (run, columns) in runs.iter().zip(columns) {
            let n = columns.len();
            let total = run.result.rows.len();
            let mut rows: Vec<Vec<Cell>> = Vec::new();
            let mut truncated = false;
            let mut nested = std::collections::HashMap::new();
            for r in &run.result.rows {
                if rows.len() >= MAX_ROWS {
                    truncated = true;
                    break;
                }
                let kept = match row_budgeted(
                    r,
                    n,
                    &rows,
                    base,
                    lang.syn(),
                    &lease,
                    evictor,
                    &mut sizes,
                    &mut nested_sizes,
                ) {
                    Ok(kept) => kept,
                    Err(BudgetError::Limit(m)) if m.reason == LimitReason::Category => {
                        truncated = true;
                        break;
                    }
                    Err(BudgetError::Limit(m)) => return Err(RunRefusal::Result(m)),
                    Err(_) => return Err(RunRefusal::Allocation),
                };
                let (row, tables) = kept;
                for (j, t) in tables {
                    nested.insert((rows.len(), j), t);
                }
                rows.push(row);
            }
            let total = u64::try_from(total).unwrap_or(u64::MAX);
            statements.push(stmt_of(
                run,
                &columns,
                total,
                rows.len(),
                truncated,
                rows.first(),
            ));
            data.push(crate::run::StmtRows {
                columns,
                ordered: run.ordered,
                rows,
                nested,
            });
        }
        let retained = lease
            .try_into_shared()
            .map_err(|_| RunRefusal::Allocation)?;
        Ok(Report {
            statements,
            elapsed_ms: elapsed_ms(started),
            error: None,
            rows: Rows { stmts: data },
            later: None,
            cancelled: false,
            retained: Some(retained),
        })
    }
}

fn schema_bytes(
    columns: &[query_ir::ColumnInfo],
    base: &TestBase,
    lang: Lang,
    budget: &memory_budget::MemoryBudget,
) -> Result<u64, BudgetError> {
    let schema = base.schema();
    let mut texts = 0u64;
    for c in columns {
        let mut count = memory_budget::TextCount::new(Category::Execution, u64::MAX);
        query_testkit::repr::write_type_repr_budgeted(
            schema,
            &c.ty,
            lang.syn(),
            budget,
            Category::Execution,
            &mut count,
            true,
        )?;
        texts += block(c.name.len() as u64) + block(count.len());
    }
    Ok(2 * (24
        + block((columns.len() as u64).saturating_mul(std::mem::size_of::<Column>() as u64))
        + texts))
}

fn columns_of(columns: &[query_ir::ColumnInfo], base: &TestBase, lang: Lang) -> Vec<Column> {
    let schema = base.schema();
    columns
        .iter()
        .map(|c| Column {
            name: c.name.clone(),
            ty: query_testkit::repr::type_repr(schema, &c.ty, lang.syn()),
            width: c.width,
            kind: if c.nested.is_some() {
                CellKind::Nested
            } else {
                column_kind(&c.ty)
            },
        })
        .collect()
}

fn stmt_of(
    run: &StmtRun,
    columns: &[Column],
    total: u64,
    kept: usize,
    truncated: bool,
    first: Option<&Vec<Cell>>,
) -> Stmt {
    if run.dropped {
        return Stmt::Dropped;
    }
    if let Some(name) = &run.put {
        return Stmt::Count {
            name: name.clone(),
            n: put_count(first.and_then(|r| r.first())),
        };
    }
    Stmt::Result {
        columns: columns.to_vec(),
        total,
        kept: u64::try_from(kept).unwrap_or(u64::MAX),
        truncated,
        ordered: run.ordered,
    }
}

const fn block(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        n.div_ceil(16).saturating_mul(16).saturating_add(16)
    }
}

fn push_growth<T>(
    rows: &[T],
    lease: &Reservation,
    evictor: &dyn Evictor,
) -> Result<(), BudgetError> {
    let cap = rows.len();
    if cap == 0 || !cap.is_power_of_two() {
        return Ok(());
    }
    let grow = block(
        (cap as u64)
            .saturating_mul(2)
            .saturating_mul(std::mem::size_of::<T>() as u64),
    );
    lease.try_grow_or_evict(grow, evictor)?;
    Ok(())
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn row_budgeted(
    r: &[QValue],
    n: usize,
    rows: &[Vec<Cell>],
    base: &TestBase,
    lang: SynLang,
    lease: &Reservation,
    evictor: &dyn Evictor,
    sizes: &mut RowSizes,
    nested_sizes: &mut RowSizes,
) -> Result<(Vec<Cell>, Vec<(usize, crate::refs::NestedOut)>), BudgetError> {
    #[cfg(test)]
    if crate::cell::reference::per_cell() {
        return row_budgeted_per_cell(r, n, rows, base, lang, lease, evictor);
    }
    push_growth(rows, lease, evictor)?;
    let values = r.get(..n).unwrap_or(r);
    let row_block = block((n as u64).saturating_mul(std::mem::size_of::<Cell>() as u64));
    let charge = row_block.saturating_add(sizes.measure(values, base, lang)?);
    let before = lease.bytes();
    lease.try_grow_or_evict(charge, evictor)?;
    let mut row = Vec::with_capacity(n);
    let mut tables = Vec::new();
    let mut kept = || -> Result<(), BudgetError> {
        let budget = lease.budget();
        for (j, (v, &len)) in values.iter().zip(sizes.get()).enumerate() {
            row.push(cell_write(v, base, lang, budget, len)?);
            if let QValue::Nested(t) = v {
                tables.push((
                    j,
                    nested_out_budgeted(t, base, lang, lease, evictor, nested_sizes)?,
                ));
            }
        }
        Ok(())
    };
    match kept() {
        Ok(()) => Ok((row, tables)),
        Err(e) => {
            drop(row);
            drop(tables);
            let _ = lease.shrink(lease.bytes().saturating_sub(before));
            Err(e)
        }
    }
}

fn nested_out_budgeted(
    t: &query_ir::NestedResult,
    base: &TestBase,
    lang: SynLang,
    lease: &Reservation,
    evictor: &dyn Evictor,
    sizes: &mut RowSizes,
) -> Result<crate::refs::NestedOut, BudgetError> {
    let columns = nested_columns(t, base, lang);
    lease.try_grow_or_evict(24 + column_model(&columns), evictor)?;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for r in t.rows.iter().take(MAX_NESTED_ROWS) {
        push_growth(&rows, lease, evictor)?;
        let row_block = block((r.len() as u64).saturating_mul(std::mem::size_of::<Cell>() as u64));
        lease.try_grow_or_evict(
            row_block.saturating_add(sizes.measure(r, base, lang)?),
            evictor,
        )?;
        let mut row = Vec::with_capacity(r.len());
        row_write(r, sizes.get(), base, lang, lease.budget(), &mut row)?;
        rows.push(row);
    }
    Ok(crate::refs::NestedOut { columns, rows })
}

fn nested_columns(t: &query_ir::NestedResult, base: &TestBase, lang: SynLang) -> Vec<Column> {
    let schema = base.schema();
    t.columns
        .iter()
        .map(|c| Column {
            name: c.name.clone(),
            ty: query_testkit::repr::type_repr(schema, &c.ty, lang),
            width: c.width,
            kind: if c.nested.is_some() {
                CellKind::Nested
            } else {
                column_kind(&c.ty)
            },
        })
        .collect()
}

fn nested_out(
    t: &query_ir::NestedResult,
    base: &TestBase,
    lang: SynLang,
) -> crate::refs::NestedOut {
    let columns = nested_columns(t, base, lang);
    let rows = t
        .rows
        .iter()
        .take(MAX_NESTED_ROWS)
        .map(|r| r.iter().map(|v| cell(v, base, lang)).collect())
        .collect();
    crate::refs::NestedOut { columns, rows }
}

pub(crate) fn param_error_text(name: &str, e: &query_testhost::ParamError) -> String {
    let what = match e {
        query_testhost::ParamError::Unsupported(w) => later_text(w),
        query_testhost::ParamError::Bad(m) => format!("недопустимое значение: {m}"),
    };
    if name.is_empty() {
        format!("параметры: {what}")
    } else {
        format!("параметр {name}: {what}")
    }
}

fn put_count(c: Option<&Cell>) -> u64 {
    c.and_then(|c| c.t.parse::<u64>().ok()).unwrap_or(0)
}
