use crate::batch_memory::BatchMemory;
use crate::exec_host::TestHost;
use crate::host::{TestBase, project_root};
use crate::params::{CaseParams, ParamErr, case_params};
use crate::repr::repr_lang;
use bsl_prim::Number;
use query_exec::{
    BatchError, ExecHost, QueryResult, RunMode as ExecMode, StmtOutcome, TempTables, count_column,
    explain_last, run_batch_budgeted, run_batch_detailed,
};
use query_ir::ir::{BuildError, build_stmt};
use query_ir::{
    AnalyzeMode, AnalyzeOptions, CompileError, ExecError, HostError, QValue, TempMap, analyze_stmt,
};
use query_syntax::Lang;
use query_syntax::ParseMode;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone, PartialEq)]
pub enum CaseOutcome {
    Ok(Value),
    Err(Vec<String>),
    Unsupported(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Match,
    Differ(String),
    Unsupported(&'static str),
}

fn conformance_dir() -> Result<PathBuf, String> {
    let p = std::env::var_os("CONFORMANCE_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(project_root()?.join("tests/conformance")))
        .ok_or("CONFORMANCE_DIR is not set and the project root is not found")?;
    if p.is_dir() {
        Ok(p)
    } else {
        Err(format!(
            "CONFORMANCE_DIR: {} is not a directory",
            p.display()
        ))
    }
}

pub fn cases(set: &str) -> Result<Vec<Value>, String> {
    cases_in(&conformance_dir()?, set)
}

pub fn cases_in(dir: &Path, set: &str) -> Result<Vec<Value>, String> {
    let path = dir.join(set);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        out.push(v);
    }
    Ok(out)
}

pub fn case_by_id(set: &str, id: &str) -> Result<Value, String> {
    cases(set)?
        .into_iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| format!("{set}: no case {id}"))
}

pub fn has_walk_or_unload(case: &Value) -> bool {
    ["walk", "unload"]
        .iter()
        .any(|k| case.get(*k).is_some_and(|v| !v.is_null()))
}

enum Stop {
    Err(Vec<String>),
    Unsupported(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunFailure {
    pub chain: Vec<String>,
    pub at: Option<(u32, u32)>,
    pub later: Option<&'static str>,
}

impl RunFailure {
    fn err(chain: Vec<String>) -> RunFailure {
        RunFailure {
            chain,
            at: None,
            later: None,
        }
    }

    fn later(what: &'static str) -> RunFailure {
        RunFailure {
            chain: Vec::new(),
            at: None,
            later: Some(what),
        }
    }
}

impl From<RunFailure> for Stop {
    fn from(f: RunFailure) -> Stop {
        match f.later {
            Some(w) => Stop::Unsupported(w),
            None => Stop::Err(f.chain),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunMode {
    pub lang: Lang,
    pub inter: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StmtRun {
    pub result: QueryResult,
    pub ordered: bool,
    pub put: Option<String>,
    pub dropped: bool,
}

pub fn run_case(base: &TestBase, case: &Value) -> CaseOutcome {
    let lang = case_lang(case);
    let host = match TestHost::new(base, lang) {
        Ok(h) => h,
        Err(e) => return CaseOutcome::Err(vec![format!("internal: {e}")]),
    };
    run_case_on(&host, case)
}

pub fn run_case_movements(base: &TestBase, case: &Value) -> CaseOutcome {
    let lang = case_lang(case);
    let mut host = match TestHost::new(base, lang) {
        Ok(h) => h,
        Err(e) => return CaseOutcome::Err(vec![format!("internal: {e}")]),
    };
    host.movements = true;
    run_case_on(&host, case)
}

pub fn run_case_on(host: &TestHost<'_>, case: &Value) -> CaseOutcome {
    match run_inner(host, case) {
        Ok(v) => CaseOutcome::Ok(v),
        Err(Stop::Err(c)) => CaseOutcome::Err(c),
        Err(Stop::Unsupported(w)) => CaseOutcome::Unsupported(w),
    }
}

fn compile_chain(e: &CompileError, text: &str, lang: Lang, base: &TestBase) -> RunFailure {
    let script = base.schema.script();
    let mut chain = vec![e.render(text, lang, script)];
    let mut c = e.cause.as_deref();
    while let Some(x) = c {
        chain.push(x.message(lang, script));
        c = x.cause.as_deref();
    }
    RunFailure {
        chain,
        at: e.at.map(|a| (a.pos.line, a.pos.col)),
        later: None,
    }
}

fn host_chain(h: &HostError) -> Vec<String> {
    let mut out = vec![h.message.clone()];
    let mut c = h.cause.as_deref();
    while let Some(x) = c {
        out.push(x.message.clone());
        c = x.cause.as_deref();
    }
    out
}

fn exec_chain(e: &ExecError, lang: Lang, base: &TestBase) -> RunFailure {
    if let ExecError::Later { what, stage } = e {
        if verbose() {
            eprintln!("LATER: {what}: {stage}");
        }
        return RunFailure::later("construct of a later stage");
    }
    RunFailure::err(match e {
        ExecError::Runtime(r) | ExecError::Rights(r) => r.messages(lang, base.schema.script()),
        ExecError::Host(h) => host_chain(h),
        ExecError::Fatal(s) => vec![format!("internal: {s}")],
        ExecError::Later { .. } => vec![e.to_string()],
        ExecError::Memory(m) => vec![m.public_text().to_string()],
        ExecError::Structure(l) => vec![
            "MEMORY_LIMIT: execution".to_string(),
            ExecError::structure_text(*l).to_string(),
        ],
    })
}

fn case_lang(case: &Value) -> Lang {
    match case
        .get("headers")
        .and_then(|h| h.get("Accept-Language"))
        .and_then(Value::as_str)
    {
        Some("en") => Lang::En,
        _ => Lang::Ru,
    }
}

fn run_inner(host: &TestHost<'_>, case: &Value) -> Result<Value, Stop> {
    let base = host.base;
    if case["mode"] != "query" {
        return Err(Stop::Unsupported("mode bsl"));
    }
    let text = case["text"]
        .as_str()
        .ok_or(Stop::Unsupported("case without text"))?;
    let lang = host.lang();
    let mode = match case["exec"].as_str() {
        Some("one") => ExecMode::One,
        Some("inter") => ExecMode::Intermediate,
        _ => ExecMode::Batch,
    };
    let params = case_params(host, case).map_err(|e| match e {
        ParamErr::Unsupported(w) => Stop::Unsupported(w),
        ParamErr::Later { what, stage } => {
            if verbose() {
                eprintln!("LATER: {what}: {stage}");
            }
            Stop::Unsupported(what)
        }
        ParamErr::Bad(m) => Stop::Err(vec![format!("parameter: {m}")]),
        ParamErr::UndefinedType(name) => {
            let message = platform_catalog::messages::text_key(
                "bsl:IDS_RTE_TYPENOTDEFINED",
                match lang {
                    Lang::Ru => platform_catalog::messages::Lang::Ru,
                    Lang::En => platform_catalog::messages::Lang::En,
                },
            )
            .unwrap_or("IDS_RTE_TYPENOTDEFINED");
            Stop::Err(vec![format!("{message} ({name})")])
        }
    })?;
    let schema = &*base.schema;
    let mut temp = TempTables::new();
    let runs = run_batch_detailed(host, text, &params, &mut temp, mode)
        .map_err(|e| Stop::from(batch_failure(e, text, lang, base)))?;
    let walk_kind = case["walk"].as_str().unwrap_or("");
    let unload_kind = case["unload"].as_str().unwrap_or("");
    let syntax = query_syntax::parse_batch(text, ParseMode::Query).ok();
    let mut results = Vec::with_capacity(runs.len());
    for (statement, run) in runs.iter().enumerate() {
        let walked = match &run.outcome {
            StmtOutcome::Result(res)
                if res.totals.is_some() || !walk_kind.is_empty() || !unload_kind.is_empty() =>
            {
                Some(Arc::clone(res))
            }
            StmtOutcome::Count(_) if !walk_kind.is_empty() => {
                Some(Arc::new(stmt_result(run.outcome.clone(), base)))
            }
            _ => None,
        };
        if let Some(res) = &walked {
            let columns: Vec<Value> = res
                .columns
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "type": query_testkit::repr::type_repr(schema, &c.ty, lang),
                        "width": c.width,
                    })
                })
                .collect();
            let coll = base.collation.as_ref();
            let rows = crate::walks::rows_json(res, walk_kind, base, lang, coll)
                .map_err(|e| Stop::Err(crate::walks::walk_error(&e, "Выбрать", lang)))?;
            let mut r = json!({
                "columns": columns,
                "empty": res.rows.is_empty(),
                "rows": rows,
                "ordered": run.ordered,
            });
            if let Some(t) = &res.totals
                && run.ordered
                && !t.spec.order.is_empty()
            {
                let cols: Option<Vec<usize>> = t
                    .spec
                    .order
                    .iter()
                    .map(|k| match k.source {
                        query_exec::totals::KeySource::Row(c) => Some(c),
                        _ => None,
                    })
                    .collect();
                if let (Some(cols), Some(m)) = (cols, r.as_object_mut()) {
                    m.insert("_ties".into(), json!(cols));
                    if walk_kind.is_empty() {
                        let mask = crate::walks::detail_mask(res, coll).map_err(|e| {
                            Stop::Err(crate::walks::walk_error(&e, "Выбрать", lang))
                        })?;
                        m.insert("_details".into(), json!(mask));
                    }
                }
            }
            if !unload_kind.is_empty() {
                let (t, c, u) =
                    crate::walks::unload_json(res, unload_kind, &columns, base, lang)
                        .map_err(|e| Stop::Err(crate::walks::walk_error(&e, "Выгрузить", lang)))?;
                if let Some(m) = r.as_object_mut() {
                    m.insert("unload_type".into(), t);
                    m.insert("unload_columns".into(), c);
                    m.insert("unload".into(), u);
                }
            }
            results.push(r);
            continue;
        }
        let (columns, rows): (Vec<Value>, Vec<Vec<QValue>>) = match &run.outcome {
            StmtOutcome::Dropped => {
                results.push(Value::Null);
                continue;
            }
            StmtOutcome::Count(n) => (
                vec![count_column_json(schema, lang)],
                vec![vec![QValue::Number(Number::from_u64(*n))]],
            ),
            StmtOutcome::Result(res) => {
                let columns = res
                    .columns
                    .iter()
                    .map(|c| {
                        json!({
                            "name": c.name,
                            "type": query_testkit::repr::type_repr(schema, &c.ty, lang),
                            "width": c.width,
                        })
                    })
                    .collect();
                (columns, res.rows.clone())
            }
        };
        let n = columns.len();
        let rows: Vec<Value> = rows
            .iter()
            .map(|row| {
                Value::Array(
                    row.iter()
                        .take(n)
                        .map(|v| Value::String(repr_lang(v, base, lang)))
                        .collect(),
                )
            })
            .collect();
        let mut result = json!({
            "columns": columns,
            "empty": rows.is_empty(),
            "rows": rows,
            "ordered": run.ordered,
        });
        if run.ordered
            && let Some(cols) = syntax
                .as_ref()
                .and_then(|batch| batch.stmts.get(statement))
                .and_then(crate::detail_ties::columns)
            && let Some(fields) = result.as_object_mut()
        {
            fields.insert("_ties".into(), json!(cols));
            fields.insert("_details".into(), json!(vec![true; rows.len()]));
        }
        results.push(result);
    }
    Ok(json!({ "results": results }))
}

fn count_column_json(schema: &query_ir::QuerySchema, lang: Lang) -> Value {
    let c = count_column(schema.script());
    json!({
        "name": c.name,
        "type": query_testkit::repr::type_repr(schema, &c.ty, lang),
        "width": c.width,
    })
}

fn stmt_result(o: StmtOutcome, base: &TestBase) -> QueryResult {
    match o {
        StmtOutcome::Result(r) => Arc::try_unwrap(r).unwrap_or_else(|r| (*r).clone()),
        StmtOutcome::Count(n) => QueryResult {
            columns: vec![count_column(base.schema.script())],
            rows: vec![vec![QValue::Number(Number::from_u64(n))]],
            totals: None,
        },
        StmtOutcome::Dropped => QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            totals: None,
        },
    }
}

fn batch_failure(e: BatchError, text: &str, lang: Lang, base: &TestBase) -> RunFailure {
    match e {
        BatchError::Compile(e) => compile_chain(&e, text, lang, base),
        BatchError::Exec(e) => exec_chain(&e, lang, base),
    }
}

pub fn run_text(
    base: &TestBase,
    text: &str,
    params: &CaseParams,
    mode: RunMode,
) -> Result<Vec<StmtRun>, RunFailure> {
    run_text_cancel(base, text, params, mode, &AtomicBool::new(false))
}

#[doc(hidden)]
pub fn run_text_with_plan(
    base: &TestBase,
    text: &str,
    params: &CaseParams,
    mode: RunMode,
    plan: query_exec::plan::PlanOptions,
) -> Result<Vec<StmtRun>, RunFailure> {
    let lang = mode.lang;
    let cancel = AtomicBool::new(false);
    let host = TestHost::with_cancel(base, lang, &cancel)
        .map_err(|e| RunFailure::err(vec![format!("internal: {e}")]))?
        .with_plan(plan);
    let exec = if mode.inter {
        ExecMode::Intermediate
    } else {
        ExecMode::Batch
    };
    let mut temp = TempTables::new();
    let runs = run_batch_detailed(&host, text, params, &mut temp, exec)
        .map_err(|e| batch_failure(e, text, lang, base))?;
    Ok(runs.into_iter().map(|r| stmt_run(r, base)).collect())
}

pub fn run_text_cancel(
    base: &TestBase,
    text: &str,
    params: &CaseParams,
    mode: RunMode,
    cancel: &AtomicBool,
) -> Result<Vec<StmtRun>, RunFailure> {
    let lang = mode.lang;
    let host = TestHost::with_cancel(base, lang, cancel)
        .map_err(|e| RunFailure::err(vec![format!("internal: {e}")]))?;
    let exec = if mode.inter {
        ExecMode::Intermediate
    } else {
        ExecMode::Batch
    };
    let mut temp = TempTables::new();
    let runs = run_batch_detailed(&host, text, params, &mut temp, exec)
        .map_err(|e| batch_failure(e, text, lang, base))?;
    Ok(runs.into_iter().map(|r| stmt_run(r, base)).collect())
}

fn stmt_run(r: query_exec::StmtRun, base: &TestBase) -> StmtRun {
    let dropped = matches!(r.outcome, StmtOutcome::Dropped);
    StmtRun {
        result: stmt_result(r.outcome, base),
        ordered: r.ordered,
        put: r.put,
        dropped,
    }
}

pub fn run_text_cancel_budgeted(
    base: &TestBase,
    text: &str,
    params: &CaseParams,
    mode: RunMode,
    cancel: &AtomicBool,
    memory: &BatchMemory,
) -> Result<Vec<StmtRun>, RunFailure> {
    let lang = mode.lang;
    let host = TestHost::with_memory(base, lang, cancel, memory)
        .map_err(|e| RunFailure::err(vec![format!("internal: {e}")]))?;
    let exec = if mode.inter {
        ExecMode::Intermediate
    } else {
        ExecMode::Batch
    };
    let evictor: Arc<dyn memory_budget::Evictor + Send + Sync> = memory.evictor.clone();
    let mut temp = TempTables::with_lease(Arc::clone(&memory.exec), evictor);
    let runs = run_batch_budgeted(&host, text, params, &mut temp, exec, &memory.mem())
        .map_err(|e| batch_failure(e, text, lang, base))?;
    Ok(runs.into_iter().map(|r| stmt_run(r, base)).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRun {
    pub ir: Vec<String>,
    pub plan: String,
    pub executed: usize,
}

pub fn explain_text(
    base: &TestBase,
    text: &str,
    params: &CaseParams,
    mode: RunMode,
    cancel: &AtomicBool,
) -> Result<ExplainRun, RunFailure> {
    let lang = mode.lang;
    let schema = &*base.schema;
    let batch = query_ir::compile_batch(text, ParseMode::Query, schema)
        .map_err(|e| compile_chain(&e, text, lang, base))?;
    let mut temps = TempMap::default();
    let mut ir = Vec::with_capacity(batch.stmts.len());
    for i in 0..batch.stmts.len() {
        let opts = AnalyzeOptions {
            temp_tables: &temps,
            params,
            mode: AnalyzeMode::Execute,
            inter: mode.inter,
        };
        let st = analyze_stmt(&batch, text, i, schema, &opts)
            .map_err(|e| compile_chain(&e, text, lang, base))?;
        if let Some(d) = &st.drop {
            temps.remove(d);
            ir.push(format!("DROP {d}"));
            continue;
        }
        if let Some(query_ir::TempOut::Put(t)) = &st.temp_out {
            temps.insert(t.clone());
        }
        let s = build_stmt(&st, schema).map_err(|e| match e {
            BuildError::Later { what, .. } => RunFailure::later(what),
            BuildError::Internal(s) => RunFailure::err(vec![format!("internal: {s}")]),
            BuildError::Compile(e) => compile_chain(&e, text, lang, base),
        })?;
        ir.push(query_ir::ir::print(&s, schema));
    }
    let host = TestHost::with_cancel(base, lang, cancel)
        .map_err(|e| RunFailure::err(vec![format!("internal: {e}")]))?;
    let mut temp = TempTables::new();
    let plan = explain_last(&host, text, params, &mut temp)
        .map_err(|e| batch_failure(e, text, lang, base))?;
    Ok(ExplainRun {
        ir,
        plan,
        executed: batch.stmts.len().saturating_sub(1),
    })
}

pub fn outcome_json(base: &TestBase, lang: Lang, runs: &[StmtRun]) -> Value {
    let results: Vec<Value> = runs
        .iter()
        .map(|run| result_json(base, lang, &run.result, run.ordered))
        .collect();
    json!({ "results": results })
}

fn result_json(base: &TestBase, lang: Lang, res: &QueryResult, ordered: bool) -> Value {
    let schema = &*base.schema;
    let columns: Vec<Value> = res
        .columns
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "type": query_testkit::repr::type_repr(schema, &c.ty, lang),
                "width": c.width,
            })
        })
        .collect();
    let n = columns.len();
    let rows: Vec<Value> = res
        .rows
        .iter()
        .map(|row| {
            Value::Array(
                row.iter()
                    .take(n)
                    .map(|v| Value::String(repr_lang(v, base, lang)))
                    .collect(),
            )
        })
        .collect();
    json!({
        "columns": columns,
        "empty": rows.is_empty(),
        "rows": rows,
        "ordered": ordered,
    })
}

fn verbose() -> bool {
    std::env::var_os("QUERY_TESTHOST_VERBOSE").is_some()
}

pub fn check(case: &Value, got: &CaseOutcome) -> Verdict {
    let want = &case["expect"];
    let id = case["id"].as_str().unwrap_or("?");
    let differ = |why: String, ours: &dyn std::fmt::Debug| {
        if verbose() {
            eprintln!("DIFF {id}: {why}\n  ours: {ours:?}\n  want: {want}");
        }
        Verdict::Differ(why)
    };
    match got {
        CaseOutcome::Unsupported(w) => Verdict::Unsupported(w),
        CaseOutcome::Err(chain) => {
            if want["ok"] != false {
                return differ("error, the answer is a result".into(), chain);
            }
            let theirs: Vec<&str> = match want["cause"].as_array() {
                Some(cause) => cause.iter().filter_map(Value::as_str).collect(),
                None => want["error"]
                    .as_str()
                    .map(|error| {
                        if want["error_kind"] == "service" {
                            error
                                .strip_prefix("<service failure> HTTP 500:")
                                .map(|body| {
                                    body.trim_start_matches(|c: char| {
                                        c.is_whitespace() || c == '\u{feff}'
                                    })
                                })
                                .unwrap_or(error)
                        } else {
                            error
                        }
                    })
                    .into_iter()
                    .collect(),
            };
            if chain.len() != theirs.len() {
                return differ(
                    format!("chain length {} against {}", chain.len(), theirs.len()),
                    chain,
                );
            }
            match chain.iter().zip(&theirs).position(|(a, b)| a != b) {
                Some(i) => differ(format!("chain item {i} differs"), chain),
                None => Verdict::Match,
            }
        }
        CaseOutcome::Ok(v) => {
            if want["ok"] != true {
                return differ("a result, the answer is an error".into(), v);
            }
            let (ours, theirs) = match (v["results"].as_array(), want["results"].as_array()) {
                (Some(a), Some(b)) => (a, b),
                _ => return differ("no results".into(), v),
            };
            if ours.len() != theirs.len() {
                return differ(
                    format!("{} results against {}", ours.len(), theirs.len()),
                    v,
                );
            }
            for (k, (o, t)) in ours.iter().zip(theirs).enumerate() {
                if t.is_null() || o.is_null() {
                    if t.is_null() && o.is_null() {
                        continue;
                    }
                    return differ(format!("result {k}: Неопределено on one side only"), v);
                }
                if let Some(why) = compare_result(o, t, true) {
                    return differ(format!("result {k}: {why}"), v);
                }
                if std::env::var_os("QUERY_TESTHOST_TIES").is_some()
                    && let Some(why) = compare_result(o, t, false)
                {
                    eprintln!("TIES {id}: result {k}: strictly {why}");
                }
            }
            Verdict::Match
        }
    }
}

fn compare_result(ours: &Value, want: &Value, untie: bool) -> Option<String> {
    let (oc, wc) = (ours["columns"].as_array()?, want["columns"].as_array()?);
    if oc.len() != wc.len() {
        return Some(format!("{} columns against {}", oc.len(), wc.len()));
    }
    for (i, (a, b)) in oc.iter().zip(wc).enumerate() {
        for key in ["name", "type", "width"] {
            if a[key] != b[key] {
                return Some(format!("column {i}: {key} differs"));
            }
        }
    }
    if ours["empty"] != want["empty"] {
        return Some("emptiness differs".into());
    }
    for key in ["unload_type", "unload_columns", "unload"] {
        if key == "unload" && untie && !ours["_ties"].is_null() {
            continue;
        }
        if !want[key].is_null() && ours[key] != want[key] {
            return Some(format!("{key} differs"));
        }
    }
    let strs = |v: &Value| -> Vec<Value> { v["rows"].as_array().cloned().unwrap_or_default() };
    let (mut o, mut w) = (strs(ours), strs(want));
    if let Some(cols) = ours["_ties"].as_array().filter(|_| untie) {
        let cols: Vec<usize> = cols
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|c| usize::try_from(c).ok())
            .collect();
        let mask: Option<Vec<bool>> = ours["_details"]
            .as_array()
            .map(|m| m.iter().map(|b| b.as_bool().unwrap_or(false)).collect());
        o = crate::walks::untie(&o, &cols, mask.as_deref());
        w = crate::walks::untie(&w, &cols, mask.as_deref());
        if let (Some(a), Some(b)) = (ours["unload"].as_str(), want["unload"].as_str())
            && crate::walks::untie_text(a, &cols) != crate::walks::untie_text(b, &cols)
        {
            return Some("unload differs".into());
        }
    }
    if o.len() != w.len() {
        return Some(format!("{} rows against {}", o.len(), w.len()));
    }
    if want["ordered"] == true {
        return o.iter().zip(&w).enumerate().find_map(|(i, (a, b))| {
            (a != b).then(|| match (a.as_array(), b.as_array()) {
                (Some(x), Some(y)) => match x.iter().zip(y).position(|(p, q)| p != q) {
                    Some(c) => format!("row {i} differs at column {c}"),
                    None => format!("row {i} differs"),
                },
                _ => format!("row {i} differs"),
            })
        });
    }
    o.sort_by_key(canonical);
    w.sort_by_key(canonical);
    o.iter()
        .zip(&w)
        .position(|(a, b)| a != b)
        .map(|_| "the rows differ as a multiset".to_string())
}

pub(crate) fn canonical(v: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let b: std::collections::BTreeMap<String, Value> =
                    m.iter().map(|(k, x)| (k.clone(), sorted(x))).collect();
                Value::Object(b.into_iter().collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            x => x.clone(),
        }
    }
    sorted(v).to_string()
}

pub fn check_case(base: &TestBase, case: &Value) -> Option<String> {
    match check(case, &run_case(base, case)) {
        Verdict::Match => None,
        Verdict::Differ(d) => Some(d),
        Verdict::Unsupported(w) => Some(format!("unsupported: {w}")),
    }
}

pub fn known_deviations(group: &str) -> usize {
    let Ok(dir) = conformance_dir() else { return 0 };
    let Ok(text) = std::fs::read_to_string(dir.join("known-deviations.toml")) else {
        return 0;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return 0;
    };
    let prefix = format!("query/{group}.");
    doc.get("deviation")
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("id").and_then(toml::Value::as_str))
                .filter(|id| id.starts_with(&prefix))
                .count()
        })
        .unwrap_or(0)
}
