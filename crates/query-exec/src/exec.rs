use bsl_prim::Number;
use query_ir::{ExecError, QValue, StmtL};

use crate::model::{VALUE_SIZE, rows_model, value_model, vec_block};
use crate::ops::{Cx, rows_push_growth};
use crate::plan::{PlanOptions, build, plan_temps};
use crate::proxy::with_source;
use crate::result::QueryResult;
use crate::source::{DataSource, ExecEnv};
use crate::temp::TempTables;

pub fn execute(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
) -> Result<QueryResult, ExecError> {
    execute_with(stmt, src, env, PlanOptions::default())
}

#[doc(hidden)]
pub fn execute_with(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    opts: PlanOptions,
) -> Result<QueryResult, ExecError> {
    execute_temps(stmt, src, env, None, opts)
}

#[doc(hidden)]
pub fn execute_temps(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
) -> Result<QueryResult, ExecError> {
    execute_temps_impl(stmt, src, env, temps, opts, None)
}

fn execute_temps_impl(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<QueryResult, ExecError> {
    let Some(obs) = observation else {
        return with_source(stmt.max_depth, src, move |s| {
            run(stmt, s, env, temps, opts, None)
        })?;
    };
    let seed = obs.execution_seed();
    let mut guard = ObservationGuard {
        session: obs,
        published: false,
    };
    let transferred = with_source(stmt.max_depth, src, move |s| {
        let local = crate::observe::ObservationSession::from_seed(seed);
        let result = run(stmt, s, env, temps, opts, Some(&local));
        local.finish(result.is_ok());
        (result, local.seed())
    });
    match transferred {
        Ok((result, report)) => {
            obs.publish(report);
            guard.published = true;
            result
        }
        Err(error) => Err(error),
    }
}

struct ObservationGuard<'a> {
    session: &'a crate::observe::ObservationSession,
    published: bool,
}
impl Drop for ObservationGuard<'_> {
    fn drop(&mut self) {
        if !self.published {
            self.session.finish(false);
        }
    }
}

fn charge_sets(r: &query_ir::RelL, cx: &mut Cx<'_>) -> Result<(), ExecError> {
    use query_ir::{PExpr, RelL};
    fn expr(e: &PExpr, bytes: &mut u64) {
        crate::plan::walk(e, &mut |x| match x {
            PExpr::Set(s) => {
                *bytes += vec_block(s.values.len(), VALUE_SIZE)
                    + s.values.iter().map(value_model).sum::<u64>();
            }
            PExpr::InList { rhs, .. } => {
                *bytes += rhs
                    .iter()
                    .filter_map(|y| match y {
                        PExpr::Lit(v) => Some(value_model(v)),
                        _ => None,
                    })
                    .sum::<u64>();
            }
            PExpr::InQuery { q, .. } => rel(q, bytes),
            _ => {}
        });
    }
    fn exprs<'e>(es: impl IntoIterator<Item = &'e PExpr>, bytes: &mut u64) {
        for e in es {
            expr(e, bytes);
        }
    }
    fn rel(r: &RelL, b: &mut u64) {
        match r {
            RelL::Empty
            | RelL::Scan { .. }
            | RelL::TempScan { .. }
            | RelL::Values { .. }
            | RelL::RegisterRows(_) => {}
            RelL::Slice {
                input,
                totals,
                step,
            } => {
                rel(input, b);
                if let Some(t) = totals {
                    rel(t, b);
                }
                exprs(step.bound.iter(), b);
            }
            RelL::RunningBalance { input, step } => {
                rel(input, b);
                expr(&step.period, b);
                exprs(&step.period_fields, b);
                exprs(step.bounds.iter().flatten(), b);
            }
            RelL::Join { l, r, on, eq, .. } => {
                rel(l, b);
                rel(r, b);
                expr(on, b);
                for (x, y) in eq {
                    expr(x, b);
                    expr(y, b);
                }
            }
            RelL::Filter { input, pred } => {
                rel(input, b);
                expr(pred, b);
            }
            RelL::Project { input, exprs: es } => {
                rel(input, b);
                exprs(es, b);
            }
            RelL::Aggregate {
                input, keys, aggs, ..
            } => {
                rel(input, b);
                exprs(keys, b);
                exprs(aggs.iter().filter_map(|a| a.arg.as_ref()), b);
            }
            RelL::CheckAccess { input, .. } | RelL::Distinct(input) | RelL::Top { input, .. } => {
                rel(input, b)
            }
            RelL::Sort { input, keys } | RelL::HierarchyOrder { input, keys, .. } => {
                rel(input, b);
                exprs(keys.iter().map(|(k, _)| k), b);
            }
            RelL::Union { parts, .. } => parts.iter().for_each(|p| rel(p, b)),
            RelL::HierarchyClosure { seeds, .. } => match seeds {
                query_ir::HierSeeds::Values(v) => exprs(v, b),
                query_ir::HierSeeds::Rel(r) => rel(r, b),
            },
            RelL::SemiJoin { input, l, q, .. } => {
                rel(input, b);
                exprs(l, b);
                rel(q, b);
            }
        }
    }
    let mut bytes = 0u64;
    rel(r, &mut bytes);
    cx.mem.grow(bytes)
}

fn run(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
    observation: Option<&crate::observe::ObservationSession>,
) -> Result<QueryResult, ExecError> {
    crate::register::check_slice_use(stmt, src, observation)?;
    let mut plan = plan_temps(stmt, src, temps, opts)?;
    if let Some(obs) = observation {
        obs.plan(&mut plan, &[0]);
        for (index, nested) in stmt.nested.iter().enumerate() {
            if nested.rows.is_some() {
                obs.nested_slot(index);
            }
        }
        if let Some(totals) = &stmt.totals {
            for point in &totals.points {
                for target in &point.hier {
                    obs.table(target.table);
                }
            }
        }
    }
    let mut op = build(&plan);
    let mut cx = Cx::new(env, src);
    cx.temps = temps;
    cx.observation = observation.cloned();
    cx.week_start = stmt.week_start;
    cx.plan_options(opts);
    charge_sets(&stmt.root, &mut cx)?;
    let width = if stmt.totals.is_some() {
        usize::MAX
    } else {
        stmt.columns.len()
    };
    let mut rows = Vec::new();
    let mut shared = crate::model::Payloads::default();
    while let Some(mut row) = op.next(&mut cx)? {
        row.truncate(width);
        let release = rows_push_growth(&rows, &cx, rows.capacity())?;
        let mem = &cx.mem;
        let (bytes, old_table) =
            crate::model::row_cap_model_shared(&row, &mut shared, &mut |n| mem.grow(n))?;
        cx.mem.grow(bytes)?;
        rows.push(row);
        cx.mem.shrink(release + old_table);
    }
    cx.mem.shrink(shared.table());
    drop(shared);
    op.close(&mut cx);
    drop(op);
    cx.mem.release_scratch();
    crate::ops::nested::stitch(stmt, &mut rows, &mut cx, temps, opts)?;
    if let Some(c) = stmt.auto_number {
        for (i, row) in rows.iter_mut().enumerate() {
            if let Some(v) = row.get_mut(usize::from(c)) {
                *v = QValue::Number(Number::from_u64(i as u64 + 1));
            }
        }
    }
    cx.mem.release_held();
    cx.mem.release_scratch();
    let totals = match &stmt.totals {
        Some(spec) => Some(crate::totals::build(
            spec.clone(),
            &rows,
            src,
            env,
            &cx.mem,
        )?),
        None => None,
    };
    let columns = stmt.columns.clone();
    cx.mem.grow(crate::model::columns_model(&columns))?;
    cx.mem.reconcile(
        rows_model(&rows)
            + crate::model::columns_model(&columns)
            + totals.as_ref().map_or(0, crate::totals::TotalsTree::model),
    );
    Ok(QueryResult {
        columns,
        rows,
        totals,
    })
}

pub fn result_model(r: &QueryResult) -> u64 {
    crate::model::columns_model(&r.columns)
        + rows_model(&r.rows)
        + r.totals
            .as_ref()
            .map_or(0, crate::totals::TotalsTree::model)
}

#[doc(hidden)]
pub fn execute_temps_observed(
    stmt: &StmtL,
    src: &dyn DataSource,
    env: &ExecEnv<'_>,
    temps: Option<&TempTables>,
    opts: PlanOptions,
    obs: &crate::observe::ObservationSession,
) -> Result<QueryResult, ExecError> {
    execute_temps_impl(stmt, src, env, temps, opts, Some(obs))
}
