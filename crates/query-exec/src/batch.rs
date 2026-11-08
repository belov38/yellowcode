use std::cell::Cell;
use std::sync::Arc;

use crate::temp::provenance::OriginAdapter;
use memory_budget::{Evictor, Reservation, StructureLimit};
use query_ir::bind::ParamValuesSnapshot;
use query_ir::bind::temp_provenance::{BoundTempStatement, TempOriginProvider};
use query_ir::bind::{bind_with_policy_charge_with_admission, preflight_virtual_with_admission};
use query_ir::ir::{BuildError, Rel, build_stmt};
use query_ir::tir::{From as TFrom, Query, Select, StmtKind, TExpr, TKind};
use query_ir::{
    AnalyzeMode, AnalyzeOptions, BindCtx, CompileError, CompileRefusal, CompiledStmt, ExecError,
    ParamInfo, ParamTypes, ParamValue, ParamValues, QValue, TempOut,
};
use query_syntax::{ParseMode, Preflight, PublicParseLimit};

use crate::exec::execute_temps;
use crate::host::ExecHost;
use crate::ops::{MemBudget, present, row_bytes};
use crate::plan::explain_temps;
use crate::result::QueryResult;
use crate::temp::TempTables;

pub trait ParamSource: ParamValues + ParamTypes {}

impl<T: ParamValues + ParamTypes + ?Sized> ParamSource for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Batch,
    One,
    Intermediate,
}

#[derive(Clone, Debug)]
pub enum StmtOutcome {
    Result(Arc<QueryResult>),
    Count(u64),
    Dropped,
}

#[derive(Debug)]
pub enum BatchError {
    Compile(CompileError),
    Exec(ExecError),
}

impl BatchError {
    fn retains_compile_storage(&self) -> bool {
        !matches!(
            self,
            Self::Exec(
                ExecError::Memory(_)
                    | ExecError::Structure(_)
                    | ExecError::Later { .. }
                    | ExecError::Native(_)
                    | ExecError::Rights(_)
                    | ExecError::Fatal(_)
            )
        )
    }
}

#[derive(Clone, Debug)]
pub struct StmtRun {
    pub outcome: StmtOutcome,
    pub ordered: bool,
    pub put: Option<String>,
}

pub fn run_batch(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
) -> Result<Vec<StmtOutcome>, BatchError> {
    Ok(run_batch_detailed(host, text, params, temp, mode)?
        .into_iter()
        .map(|r| r.outcome)
        .collect())
}

pub fn run_batch_detailed(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
) -> Result<Vec<StmtRun>, BatchError> {
    run_batch_inner(host, text, params, temp, mode, None, None)
}

pub struct BatchMem<'a> {
    pub compile: &'a Reservation,
    pub evictor: &'a dyn Evictor,
    pub max_ref_fanout: u32,
    pub max_predefined_bytes: u64,
    pub max_table_width: u32,
    pub model: CompileModel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileModel {
    pub fixed_parse: u64,
    pub c_tok: u64,
    pub c_text: u64,
    pub c_stmt: u64,
    pub fixed_sema: u64,
    pub c_sema: u64,
    pub c_sema_type: u64,
    pub c_dot: u64,
    pub c_deref: u64,
    pub c_star: u64,
    pub c_temp: u64,
    pub fixed_build: u64,
    pub c_build: u64,
    pub c_bfan: u64,
    pub c_bcol: u64,
    pub c_bjoin: u64,
    pub c_btype: u64,
    pub fixed_lower: u64,
    pub c_lower: u64,
    pub c_lfan: u64,
    pub c_plan: u64,
    pub c_pre_names: u64,
    pub c_ltype: u64,
}

impl CompileModel {
    pub const DEFAULT: CompileModel = CompileModel {
        fixed_parse: 32 << 10,
        c_tok: 320,
        c_text: 8,
        c_stmt: 512,
        fixed_sema: 80 << 10,
        c_sema: 768,
        c_sema_type: 5,
        c_dot: 96,
        c_deref: 16,
        c_star: 512,
        c_temp: 1024,
        fixed_build: 32 << 10,
        c_build: 560,
        c_bfan: 96,
        c_bcol: 320,
        c_bjoin: 1024,
        c_btype: 16,
        fixed_lower: 24 << 10,
        c_lower: 768,
        c_lfan: 2560,
        c_plan: 1024,
        c_pre_names: 2,
        c_ltype: 16,
    };

    pub fn parse(&self, pre: &Preflight, text_bytes: usize) -> u64 {
        self.fixed_parse
            + self.c_tok * u64::from(pre.tokens)
            + self.c_text * text_bytes as u64
            + self.c_stmt * u64::from(pre.statements)
    }

    pub fn sema(
        &self,
        pre: &Preflight,
        i: usize,
        max_ref_fanout: u32,
        max_table_width: u32,
        temps: usize,
    ) -> u64 {
        let tokens = pre.tokens_per_statement.get(i).copied().unwrap_or(0);
        let dots = pre.dots_per_statement.get(i).copied().unwrap_or(0);
        let chained = pre.chained_dots_per_statement.get(i).copied().unwrap_or(0);
        let stars = pre.stars_per_statement.get(i).copied().unwrap_or(0);
        let staging = u64::from(tokens).saturating_mul(
            (6 * std::mem::size_of::<query_ir::tir::DrCrArgDependency>()
                + 16 * std::mem::size_of::<usize>()) as u64,
        );
        let targets = u64::from(stars)
            .saturating_mul(u64::from(max_table_width.max(1)))
            .saturating_mul(
                (2 * std::mem::size_of::<query_ir::tir::DrCrWildcardTarget>()
                    + 4 * std::mem::size_of::<query_ir::QFieldId>()) as u64,
            );
        let ordinary = self.fixed_sema
            + self.c_sema * u64::from(tokens)
            + self.c_sema_type * u64::from(tokens) * u64::from(max_ref_fanout).div_ceil(8)
            + self.c_dot * u64::from(dots)
            + self.c_deref * u64::from(chained) * u64::from(max_ref_fanout.max(1))
            + self.c_star * u64::from(stars) * u64::from(max_table_width.max(1))
            + self.c_temp * temps as u64;
        ordinary.saturating_add(staging).saturating_add(targets)
    }

    pub fn build(&self, st: &query_ir::sema::stats::StmtStats) -> u64 {
        let ordinary = self.fixed_build
            + self.c_build * st.nodes
            + self.c_bfan * st.fanout_sum
            + self.c_bcol * st.columns
            + self.c_bjoin * (st.joins + st.subqueries)
            + self.c_btype * st.type_refs;
        ordinary
            .saturating_add(
                st.drcr_dependencies
                    .saturating_add(st.accounting_dependencies)
                    .saturating_mul(
                        2 * std::mem::size_of::<query_ir::tir::DrCrArgDependency>() as u64,
                    ),
            )
            .saturating_add(
                st.drcr_wildcard_targets
                    .saturating_add(st.accounting_wildcard_targets)
                    .saturating_mul(
                        2 * std::mem::size_of::<query_ir::tir::DrCrWildcardTarget>() as u64,
                    ),
            )
            .saturating_add(
                st.drcr_wildcard_fields
                    .saturating_add(st.accounting_wildcard_fields)
                    .saturating_mul(2 * std::mem::size_of::<query_ir::QFieldId>() as u64),
            )
            .saturating_add(
                st.drcr_nested_nodes
                    .saturating_add(st.accounting_nested_nodes)
                    .saturating_mul(std::mem::size_of::<query_ir::tir::DrCrNested>() as u64),
            )
            .saturating_add(st.accounting_sources.saturating_mul(
                2 * std::mem::size_of::<query_ir::tir::AccountingDependencies>() as u64,
            ))
            .saturating_add(st.accounting_slots.saturating_mul(2))
    }

    pub fn lower(&self, ir: &query_ir::ir::stats::IrStats, max_predefined_bytes: u64) -> u64 {
        self.fixed_lower
            + self.c_lower * (ir.rels + ir.exprs)
            + self.c_lfan * ir.fanout_sum
            + self.c_plan * ir.rels
            + self.c_pre_names * ir.tables * max_predefined_bytes
            + self.c_ltype * ir.type_refs
    }

    pub fn policy(
        &self,
        stats: &query_ir::bind::PolicyCompileStats,
        max_predefined_bytes: u64,
    ) -> u64 {
        let p = &stats.predicates;
        let policy_ir = query_ir::ir::stats::IrStats {
            rels: 4 * (p.sources + p.joins + p.subqueries + 1),
            exprs: 4 * (p.nodes + p.columns),
            fanout_sum: 4 * p.fanout_sum,
            tables: p.sources,
            type_refs: 4 * p.type_refs,
            ..Default::default()
        };
        self.lower(&stats.statement, max_predefined_bytes)
            .saturating_add(2 * self.build(p))
            .saturating_add(self.lower(&policy_ir, max_predefined_bytes))
            .saturating_add(stats.payload_bytes)
    }
}

pub const PUBLIC_MAX_COLUMNS: u64 = 4096;

fn structure(limit: PublicParseLimit) -> BatchError {
    exec(ExecError::Structure(match limit {
        PublicParseLimit::Statements => StructureLimit::Statements,
        PublicParseLimit::Columns => StructureLimit::Columns,
        PublicParseLimit::Depth => StructureLimit::Depth,
    }))
}

impl BatchMem<'_> {
    fn grow(&self, bytes: u64) -> Result<(), BatchError> {
        self.compile
            .try_grow_or_evict(bytes, self.evictor)
            .map_err(|m| exec(ExecError::Memory(m)))
    }

    fn release(&self, bytes: u64) {
        let _ = self.compile.shrink(bytes);
    }
}

struct CompileCall<'m, 'a> {
    mem: &'m BatchMem<'a>,
    charged: Cell<u64>,
}

impl<'a> std::ops::Deref for CompileCall<'_, 'a> {
    type Target = BatchMem<'a>;

    fn deref(&self) -> &Self::Target {
        self.mem
    }
}

impl CompileCall<'_, '_> {
    fn grow(&self, bytes: u64) -> Result<(), BatchError> {
        let next = crate::model::size_add(self.charged.get(), bytes).map_err(exec)?;
        self.mem.grow(bytes)?;
        self.charged.set(next);
        Ok(())
    }

    fn release(&self, bytes: u64) {
        self.charged.set(self.charged.get() - bytes);
        self.mem.release(bytes);
    }
    fn release_remaining(&self) {
        self.mem.release(self.charged.replace(0));
    }
    fn hand_off_diagnostic(&self) {
        self.charged.set(0);
    }
}
impl Drop for CompileCall<'_, '_> {
    fn drop(&mut self) {
        self.release_remaining();
    }
}

pub fn run_batch_budgeted(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
    mem: &BatchMem<'_>,
) -> Result<Vec<StmtRun>, BatchError> {
    run_batch_inner(host, text, params, temp, mode, Some(mem), None)
}

struct SnapshotCharges<'c, 'm, 'a> {
    call: &'c CompileCall<'m, 'a>,
    retained: Cell<u64>,
    copies: Cell<u64>,
}

impl<'c, 'm, 'a> SnapshotCharges<'c, 'm, 'a> {
    fn new(call: &'c CompileCall<'m, 'a>) -> Self {
        Self {
            call,
            retained: Cell::new(0),
            copies: Cell::new(0),
        }
    }

    fn admit(
        &self,
        name: &str,
        value: Option<&ParamValue>,
        new_entry: bool,
    ) -> Result<(), ExecError> {
        let (retained, copies) = parameter_snapshot_bytes(name, value)?;
        let retained = if new_entry { retained } else { 0 };
        let next_retained = crate::model::size_add(self.retained.get(), retained)?;
        let next_copies = crate::model::size_add(self.copies.get(), copies)?;
        let bytes = crate::model::size_add(retained, copies)?;
        self.call.grow(bytes).map_err(|error| match error {
            BatchError::Exec(error) => error,
            _ => ExecError::Fatal("parameter snapshot admission failed".into()),
        })?;
        self.retained.set(next_retained);
        self.copies.set(next_copies);
        Ok(())
    }

    fn statement(&self) -> StatementCopiesGuard<'_, 'c, 'm, 'a> {
        StatementCopiesGuard(self)
    }

    fn release_copies(&self) {
        self.call.release(self.copies.replace(0));
    }
}

impl Drop for SnapshotCharges<'_, '_, '_> {
    fn drop(&mut self) {
        self.release_copies();
        self.call.release(self.retained.replace(0));
    }
}

struct StatementCopiesGuard<'s, 'c, 'm, 'a>(&'s SnapshotCharges<'c, 'm, 'a>);

impl Drop for StatementCopiesGuard<'_, '_, '_, '_> {
    fn drop(&mut self) {
        self.0.release_copies();
    }
}

fn parameter_snapshot_bytes(
    name: &str,
    value: Option<&ParamValue>,
) -> Result<(u64, u64), ExecError> {
    use crate::model::{block, size_add, type_model, value_model, vec_block};

    let payload = match value {
        None => 0,
        Some(ParamValue::Value(value)) => value_model(value),
        Some(ParamValue::List(values)) => values.iter().try_fold(
            vec_block(values.capacity(), std::mem::size_of::<QValue>()),
            |bytes, value| size_add(bytes, value_model(value)),
        )?,
        Some(ParamValue::Table { columns, rows }) => {
            let mut bytes = size_add(
                vec_block(
                    columns.capacity(),
                    std::mem::size_of::<query_ir::TempColumn>(),
                ),
                vec_block(rows.capacity(), std::mem::size_of::<Vec<QValue>>()),
            )?;
            for column in columns {
                bytes = size_add(bytes, block(column.name.capacity() as u64))?;
                if let Some(ty) = &column.ty {
                    bytes = size_add(bytes, type_model(ty))?;
                }
            }
            for row in rows {
                bytes = size_add(
                    bytes,
                    vec_block(row.capacity(), std::mem::size_of::<QValue>()),
                )?;
                for value in row {
                    bytes = size_add(bytes, value_model(value))?;
                }
            }
            bytes
        }
    };
    let entry =
        block(8 * (std::mem::size_of::<(String, Option<Arc<ParamValue>>)>() as u64 + 1) + 128)
            .saturating_add(block(16 + std::mem::size_of::<ParamValue>() as u64));
    let names = block((name.len() as u64).saturating_mul(3).saturating_add(16));
    let metadata = size_add(entry, names)?;
    Ok((
        size_add(metadata, payload)?,
        size_add(metadata, payload.saturating_mul(3))?,
    ))
}

fn run_batch_inner(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
    mem: Option<&BatchMem<'_>>,
    observations: Option<&mut Vec<crate::observe::ObservationSession>>,
) -> Result<Vec<StmtRun>, BatchError> {
    forget(host, temp);
    let call = mem.map(|mem| CompileCall {
        mem,
        charged: Cell::new(0),
    });
    let r = {
        let charges = call.as_ref().map(SnapshotCharges::new);
        let charge = |name: &str, value: Option<&ParamValue>, new_entry: bool| {
            if let Some(charges) = &charges {
                charges.admit(name, value, new_entry)?;
            }
            Ok(())
        };
        let values = if call.is_some() {
            ParamValuesSnapshot::with_charge(params, &charge)
        } else {
            ParamValuesSnapshot::new(params)
        };
        run_batch_leased(
            host,
            text,
            params,
            &values,
            temp,
            mode,
            call.as_ref(),
            charges.as_ref(),
            observations,
        )
    };
    if let Some(call) = &call {
        if r.as_ref()
            .is_err_and(|error| error.retains_compile_storage())
        {
            call.hand_off_diagnostic();
        } else {
            call.release_remaining();
        }
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn run_batch_leased(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    values: &ParamValuesSnapshot<'_>,
    temp: &mut TempTables,
    mode: RunMode,
    mem: Option<&CompileCall<'_, '_>>,
    snapshot_charges: Option<&SnapshotCharges<'_, '_, '_>>,
    mut observations: Option<&mut Vec<crate::observe::ObservationSession>>,
) -> Result<Vec<StmtRun>, BatchError> {
    let (batch, stages) = match mem {
        None => (
            query_ir::compile_batch(text, ParseMode::Query, host.schema())
                .map_err(BatchError::Compile)?,
            None,
        ),
        Some(mem) => {
            let pre = query_syntax::public_preflight(text).map_err(structure)?;
            mem.grow(mem.model.parse(&pre, text.len()))?;
            let batch = match query_ir::compile_batch_public(text, ParseMode::Query, host.schema())
            {
                Err(CompileRefusal::Structure(l)) => return Err(structure(l)),
                Ok(r) => r.map_err(BatchError::Compile)?,
            };
            (batch, Some((mem, Box::new(pre))))
        }
    };
    host.admit_batch(&batch, temp).map_err(exec)?;
    let capacity = if mode == RunMode::One {
        1
    } else {
        batch.stmts.len()
    };
    let metadata_env = host.env();
    let metadata = mem.map(|_| MemBudget::of_env(&metadata_env));
    if let Some(metadata) = &metadata {
        metadata
            .grow(crate::model::size_vec(capacity, std::mem::size_of::<StmtRun>()).map_err(exec)?)
            .map_err(exec)?;
    }
    let mut out: Vec<StmtRun> = Vec::with_capacity(capacity);
    for i in 0..batch.stmts.len() {
        crate::probe::hit();
        let observation = observations.as_deref_mut().and_then(|sessions| {
            sessions.push(crate::observe::ObservationSession::new());
            sessions.last()
        });
        host.check_txn().map_err(|e| {
            if let Some(obs) = observation {
                obs.finish(false);
            }
            exec(ExecError::Host(e))
        })?;
        let stage = stages.as_ref().map(|(mem, pre)| Stage { mem, pre, i });
        match step(
            host,
            &batch,
            text,
            i,
            params,
            values,
            temp,
            mode,
            stage.as_ref(),
            snapshot_charges,
            metadata.as_ref(),
            observation,
        ) {
            Ok(r) => {
                if mode == RunMode::One {
                    let names = out
                        .iter()
                        .try_fold(0, |bytes, previous| {
                            crate::model::size_add(
                                bytes,
                                previous.put.as_ref().map_or(Ok(0), |name| {
                                    crate::model::size_block(name.capacity() as u64)
                                })?,
                            )
                        })
                        .map_err(exec)?;
                    out.clear();
                    if let Some(metadata) = &metadata {
                        metadata.shrink(names);
                    }
                }
                out.push(r);
            }
            Err(Failure {
                err: BatchError::Exec(e),
                compile_stage,
            }) => {
                if !matches!(e, ExecError::Later { .. }) && !compile_stage {
                    host.runtime_failed();
                }
                return Err(BatchError::Exec(e));
            }
            Err(Failure { err, .. }) => return Err(err),
        }
    }
    if let Some(metadata) = &metadata {
        metadata.reconcile(metadata.used());
    }
    Ok(out)
}

struct Failure {
    err: BatchError,
    compile_stage: bool,
}

impl From<BatchError> for Failure {
    fn from(err: BatchError) -> Failure {
        Failure {
            err,
            compile_stage: false,
        }
    }
}

fn compile_stage(err: BatchError) -> Failure {
    Failure {
        err,
        compile_stage: true,
    }
}

fn forget(host: &dyn ExecHost, temp: &mut TempTables) {
    temp.forget_rolled_back(&|t| host.txn_rolled_back(t));
}

fn exec(e: ExecError) -> BatchError {
    BatchError::Exec(e)
}

fn build_error(e: BuildError) -> BatchError {
    match e {
        BuildError::Later { what, stage } => exec(ExecError::Later { what, stage }),
        BuildError::Internal(s) => exec(ExecError::Fatal(s)),
        BuildError::Compile(e) => BatchError::Compile(e),
    }
}

struct Stage<'s, 'a> {
    mem: &'s CompileCall<'s, 'a>,
    pre: &'s Preflight,
    i: usize,
}

struct PolicyInputsGuard<'c, 'm, 'a> {
    call: Option<&'c CompileCall<'m, 'a>>,
    bytes: Cell<u64>,
}
impl Drop for PolicyInputsGuard<'_, '_, '_> {
    fn drop(&mut self) {
        if let Some(call) = self.call {
            call.release(self.bytes.get());
        }
    }
}

struct OriginCompileOwner<'c, 'm, 'a, 'e> {
    call: Option<&'c CompileCall<'m, 'a>>,
    lease: Option<&'e Reservation>,
    evictor: Option<&'e dyn Evictor>,
    limit: u64,
    bytes: Cell<u64>,
}
impl OriginCompileOwner<'_, '_, '_, '_> {
    fn admit(&self, bytes: u64) -> Result<(), ExecError> {
        let next = crate::model::size_add(self.bytes.get(), bytes)?;
        if let Some(call) = self.call {
            call.grow(bytes).map_err(|e| match e {
                BatchError::Exec(e) => e,
                _ => ExecError::Fatal("origin admission failed".into()),
            })?;
        } else if let Some(lease) = self.lease {
            match self.evictor {
                Some(e) => lease.try_grow_or_evict(bytes, e),
                None => lease.try_grow(bytes),
            }
            .map_err(ExecError::Memory)?;
        } else if next > self.limit {
            return Err(ExecError::Fatal("origin compile memory limit".into()));
        }
        self.bytes.set(next);
        Ok(())
    }
}
impl Drop for OriginCompileOwner<'_, '_, '_, '_> {
    fn drop(&mut self) {
        let bytes = self.bytes.replace(0);
        if let Some(call) = self.call {
            call.release(bytes);
        } else if let Some(lease) = self.lease
            && bytes != 0
        {
            let _ = lease.shrink(bytes);
        }
    }
}

struct PendingResult<'a> {
    result: Option<QueryResult>,
    lease: Option<&'a Reservation>,
    charge: u64,
}
impl Drop for PendingResult<'_> {
    fn drop(&mut self) {
        drop(self.result.take());
        if let Some(lease) = self.lease
            && self.charge != 0
        {
            let _ = lease.shrink(self.charge);
        }
    }
}

struct PendingTempWrite<'a> {
    rows: Option<Vec<crate::Row>>,
    shown: Option<Vec<crate::Row>>,
    lease: Option<&'a Reservation>,
    rows_charge: u64,
    shown_charge: u64,
}
impl Drop for PendingTempWrite<'_> {
    fn drop(&mut self) {
        drop(self.rows.take());
        drop(self.shown.take());
        if let Some(lease) = self.lease {
            let _ = lease.shrink(self.rows_charge + self.shown_charge);
        }
    }
}

struct Prepared {
    compiled: Arc<CompiledStmt>,
    ordered: bool,
    lowered: Option<BoundTempStatement>,
}

fn drcr_lower_bytes(
    stmt: &query_ir::ir::StmtS,
    schema: &query_ir::QuerySchema,
    stats: &query_ir::ir::stats::IrStats,
) -> u64 {
    let sources=stmt.sources.iter().filter(|s|matches!(&s.kind,query_ir::ir::SrcKind::Virtual {table,..} if schema.table(*table).kind==query_ir::schema::TableKind::Virtual(query_ir::VtKind::DrCrTurnovers))).count() as u64;
    if sources == 0 {
        return 0;
    }
    let entries = stats.exprs.saturating_mul(
        3 * (std::mem::size_of::<query_ir::lower::DrCrResourceL>()
            + std::mem::size_of::<query_ir::lower::DrCrSideL>()) as u64,
    );
    let area_snapshot = sources.saturating_mul(
        4 * (std::mem::size_of::<(
            query_ir::QTableId,
            (ibschema::storage::FieldId, ibschema::storage::FieldId, i64),
        )>() as u64
            + 1)
            + 64,
    );
    entries.saturating_add(area_snapshot).saturating_add(
        sources
            .saturating_mul(3 * std::mem::size_of::<query_ir::lower::RegisterRowsL>() as u64 + 96),
    )
}

fn turnovers_lower_bytes(
    stmt: &query_ir::ir::StmtS,
    schema: &query_ir::QuerySchema,
    stats: &query_ir::ir::stats::IrStats,
) -> u64 {
    let mut sources = 0u64;
    let mut columns = 0u64;
    for source in &stmt.sources {
        if let query_ir::ir::SrcKind::Virtual { table, .. } = &source.kind {
            let table = schema.table(*table);
            if table.kind == query_ir::TableKind::Virtual(query_ir::VtKind::Turnovers)
                && table.owner.is_some_and(|owner| {
                    matches!(schema.table(owner).kind,
                    query_ir::TableKind::Object(kind) if kind.as_str() == "AccountingRegister")
                })
            {
                sources = sources.saturating_add(1);
                columns = columns.saturating_add(table.fields.len() as u64);
            }
        }
    }
    if sources == 0 {
        return 0;
    }
    let entries = stats.exprs.saturating_mul(
        3 * (std::mem::size_of::<query_ir::lower::AccountingTurnoversResourceL>()
            + 3 * std::mem::size_of::<query_ir::lower::AggL>()
            + 6 * std::mem::size_of::<query_ir::PExpr>()) as u64,
    );
    entries
        .saturating_add(columns.saturating_mul(3 * std::mem::size_of::<query_ir::PExpr>() as u64))
        .saturating_add(
            sources.saturating_mul(
                3 * std::mem::size_of::<query_ir::lower::RegisterRowsL>() as u64 + 96,
            ),
        )
}

fn balance_lower_bytes(
    stmt: &query_ir::ir::StmtS,
    schema: &query_ir::QuerySchema,
    stats: &query_ir::ir::stats::IrStats,
) -> u64 {
    let mut sources = 0u64;
    let mut fields = 0u64;
    for source in &stmt.sources {
        let query_ir::ir::SrcKind::Virtual { table, .. } = &source.kind else {
            continue;
        };
        let t = schema.table(*table);
        if t.kind == query_ir::TableKind::Virtual(query_ir::VtKind::Balance)
            && t.owner.is_some_and(|owner| matches!(schema.table(owner).kind, query_ir::TableKind::Object(kind) if kind.as_str() == "AccountingRegister")) {
            sources = sources.saturating_add(1);
            fields = fields.saturating_add(t.fields.len() as u64);
        }
    }
    if sources == 0 {
        return 0;
    }
    fields
        .saturating_mul(3 * std::mem::size_of::<query_ir::PExpr>() as u64)
        .saturating_add(stats.exprs.saturating_mul(
            3 * (std::mem::size_of::<query_ir::lower::BalanceResourceL>()
                + 24 * std::mem::size_of::<query_ir::PExpr>()) as u64,
        ))
        .saturating_add(
            sources.saturating_mul(
                3 * std::mem::size_of::<query_ir::lower::BalanceRowsL>() as u64 + 4096,
            ),
        )
}

fn checked_batch_origins<'a>(
    temp_origins: Option<&'a dyn TempOriginProvider>,
    accounting_memory: Option<&memory_budget::RequestMemory>,
) -> Result<Option<&'a dyn TempOriginProvider>, ExecError> {
    if accounting_memory.is_some() && temp_origins.is_none() {
        return Err(ExecError::Fatal(
            "managed batch: authenticated temp origins required".into(),
        ));
    }
    Ok(temp_origins)
}

pub(crate) fn managed_execution_root(
    selected: &crate::source::ExecEnv<'_>,
    execution: &crate::source::ExecEnv<'_>,
) -> bool {
    selected
        .memory
        .zip(execution.memory)
        .zip(execution.lease)
        .is_some_and(|((selected, current), lease)| {
            selected.row_limit == current.row_limit
                && selected.budget.same_scope(&current.budget)
                && selected.budget.same_scope(lease.budget())
                && lease.category() == memory_budget::Category::Execution
        })
}

pub(crate) fn assert_managed_execution_root(
    selected: &crate::source::ExecEnv<'_>,
    execution: &crate::source::ExecEnv<'_>,
) {
    assert!(
        managed_execution_root(selected, execution),
        "managed request/lease scope invariant"
    );
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    host: &dyn ExecHost,
    batch: &query_syntax::ast::Batch,
    text: &str,
    i: usize,
    params: &dyn ParamSource,
    values: &ParamValuesSnapshot<'_>,
    temp: &TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    leased: &mut u64,
    policy_inputs: &Cell<u64>,
    origin_owner: &OriginCompileOwner<'_, '_, '_, '_>,
    observation: Option<&crate::observe::ObservationSession>,
    accounting_memory: Option<&memory_budget::RequestMemory>,
) -> Result<Prepared, Failure> {
    let schema = host.schema();
    let opts = AnalyzeOptions {
        temp_tables: temp.schemas(),
        params,
        mode: AnalyzeMode::Execute,
        inter: mode == RunMode::Intermediate,
    };
    let mut take = |bytes: u64| -> Result<(), Failure> {
        if let Some(st) = stage {
            st.mem.grow(bytes).map_err(compile_stage)?;
            *leased += bytes;
        }
        Ok(())
    };
    if let Some(st) = stage {
        take(st.mem.model.sema(
            st.pre,
            st.i,
            st.mem.max_ref_fanout,
            st.mem.max_table_width,
            temp.len(),
        ))?;
    }
    let compiled = match query_ir::sema::analyze_stmt_for_execution(batch, text, i, schema, &opts) {
        Ok(compiled) => compiled,
        Err(query_ir::sema::ExecutionAnalysisFailure::Compile(error)) => {
            return Err(BatchError::Compile(error).into());
        }
        Err(query_ir::sema::ExecutionAnalysisFailure::PendingViewFailure { table, original }) => {
            let view = schema.table(table);
            let register = view
                .object
                .or_else(|| view.owner.and_then(|owner| schema.table(owner).object))
                .ok_or_else(|| {
                    compile_stage(exec(ExecError::Fatal(
                        "balance view without register metadata".into(),
                    )))
                })?;
            let src = host.read_source();
            let error = crate::register::resolve_pending_view_failure_with_observation(
                &*src,
                register,
                original,
                observation,
            )
            .map_err(|error| compile_stage(exec(error)))?;
            return Err(BatchError::Compile(error).into());
        }
    };
    if compiled.drop.is_some() {
        return Ok(Prepared {
            compiled,
            ordered: false,
            lowered: None,
        });
    }
    if let Some(st) = stage {
        let stats = query_ir::sema::stats::stats(&compiled);
        if stats.columns > PUBLIC_MAX_COLUMNS {
            return Err(compile_stage(exec(ExecError::Structure(
                StructureLimit::Columns,
            ))));
        }
        take(st.mem.model.build(&stats))?;
    }
    let ctx = BindCtx {
        security: query_ir::host::SecurityCtx {
            access: host.access(),
            session: host.session(),
        },
        schema,
        storage: host.storage(),
        map: host.map(),
        params: values,
        host,
    };
    let adapter = OriginAdapter::new(temp);
    let temp_origins = checked_batch_origins(Some(&adapter), accounting_memory).map_err(exec)?;
    let preflight =
        preflight_virtual_with_admission(&compiled, &ctx, temp_origins, accounting_memory)
            .map_err(exec);
    values.check().map_err(|error| compile_stage(exec(error)))?;
    preflight?;
    let ir = build_stmt(&compiled, schema).map_err(build_error)?;
    if let Some(st) = stage {
        let stats = query_ir::ir::stats::stats(&ir);
        let bytes = st
            .mem
            .model
            .lower(&stats, st.mem.max_predefined_bytes)
            .saturating_add(drcr_lower_bytes(&ir, schema, &stats))
            .saturating_add(turnovers_lower_bytes(&ir, schema, &stats))
            .saturating_add(balance_lower_bytes(&ir, schema, &stats));
        st.mem.grow(bytes)?;
        *leased += bytes;
    }
    let mut policy_charge = |stats: &query_ir::bind::PolicyCompileStats| {
        if let Some(st) = stage {
            let bytes = st.mem.model.policy(stats, st.mem.max_predefined_bytes);
            st.mem.grow(bytes).map_err(|e| match e {
                BatchError::Exec(e) => e,
                _ => ExecError::Fatal("policy lease failed".into()),
            })?;
            policy_inputs.set(crate::model::size_add(
                policy_inputs.get(),
                stats.payload_bytes,
            )?);
            *leased += bytes.saturating_sub(stats.payload_bytes);
        }
        Ok(())
    };
    let mut origin_charge = |bytes| origin_owner.admit(bytes);
    let origins = temp_origins.map(|provider| {
        (
            provider,
            compiled.temp_out.as_ref(),
            &mut origin_charge as &mut dyn FnMut(u64) -> Result<(), ExecError>,
        )
    });
    let lowered = bind_with_policy_charge_with_admission(
        &ir,
        &ctx,
        &mut policy_charge,
        origins,
        accounting_memory,
    )
    .map_err(exec);
    values.check().map_err(exec)?;
    let lowered = lowered?;
    Ok(Prepared {
        compiled,
        ordered: is_ordered(&ir.root),
        lowered: Some(lowered),
    })
}

#[allow(clippy::too_many_arguments)]
fn step(
    host: &dyn ExecHost,
    batch: &query_syntax::ast::Batch,
    text: &str,
    i: usize,
    params: &dyn ParamSource,
    values: &ParamValuesSnapshot<'_>,
    temp: &mut TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    snapshot_charges: Option<&SnapshotCharges<'_, '_, '_>>,
    metadata: Option<&MemBudget<'_>>,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<StmtRun, Failure> {
    forget(host, temp);
    let mut leased = 0u64;
    let r = {
        let _copies = snapshot_charges.map(SnapshotCharges::statement);
        step_prepared(
            host,
            batch,
            text,
            i,
            params,
            values,
            temp,
            mode,
            stage,
            &mut leased,
            metadata,
            observation,
        )
    };
    if let Some(obs) = observation {
        if r.is_ok() {
            obs.finish(true);
        } else if obs.report().closure != crate::observe::ClosureStatus::TerminalPrecheck {
            obs.finish(false);
        }
    }
    if !r
        .as_ref()
        .is_err_and(|failure| failure.err.retains_compile_storage())
        && let Some(st) = stage
    {
        st.mem.release(leased);
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn step_prepared(
    host: &dyn ExecHost,
    batch: &query_syntax::ast::Batch,
    text: &str,
    i: usize,
    params: &dyn ParamSource,
    values: &ParamValuesSnapshot<'_>,
    temp: &mut TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    leased: &mut u64,
    metadata: Option<&MemBudget<'_>>,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<StmtRun, Failure> {
    let env = host.env();
    let origin_owner = OriginCompileOwner {
        call: stage.map(|s| s.mem),
        lease: env.lease,
        evictor: env.evictor,
        limit: env.mem_limit,
        bytes: Cell::new(0),
    };
    let policy_inputs = PolicyInputsGuard {
        call: stage.map(|s| s.mem),
        bytes: Cell::new(0),
    };
    let mut p = prepare(
        host,
        batch,
        text,
        i,
        params,
        values,
        temp,
        mode,
        stage,
        leased,
        &policy_inputs.bytes,
        &origin_owner,
        observation,
        env.memory,
    )?;
    let ordered = p.ordered;
    let done = |outcome, put| {
        Ok(StmtRun {
            outcome,
            ordered,
            put,
        })
    };
    if let Some(name) = &p.compiled.drop {
        temp.drop_table(name);
        return done(StmtOutcome::Dropped, None);
    }
    let Some(bound) = &p.lowered else {
        return Err(exec(ExecError::Fatal("statement without a plan".into())).into());
    };
    let lowered = bound.statement();
    if env.memory.is_some()
        && query_syntax::with_deep_stack(lowered.max_depth, || {
            crate::accounting_inputs::has_balance_columns(&lowered.root)
        })
        .map_err(exec)?
    {
        assert_managed_execution_root(&env, &env);
    }
    let put = p
        .compiled
        .temp_out
        .as_ref()
        .map(|out| {
            let name = match out {
                TempOut::Put(schema) => &schema.name,
                TempOut::Add(name) => name,
            };
            if let Some(metadata) = metadata {
                metadata
                    .grow(crate::model::size_block(name.len() as u64).map_err(exec)?)
                    .map_err(exec)?;
            }
            Ok::<_, Failure>(name.clone())
        })
        .transpose()?;
    take_locks(host, &p.compiled, lowered)?;
    let res = {
        let src = host.read_source();
        let refused = crate::register::check_totals_use_for_execution(
            lowered,
            &*src,
            text,
            observation,
            &env,
        )
        .map_err(exec)?;
        if let Some(e) = refused {
            return Err(BatchError::Compile(e).into());
        }
        let mut presentation_mem = (!lowered.present.is_empty()).then(|| MemBudget::of_env(&env));
        let result = match observation {
            Some(obs) => crate::exec::execute_temps_observed(
                lowered,
                &*src,
                &env,
                Some(&*temp),
                host.plan_options(),
                obs,
            ),
            None => execute_temps(lowered, &*src, &env, Some(&*temp), host.plan_options()),
        }
        .map_err(exec)?;
        let charge = if env.lease.is_some() {
            crate::exec::result_model(&result)
        } else {
            0
        };
        let mut owner = PendingResult {
            result: Some(result),
            lease: env.lease,
            charge,
        };
        let res = owner
            .result
            .as_mut()
            .ok_or_else(|| exec(ExecError::Fatal("missing pending result".into())))?;
        if let Some(mem) = presentation_mem.as_mut() {
            let before = crate::exec::result_model(res);
            if !mem.is_leased()
                && let Err(e) = mem.grow(res.rows.iter().map(|r| row_bytes(r)).sum())
            {
                return Err(exec(e).into());
            }
            if let Err(e) = present::apply(lowered, &mut res.rows, host, &*src, mem) {
                return Err(exec(e).into());
            }
            let after = crate::exec::result_model(res);
            mem.reconcile(after.saturating_sub(before));
            if let Some(l) = env.lease {
                let _ = l.shrink(before.saturating_sub(after));
            }
            owner.charge = if env.lease.is_some() { after } else { 0 };
        }
        let result = owner
            .result
            .take()
            .ok_or_else(|| exec(ExecError::Fatal("missing pending result".into())))?;
        owner.charge = 0;
        result
    };
    let Some(out) = &p.compiled.temp_out else {
        return done(StmtOutcome::Result(Arc::new(res)), None);
    };
    let proof = p.lowered.take().and_then(|bound| bound.into_parts().1);
    let QueryResult { columns, rows, .. } = res;
    let cols = crate::model::columns_model(&columns);
    drop(columns);
    if let Some(lease) = env.lease {
        let _ = lease.shrink(cols);
    }
    let mem = MemBudget::of_env(&env);
    let rows_charge = if env.lease.is_some() {
        crate::model::rows_model(&rows)
    } else {
        0
    };
    let mut pending = PendingTempWrite {
        rows: Some(rows),
        shown: None,
        lease: env.lease,
        rows_charge,
        shown_charge: 0,
    };
    let index_sets = put_rules(&p.compiled);
    if let TempOut::Add(name) = out {
        if let Some(rows) = pending.rows.as_deref_mut() {
            convert_added(rows, name, temp, &env, &mem)?;
        }
        let after = pending.rows.as_ref().map_or(0, |rows| {
            if env.lease.is_some() {
                crate::model::rows_model(rows)
            } else {
                0
            }
        });
        mem.reconcile(after.saturating_sub(pending.rows_charge));
        if let Some(lease) = env.lease {
            let _ = lease.shrink(pending.rows_charge.saturating_sub(after));
        }
        pending.rows_charge = after;
    }
    let intermediate = mode == RunMode::Intermediate;
    let columns_charge = crate::model::columns_model(&p.compiled.columns);
    let shown_charge = if !intermediate || !mem.is_leased() {
        0
    } else {
        match (out, pending.rows.as_ref()) {
            (TempOut::Put(_), Some(rows)) => crate::model::rows_clone_model(rows) + columns_charge,
            (TempOut::Add(name), Some(rows)) => temp.table(name).map_or(0, |table| {
                crate::model::rows_clone_model_of(
                    table.rows.len() + rows.len(),
                    table.rows.iter().chain(rows.iter()),
                ) + columns_charge
            }),
            _ => 0,
        }
    };
    if shown_charge != 0 {
        mem.grow(shown_charge).map_err(exec)?;
    }
    mem.reconcile(mem.used());
    pending.shown_charge = shown_charge;
    if intermediate && matches!(out, TempOut::Put(_)) {
        pending.shown = pending.rows.clone();
    }
    let rows = pending
        .rows
        .take()
        .ok_or_else(|| exec(ExecError::Fatal("missing pending rows".into())))?;
    let n = match out {
        TempOut::Put(schema) => {
            temp.put_with_origins(schema.clone(), rows, host.txn(), &index_sets, &env, proof)
        }
        TempOut::Add(name) => {
            temp.append_with_origins(name, rows, host.txn(), &index_sets, &env, proof)
        }
    }
    .map_err(exec)?;
    pending.rows_charge = 0;
    let outcome = if let (true, TempOut::Add(name)) = (intermediate, out) {
        let table = temp
            .table(name)
            .ok_or_else(|| exec(ExecError::Fatal("the table of ДОБАВИТЬ is gone".into())))?;
        StmtOutcome::Result(Arc::new(QueryResult {
            columns: p.compiled.columns.clone(),
            rows: table.rows.clone(),
            totals: None,
        }))
    } else if let Some(rows) = pending.shown.take() {
        StmtOutcome::Result(Arc::new(QueryResult {
            columns: p.compiled.columns.clone(),
            rows,
            totals: None,
        }))
    } else {
        StmtOutcome::Count(n)
    };
    pending.shown_charge = 0;
    done(outcome, put)
}

fn convert_added(
    rows: &mut [Vec<QValue>],
    name: &str,
    temp: &TempTables,
    env: &crate::source::ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<(), BatchError> {
    let Some(table) = temp.table(name) else {
        return Ok(());
    };
    for (i, col) in table.schema.columns.iter().enumerate() {
        let Some(q) = col
            .stored
            .or_else(|| col.ty.as_ref().and_then(|t| t.number))
        else {
            continue;
        };
        if q.digits == 0 {
            continue;
        }
        let to = query_ir::QType::num(q);
        for row in rows.iter_mut() {
            if let Some(v) = row.get_mut(i)
                && matches!(v, QValue::Number(_))
            {
                *v = crate::eval::cast(v, &to, &[], env, mem).map_err(exec)?;
            }
        }
    }
    Ok(())
}

fn take_locks(
    host: &dyn ExecHost,
    c: &CompiledStmt,
    lowered: &query_ir::StmtL,
) -> Result<(), BatchError> {
    let update = update_tables(host, c);
    if !update.is_empty() {
        host.lock_for_update(&update)
            .map_err(|e| exec(ExecError::Host(e)))?;
    }
    if host.txn().is_some() {
        let read: Vec<_> = lowered
            .touched
            .iter()
            .copied()
            .filter(|t| !update.contains(t))
            .collect();
        if !read.is_empty() {
            host.lock_for_read(&read)
                .map_err(|e| exec(ExecError::Host(e)))?;
        }
    }
    Ok(())
}

fn update_tables(host: &dyn ExecHost, c: &CompiledStmt) -> Vec<ibschema::storage::TableId> {
    let StmtKind::Query(q) = &c.typed.kind else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    let mut todo: Vec<&Query> = vec![q];
    while let Some(q) = todo.pop() {
        for part in &q.parts {
            select_update_tables(host, part, &mut ids, &mut todo);
        }
    }
    let mut out = Vec::new();
    for id in ids {
        if let Some(t) = host.storage().table(id)
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    out
}

fn select_update_tables<'a>(
    host: &dyn ExecHost,
    part: &'a Select,
    out: &mut Vec<query_ir::QTableId>,
    todo: &mut Vec<&'a Query>,
) {
    if let Some(named) = &part.for_update {
        if named.is_empty() {
            for s in &part.sources {
                for src in std::iter::once(s).chain(s.joins.iter().map(|j| &j.right)) {
                    let TFrom::Table { id, args, .. } = &src.from else {
                        continue;
                    };
                    if !args.is_empty() {
                        continue;
                    }
                    let t = host.schema().table(*id);
                    match (t.kind, t.owner) {
                        (query_ir::TableKind::Object(_), _) => out.push(*id),
                        (query_ir::TableKind::Tabular, Some(owner)) => out.push(owner),
                        _ => {}
                    }
                }
            }
        } else {
            out.extend(named.iter().copied());
        }
    }
    for s in &part.sources {
        for src in std::iter::once(s).chain(s.joins.iter().map(|j| &j.right)) {
            if let TFrom::Subquery(q) = &src.from {
                todo.push(q);
            }
        }
        for j in &s.joins {
            expr_queries(&j.on, &mut |q| todo.push(q));
        }
    }
    let exprs = part
        .items
        .iter()
        .map(|i| &i.expr)
        .chain(part.where_.iter())
        .chain(part.having.iter())
        .chain(part.group.iter().flatten().flatten());
    for e in exprs {
        expr_queries(e, &mut |q| todo.push(q));
    }
}

fn expr_queries<'a>(e: &'a TExpr, f: &mut dyn FnMut(&'a Query)) {
    let mut todo: Vec<&'a TExpr> = vec![e];
    while let Some(e) = todo.pop() {
        match &e.kind {
            TKind::InQuery { l, q, .. } => {
                f(q);
                todo.extend(l.iter().rev());
            }
            TKind::Neg(x) | TKind::Not(x) => todo.push(x),
            TKind::Arith { first, steps } => {
                todo.extend(steps.iter().rev().map(|s| &s.rhs));
                todo.push(first);
            }
            TKind::And(xs) | TKind::Or(xs) => todo.extend(xs.iter().rev()),
            TKind::Compare(_, a, b) => {
                todo.push(b);
                todo.push(a);
            }
            TKind::InList { l, rhs, .. } => todo.extend(l.iter().chain(rhs).rev()),
            TKind::Between { l, lo, hi, .. } => {
                todo.push(hi);
                todo.push(lo);
                todo.push(l);
            }
            TKind::IsNull { l, .. } | TKind::Refs { l, .. } => todo.push(l),
            TKind::Like { l, pattern, .. } => {
                todo.push(pattern);
                todo.push(l);
            }
            TKind::Case {
                operand,
                whens,
                else_,
            } => {
                todo.extend(else_.iter().map(|x| &**x));
                for (w, t) in whens.iter().rev() {
                    todo.push(t);
                    todo.push(w);
                }
                todo.extend(operand.iter().map(|x| &**x));
            }
            TKind::Cast { e, .. } => todo.push(e),
            TKind::Func { args, .. } => todo.extend(args.iter().rev()),
            TKind::Agg { arg, .. } => todo.extend(arg.iter().map(|x| &**x)),
            TKind::Const(_)
            | TKind::Value(_)
            | TKind::Param(_)
            | TKind::Field { .. }
            | TKind::Unit(_) => {}
        }
    }
}

fn put_rules(c: &CompiledStmt) -> Vec<(Vec<usize>, bool)> {
    let StmtKind::Query(q) = &c.typed.kind else {
        return Vec::new();
    };
    let Some(part) = q.parts.iter().find(|p| !p.index_by.is_empty()) else {
        return Vec::new();
    };
    part.index_by
        .iter()
        .enumerate()
        .map(|(i, set)| {
            (
                set.clone(),
                part.index_unique.get(i).copied().unwrap_or(false),
            )
        })
        .collect()
}

fn is_ordered(r: &Rel) -> bool {
    match r {
        Rel::Sort { .. } | Rel::Hierarchy { .. } => true,
        Rel::Top { input, .. } | Rel::Project { input, .. } | Rel::Distinct(input) => {
            is_ordered(input)
        }
        _ => false,
    }
}

pub fn find_parameters(
    host: &dyn ExecHost,
    text: &str,
    temp: &TempTables,
) -> Result<Vec<ParamInfo>, CompileError> {
    let schema = host.schema();
    let batch = query_ir::compile_batch(text, ParseMode::Query, schema)?;
    query_ir::find_parameters(&batch, text, schema, temp.schemas())
}

#[doc(hidden)]
pub fn explain_last(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
) -> Result<String, BatchError> {
    forget(host, temp);
    let values = ParamValuesSnapshot::new(params);
    let batch = query_ir::compile_batch(text, ParseMode::Query, host.schema())
        .map_err(BatchError::Compile)?;
    let n = batch.stmts.len();
    for i in 0..n.saturating_sub(1) {
        host.check_txn().map_err(|e| exec(ExecError::Host(e)))?;
        step(
            host,
            &batch,
            text,
            i,
            params,
            &values,
            temp,
            RunMode::Batch,
            None,
            None,
            None,
            None,
        )
        .map_err(|f| f.err)?;
    }
    let env = host.env();
    let origin_owner = OriginCompileOwner {
        call: None,
        lease: env.lease,
        evictor: env.evictor,
        limit: env.mem_limit,
        bytes: Cell::new(0),
    };
    let p = prepare(
        host,
        &batch,
        text,
        n.saturating_sub(1),
        params,
        &values,
        temp,
        RunMode::Batch,
        None,
        &mut 0,
        &Cell::new(0),
        &origin_owner,
        None,
        env.memory,
    )
    .map_err(|f| f.err)?;
    let Some(lowered) = &p.lowered else {
        return Ok(String::new());
    };
    if env.memory.is_some()
        && query_syntax::with_deep_stack(lowered.statement().max_depth, || {
            crate::accounting_inputs::has_balance_columns(&lowered.statement().root)
        })
        .map_err(exec)?
    {
        assert_managed_execution_root(&env, &env);
    }
    let src = host.read_source();
    Ok(explain_temps(
        lowered.statement(),
        &*src,
        Some(&*temp),
        host.plan_options(),
    ))
}

pub fn count_column(script: query_syntax::ScriptVariant) -> query_ir::ColumnInfo {
    let name = match script {
        query_syntax::ScriptVariant::Russian => "Количество",
        query_syntax::ScriptVariant::English => "Count",
    };
    query_ir::ColumnInfo {
        name: name.into(),
        ty: query_ir::QType::num(query_ir::NumQ {
            digits: 0,
            scale: 0,
            nonneg: false,
        }),
        width: 10,
        nested: None,
    }
}

#[doc(hidden)]
pub fn run_batch_observed(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
    sessions: &mut Vec<crate::observe::ObservationSession>,
) -> Result<Vec<StmtRun>, BatchError> {
    sessions.clear();
    let result = run_batch_inner(host, text, params, temp, mode, None, Some(sessions));
    if result.is_err() && sessions.is_empty() {
        let session = crate::observe::ObservationSession::new();
        session.finish(false);
        sessions.push(session);
    }
    result
}
