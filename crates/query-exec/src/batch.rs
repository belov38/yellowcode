use std::sync::Arc;

use memory_budget::{Evictor, Reservation, StructureLimit};
use query_ir::ir::{BuildError, Rel, build_stmt};
use query_ir::tir::{From as TFrom, Query, Select, StmtKind, TExpr, TKind};
use query_ir::{
    AnalyzeMode, AnalyzeOptions, BindCtx, CompileError, CompileRefusal, CompiledStmt, ExecError,
    ParamInfo, ParamTypes, ParamValues, QValue, TempOut,
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
        self.fixed_sema
            + self.c_sema * u64::from(tokens)
            + self.c_sema_type * u64::from(tokens) * u64::from(max_ref_fanout).div_ceil(8)
            + self.c_dot * u64::from(dots)
            + self.c_deref * u64::from(chained) * u64::from(max_ref_fanout.max(1))
            + self.c_star * u64::from(stars) * u64::from(max_table_width.max(1))
            + self.c_temp * temps as u64
    }

    pub fn build(&self, st: &query_ir::sema::stats::StmtStats) -> u64 {
        self.fixed_build
            + self.c_build * st.nodes
            + self.c_bfan * st.fanout_sum
            + self.c_bcol * st.columns
            + self.c_bjoin * (st.joins + st.subqueries)
            + self.c_btype * st.type_refs
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
            + 2 * self.build(p)
            + self.lower(&policy_ir, max_predefined_bytes)
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
    let r = run_batch_leased(host, text, params, temp, mode, mem, observations);
    if r.is_err()
        && let Some(mem) = mem
    {
        mem.release(mem.compile.bytes());
    }
    r
}

fn run_batch_leased(
    host: &dyn ExecHost,
    text: &str,
    params: &dyn ParamSource,
    temp: &mut TempTables,
    mode: RunMode,
    mem: Option<&BatchMem<'_>>,
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
    let mut out = Vec::with_capacity(if mode == RunMode::One {
        1
    } else {
        batch.stmts.len()
    });
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
            temp,
            mode,
            stage.as_ref(),
            observation,
        ) {
            Ok(r) => {
                if mode == RunMode::One {
                    out.clear();
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
    mem: &'s BatchMem<'a>,
    pre: &'s Preflight,
    i: usize,
}

struct Prepared {
    compiled: Arc<CompiledStmt>,
    ordered: bool,
    lowered: Option<query_ir::StmtL>,
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    host: &dyn ExecHost,
    batch: &query_syntax::ast::Batch,
    text: &str,
    i: usize,
    params: &dyn ParamSource,
    temp: &TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    leased: &mut u64,
    observation: Option<&crate::observe::ObservationSession>,
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
        params,
        host,
    };
    query_ir::bind::preflight_virtual(&compiled, &ctx).map_err(exec)?;
    let ir = build_stmt(&compiled, schema).map_err(build_error)?;
    if let Some(st) = stage {
        let stats = query_ir::ir::stats::stats(&ir);
        let bytes = st.mem.model.lower(&stats, st.mem.max_predefined_bytes);
        st.mem.grow(bytes)?;
        *leased += bytes;
    }
    let lowered = query_ir::bind::bind_with_policy_charge(&ir, &ctx, &mut |stats| {
        if let Some(st) = stage {
            let bytes = st.mem.model.policy(stats, st.mem.max_predefined_bytes);
            st.mem.grow(bytes).map_err(|e| match e {
                BatchError::Exec(e) => e,
                _ => ExecError::Fatal("policy lease failed".into()),
            })?;
            *leased += bytes;
        }
        Ok(())
    })
    .map_err(exec)?;
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
    temp: &mut TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<StmtRun, Failure> {
    forget(host, temp);
    let mut leased = 0u64;
    let r = step_prepared(
        host,
        batch,
        text,
        i,
        params,
        temp,
        mode,
        stage,
        &mut leased,
        observation,
    );
    if let Some(obs) = observation {
        if r.is_ok() {
            obs.finish(true);
        } else if obs.report().closure != crate::observe::ClosureStatus::TerminalPrecheck {
            obs.finish(false);
        }
    }
    if let Some(st) = stage {
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
    temp: &mut TempTables,
    mode: RunMode,
    stage: Option<&Stage<'_, '_>>,
    leased: &mut u64,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<StmtRun, Failure> {
    let p = prepare(
        host,
        batch,
        text,
        i,
        params,
        temp,
        mode,
        stage,
        leased,
        observation,
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
    let Some(lowered) = &p.lowered else {
        return Err(exec(ExecError::Fatal("statement without a plan".into())).into());
    };
    let env = host.env();
    take_locks(host, &p.compiled, lowered)?;
    let res = {
        let src = host.read_source();
        let refused = match observation {
            Some(obs) => crate::register::check_totals_use_observed(lowered, &*src, text, obs),
            None => crate::register::check_totals_use(lowered, &*src, text),
        }
        .map_err(exec)?;
        if let Some(e) = refused {
            return Err(BatchError::Compile(e).into());
        }
        let mut res = match observation {
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
        if !lowered.present.is_empty() {
            let mut mem = MemBudget::of_env(&env);
            if !mem.is_leased()
                && let Err(e) = mem.grow(res.rows.iter().map(|r| row_bytes(r)).sum())
            {
                return Err(exec(e).into());
            }
            if let Err(e) = present::apply(lowered, &mut res.rows, host, &*src, &mut mem) {
                if let Some(l) = env.lease {
                    let _ = l.shrink(crate::exec::result_model(&res) + mem.used());
                }
                mem.reconcile(mem.used());
                return Err(exec(e).into());
            }
            mem.reconcile(mem.used());
        }
        res
    };
    let Some(out) = &p.compiled.temp_out else {
        return done(StmtOutcome::Result(Arc::new(res)), None);
    };
    let put = Some(match out {
        TempOut::Put(schema) => schema.name.clone(),
        TempOut::Add(name) => name.clone(),
    });
    let QueryResult { columns, rows, .. } = res;
    let cols = crate::model::columns_model(&columns);
    drop(columns);
    if let Some(l) = env.lease {
        let _ = l.shrink(cols);
    }
    let mut rows = rows;
    let index_sets = put_rules(&p.compiled);
    let mem = MemBudget::of_env(&env);
    let release_rows = |rows: &Vec<Vec<QValue>>| {
        if let Some(l) = env.lease {
            let _ = l.shrink(crate::model::rows_model(rows));
        }
    };
    if let TempOut::Add(name) = out
        && let Err(e) = convert_added(&mut rows, name, temp, &env, &mem)
    {
        release_rows(&rows);
        return Err(e.into());
    }
    let intermediate = mode == RunMode::Intermediate;
    let cols_model = crate::model::columns_model(&p.compiled.columns);
    let shown_charge = match out {
        _ if !intermediate || !mem.is_leased() => 0,
        TempOut::Put(_) => crate::model::rows_clone_model(&rows) + cols_model,
        TempOut::Add(name) => match temp.table(name) {
            Some(table) => {
                crate::model::rows_clone_model_of(
                    table.rows.len() + rows.len(),
                    table.rows.iter().chain(rows.iter()),
                ) + cols_model
            }
            None => 0,
        },
    };
    if shown_charge > 0
        && let Err(e) = mem.grow(shown_charge)
    {
        release_rows(&rows);
        return Err(exec(e).into());
    }
    let shown = (intermediate && matches!(out, TempOut::Put(_))).then(|| rows.clone());
    mem.reconcile(mem.used());
    let rows_bytes = if env.lease.is_some() {
        crate::model::rows_model(&rows)
    } else {
        0
    };
    let put_result = match out {
        TempOut::Put(schema) => temp.put(schema.clone(), rows, host.txn(), &index_sets, &env),
        TempOut::Add(name) => temp.append(name, rows, host.txn(), &index_sets, &env),
    };
    let n = match put_result {
        Ok(n) => n,
        Err(e) => {
            if let Some(l) = env.lease {
                let _ = l.shrink(rows_bytes + shown_charge);
            }
            return Err(exec(e).into());
        }
    };
    if let (true, TempOut::Add(name)) = (intermediate, out) {
        let Some(table) = temp.table(name) else {
            return Err(exec(ExecError::Fatal("the table of ДОБАВИТЬ is gone".into())).into());
        };
        return done(
            StmtOutcome::Result(Arc::new(QueryResult {
                columns: p.compiled.columns.clone(),
                rows: table.rows.clone(),
                totals: None,
            })),
            put,
        );
    }
    match shown {
        Some(rows) => done(
            StmtOutcome::Result(Arc::new(QueryResult {
                columns: p.compiled.columns.clone(),
                rows,
                totals: None,
            })),
            put,
        ),
        None => done(StmtOutcome::Count(n), put),
    }
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
            temp,
            RunMode::Batch,
            None,
            None,
        )
        .map_err(|f| f.err)?;
    }
    let p = prepare(
        host,
        &batch,
        text,
        n.saturating_sub(1),
        params,
        temp,
        RunMode::Batch,
        None,
        &mut 0,
        None,
    )
    .map_err(|f| f.err)?;
    let Some(lowered) = &p.lowered else {
        return Ok(String::new());
    };
    let src = host.read_source();
    Ok(explain_temps(
        lowered,
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
