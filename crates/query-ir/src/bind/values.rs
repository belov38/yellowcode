use super::{BindCtx, Bound, PBound, ParamValue, ValueRows, ValueSet, ref_id};
use crate::exec_error::{ExecError, RuntimeError};
use crate::fold::{Folded, arith, neg};
use crate::host::PredefinedName;
use crate::ir::{AggCall, Expr, ExprKind, Rel, StmtS};
use crate::schema::{TableKind, ValuePath};
use crate::value::QValue;
use ibschema::storage::TableId;
use query_syntax::ast::BinOp;
use query_syntax::{MsgId, QueryError, with_deep_stack};
use std::collections::HashSet;
use std::sync::Arc;

pub(super) fn resolve<'s>(stmt: &'s StmtS, ctx: &BindCtx<'_>) -> Result<Bound<'s>, ExecError> {
    let (tables, constants) = storage_tables(stmt, ctx)?;
    for t in &tables {
        ctx.host.touch_table(*t).map_err(ExecError::Host)?;
    }
    let usage = with_deep_stack(stmt.max_depth, || param_usage(stmt));
    let params = bind_params(stmt, ctx, &usage)?;
    let values = bind_values(stmt, ctx)?;
    with_deep_stack(stmt.max_depth, || fold_stmt(stmt, &params, &values))?;
    Ok(Bound {
        security: Default::default(),
        stmt,
        params,
        values,
        tables,
        constants,
    })
}

fn unbound(ctx: &BindCtx<'_>, t: crate::schema::QTableId) -> ExecError {
    ExecError::Fatal(format!(
        "таблица {} не привязана к хранению",
        ctx.schema.table(t).name_ru
    ))
}

fn storage_tables(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
) -> Result<(Vec<TableId>, Vec<TableId>), ExecError> {
    let mut out = Vec::new();
    let mut constants = Vec::new();
    let mut seen = HashSet::new();
    for q in &stmt.tables {
        let t = ctx.schema.table(*q);
        let ids: Vec<TableId> = if t.kind == TableKind::Constants {
            if constants.is_empty() {
                constants = constant_tables(stmt, ctx, *q)?;
            }
            constants.clone()
        } else if ctx.schema.is_unstored(*q) {
            Vec::new()
        } else {
            vec![ctx.storage.table(*q).ok_or_else(|| unbound(ctx, *q))?]
        };
        for id in ids {
            if seen.insert(id) {
                out.push(id);
            }
        }
    }
    if !stmt.present.is_empty() {
        for t in crate::lower::present_targets(stmt, ctx.schema, ctx.storage, ctx.map) {
            if !t.route_point && seen.insert(t.storage) {
                out.push(t.storage);
            }
        }
    }
    Ok((out, constants))
}

fn constant_tables(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    all: crate::schema::QTableId,
) -> Result<Vec<TableId>, ExecError> {
    let mut v = Vec::new();
    let mut used = HashSet::new();
    visit_stmt(stmt, &mut |e, _| {
        if let ExprKind::Col {
            src,
            col: crate::ir::ColSel::Field(field),
        } = &e.kind
            && stmt
                .sources
                .get(src.0 as usize)
                .is_some_and(|s| s.kind == crate::ir::SrcKind::Table(all))
        {
            used.insert(*field);
        }
    });
    for (i, f) in ctx.schema.table(all).fields.iter().enumerate() {
        if !u16::try_from(i)
            .ok()
            .is_some_and(|i| used.contains(&crate::schema::QFieldId(i)))
        {
            continue;
        }
        let Some(md) = f.md else { continue };
        let obj = ctx.map.object(md).ok_or_else(|| {
            ExecError::Fatal(format!(
                "таблица Константы не привязана к хранению: константа {}",
                f.name_ru
            ))
        })?;
        v.push(obj.main);
    }
    Ok(v)
}

#[derive(Clone, Copy, Default)]
struct Usage {
    set: bool,
    other: bool,
    source: bool,
}

fn param_usage(stmt: &StmtS) -> Vec<Usage> {
    let mut u = vec![Usage::default(); stmt.params.len()];
    let mut scans = Vec::new();
    visit_scans(&stmt.root, &mut |p| scans.push(p));
    for p in scans {
        if let Some(s) = u.get_mut(usize::from(p)) {
            s.source = true;
        }
    }
    let mut sub_scans = Vec::new();
    visit_stmt(stmt, &mut |e, in_set| {
        if let ExprKind::InQuery { q: rel, .. } | ExprKind::SectionAgg { rel, .. } = &e.kind {
            visit_scans(rel, &mut |p| sub_scans.push(p));
        }
        if let ExprKind::Param(i) = &e.kind
            && let Some(s) = u.get_mut(usize::from(*i))
        {
            if in_set {
                s.set = true;
            } else {
                s.other = true;
            }
        }
    });
    for p in sub_scans {
        if let Some(s) = u.get_mut(usize::from(p)) {
            s.source = true;
        }
    }
    u
}

fn not_set(_name: &str) -> ExecError {
    ExecError::Runtime(RuntimeError::sql(QueryError::new(
        MsgId::DBERROR_SQLPARAMETERNOTSET,
        None,
    )))
}

fn moment_like(v: &ParamValue) -> bool {
    let is = |x: &QValue| matches!(x, QValue::PointInTime(_) | QValue::Boundary(_));
    match v {
        ParamValue::Value(x) => is(x),
        ParamValue::List(xs) => xs.iter().any(is),
        ParamValue::Table { rows, .. } => rows.iter().flatten().any(is),
    }
}

#[derive(Default)]
struct BoundParamUsage {
    bounds: HashSet<u16>,
    outside: HashSet<u16>,
}
fn vt_bound_expr_params(e: &Expr, in_bound: bool, out: &mut BoundParamUsage) {
    crate::ir::visit_expr_in_place(e, false, &mut |e| match &e.kind {
        ExprKind::Param(i) => {
            if in_bound {
                out.bounds.insert(*i);
            } else {
                out.outside.insert(*i);
            }
        }
        ExprKind::InQuery { q, .. } => vt_bound_params(q, out),
        ExprKind::SectionAgg { rel, .. } => vt_bound_params(rel, out),
        _ => {}
    });
}
fn vt_bound_params(r: &Rel, out: &mut BoundParamUsage) {
    match r {
        Rel::RegisterVt(v) | Rel::RunningBalance(v) | Rel::Slice(v) => {
            for b in v.bounds.iter().flatten() {
                vt_bound_expr_params(b, true, out);
            }
            if let Some(e) = &v.cond {
                vt_bound_expr_params(e, false, out);
            }
        }
        Rel::ParamScan { param, .. } => {
            out.outside.insert(*param);
        }
        Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } => {}
        Rel::Subquery { input, .. } | Rel::Distinct(input) | Rel::Top { input, .. } => {
            vt_bound_params(input, out)
        }
        Rel::Filter { input, pred } => {
            vt_bound_params(input, out);
            vt_bound_expr_params(pred, false, out);
        }
        Rel::Join { l, r, on, .. } => {
            vt_bound_params(l, out);
            vt_bound_params(r, out);
            vt_bound_expr_params(on, false, out);
        }
        Rel::Aggregate {
            input, keys, aggs, ..
        } => {
            vt_bound_params(input, out);
            for e in keys {
                vt_bound_expr_params(e, false, out);
            }
            for e in aggs.iter().filter_map(|a| a.arg.as_ref()) {
                vt_bound_expr_params(e, false, out);
            }
        }
        Rel::Project { input, exprs } => {
            vt_bound_params(input, out);
            for e in exprs {
                vt_bound_expr_params(e, false, out);
            }
        }
        Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
            vt_bound_params(input, out);
            for k in keys {
                vt_bound_expr_params(&k.expr, false, out);
            }
        }
        Rel::Union { parts, .. } => {
            for r in parts {
                vt_bound_params(r, out);
            }
        }
    }
}

fn ordinary_total_params(e: &crate::totals::TotalsExpr, out: &mut HashSet<u16>) {
    use crate::totals::TotalsExpr;
    match e {
        TotalsExpr::Param(i) => {
            out.insert(*i);
        }
        TotalsExpr::Agg(_) | TotalsExpr::Column(_) | TotalsExpr::Const(_) => {}
        TotalsExpr::Neg(x) | TotalsExpr::Not(x) => ordinary_total_params(x, out),
        TotalsExpr::Bin(_, l, r) | TotalsExpr::Cmp(_, l, r) | TotalsExpr::IsNull(l, r) => {
            ordinary_total_params(l, out);
            ordinary_total_params(r, out);
        }
        TotalsExpr::And(xs) | TotalsExpr::Or(xs) => {
            for x in xs {
                ordinary_total_params(x, out);
            }
        }
        TotalsExpr::Case { whens, otherwise } => {
            for (condition, value) in whens {
                ordinary_total_params(condition, out);
                ordinary_total_params(value, out);
            }
            if let Some(x) = otherwise {
                ordinary_total_params(x, out);
            }
        }
    }
}

fn bind_params(stmt: &StmtS, ctx: &BindCtx<'_>, usage: &[Usage]) -> Result<Vec<PBound>, ExecError> {
    let mut out = Vec::with_capacity(stmt.params.len());
    let mut vt_bounds = BoundParamUsage::default();
    with_deep_stack(stmt.max_depth, || {
        vt_bound_params(&stmt.root, &mut vt_bounds);
        if let Some(totals) = &stmt.totals {
            for (e, _) in &totals.items {
                ordinary_total_params(e, &mut vt_bounds.outside);
            }
            for periods in totals.points.iter().filter_map(|p| p.periods.as_ref()) {
                for e in periods.begin.iter().chain(&periods.end) {
                    vt_bound_expr_params(e, false, &mut vt_bounds);
                }
            }
        }
        for rows in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
            vt_bound_params(&rows.scan, &mut vt_bounds);
            vt_bound_expr_params(&rows.owner, false, &mut vt_bounds);
            vt_bound_expr_params(&rows.number, false, &mut vt_bounds);
            for e in &rows.exprs {
                vt_bound_expr_params(e, false, &mut vt_bounds);
            }
        }
    });
    for (i, name) in stmt.params.iter().enumerate() {
        let u = usage.get(i).copied().unwrap_or_default();
        let session = stmt.qr8.iter().find_map(|p| match p {
            crate::ir::Qr8Param::Session { slot, name } if usize::from(*slot) == i => Some(name),
            _ => None,
        });
        let v = match session {
            Some(Some(name)) => ParamValue::Value(
                ctx.security
                    .session
                    .session_param(name)
                    .map_err(ExecError::Host)?,
            ),
            Some(None) => ParamValue::Value(QValue::Undefined),
            None => ctx.params.get(name).ok_or_else(|| not_set(name))?,
        };
        for check in &stmt.qr8 {
            if let crate::ir::Qr8Param::Criterion { slot, table } = check
                && usize::from(*slot) == i
            {
                let valid = match &v {
                    ParamValue::Value(QValue::Ref { table: number, .. }) => ctx
                        .schema
                        .meta(*table)
                        .and_then(|m| m.criterion.as_ref())
                        .is_some_and(|c| {
                            c.ty.refs
                                .iter()
                                .any(|r| ctx.storage.ref_number(*r) == Some(*number))
                        }),
                    _ => false,
                };
                if !valid {
                    return Err(ExecError::Later {
                        what: "Q7d-CriterionParameterType",
                        stage: "Q7d",
                    });
                }
            }
        }
        if moment_like(&v)
            && (!vt_bounds.bounds.contains(&(i as u16)) || vt_bounds.outside.contains(&(i as u16)))
        {
            return Err(ExecError::Later {
                what: "МоментВремени вне параметров виртуальной таблицы",
                stage: "Q5",
            });
        }
        let b = match v {
            ParamValue::Value(x) if u.set && !u.other => PBound::Set(set_of(vec![x])),
            ParamValue::Value(x) => PBound::Value(x),
            ParamValue::List(xs) if u.set => PBound::Set(set_of(xs)),
            ParamValue::Table { columns, rows } if u.source || u.set => {
                if u.other {
                    return Err(ExecError::Fatal(format!(
                        "параметр {name}: таблица значений вне В (..) и ИЗ"
                    )));
                }
                let stored = stored_rows(&columns, rows);
                if u.source || stored.width != 1 {
                    PBound::Rows(Arc::new(stored))
                } else {
                    let xs = stored
                        .rows
                        .into_iter()
                        .map(|mut r| {
                            if r.is_empty() {
                                QValue::Null
                            } else {
                                r.swap_remove(0)
                            }
                        })
                        .collect();
                    PBound::Set(set_of(xs))
                }
            }
            ParamValue::List(_) | ParamValue::Table { .. } => {
                return Err(ExecError::Fatal(format!(
                    "параметр {name}: массив или таблица значений вне В (..)"
                )));
            }
        };
        out.push(b);
    }
    Ok(out)
}

fn stored_rows(columns: &[crate::temp::TempColumn], rows: Vec<Vec<QValue>>) -> ValueRows {
    let scales: Vec<Option<usize>> = columns
        .iter()
        .map(|c| {
            c.ty.as_ref().and_then(|t| t.number).map(|q| {
                if q.digits == 0 {
                    9
                } else {
                    usize::from(q.scale)
                }
            })
        })
        .collect();
    let rows = rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .enumerate()
                .map(|(i, v)| match (v, scales.get(i)) {
                    (QValue::Number(n), Some(Some(s))) => QValue::Number(cut_scale(&n, *s)),
                    (v, _) => v,
                })
                .collect()
        })
        .collect();
    ValueRows {
        width: columns.len(),
        rows,
    }
}

fn cut_scale(n: &bsl_prim::Number, scale: usize) -> bsl_prim::Number {
    let frac = n.frac_digits();
    if frac.len() <= scale {
        return n.clone();
    }
    let cut = frac.get(..scale).unwrap_or("");
    let text = format!(
        "{}{}.{}",
        if n.is_negative() { "-" } else { "" },
        n.int_digits(),
        if cut.is_empty() { "0" } else { cut }
    );
    bsl_prim::Number::parse(&text).unwrap_or_else(|| n.clone())
}

fn set_of(xs: Vec<QValue>) -> Arc<ValueSet> {
    Arc::new(ValueSet { values: xs })
}

fn bind_values(stmt: &StmtS, ctx: &BindCtx<'_>) -> Result<Vec<QValue>, ExecError> {
    let ref_of = |t: crate::schema::QTableId| -> Result<u32, ExecError> {
        ctx.schema
            .table(t)
            .reference
            .and_then(|r| ctx.storage.ref_number(r))
            .ok_or_else(|| unbound(ctx, t))
    };
    let mut out = Vec::with_capacity(stmt.values.len());
    for v in &stmt.values {
        out.push(match v {
            ValuePath::Enum { table, value } => QValue::Ref {
                table: ref_of(*table)?,
                id: ref_id(*value),
            },
            ValuePath::EmptyRef(t) => QValue::Ref {
                table: ref_of(*t)?,
                id: [0; 16],
            },
            ValuePath::SysEnum(t, i) => QValue::SysEnum(*t, *i),
            ValuePath::Predefined { table, name } => ctx
                .host
                .predefined_ref(&PredefinedName {
                    table: *table,
                    name: name.clone(),
                })
                .map_err(ExecError::Host)?,
            ValuePath::BusinessProcessPoint { .. } => {
                return Err(ExecError::Later {
                    what: "Q7d-RoutePointIdentity",
                    stage: "Q7d",
                });
            }
            ValuePath::TypeRef(t) => {
                let mut code = vec![8u8];
                code.extend(ref_of(*t)?.to_be_bytes());
                QValue::Binary(std::sync::Arc::<[u8]>::from(code).into())
            }
            ValuePath::Unknown => {
                return Err(ExecError::Fatal("ЗНАЧЕНИЕ: неизвестное значение".into()));
            }
        });
    }
    Ok(out)
}

fn fold_stmt(stmt: &StmtS, params: &[PBound], values: &[QValue]) -> Result<(), ExecError> {
    let mut err = None;
    visit_stmt(stmt, &mut |e, _| {
        if err.is_none()
            && let ExprKind::Arith { .. } = &e.kind
            && let Err(x) = const_value(e, params, values)
        {
            err = Some(x);
        }
    });
    err.map_or(Ok(()), Err)
}

fn div_zero() -> ExecError {
    ExecError::Runtime(
        RuntimeError::sql(QueryError::new(MsgId::DBERROR_SQLDIVISIONBYZERO, None))
            .with_tail(QueryError::new(MsgId::NUMERROR_DIVIDEBYZERO, None)),
    )
}

pub(crate) fn const_value(
    e: &Expr,
    params: &[PBound],
    values: &[QValue],
) -> Result<Option<QValue>, ExecError> {
    Ok(match &e.kind {
        ExprKind::Lit(v) => Some(v.clone()),
        ExprKind::Param(i) => match params.get(usize::from(*i)) {
            Some(PBound::Value(v)) => Some(v.clone()),
            _ => None,
        },
        ExprKind::Value(i) => values.get(usize::from(*i)).cloned(),
        ExprKind::Neg(x) => const_value(x, params, values)?.and_then(|v| neg(&v)),
        ExprKind::Arith { first, steps } => {
            let Some(first) = const_value(first, params, values)? else {
                return Ok(None);
            };
            let mut rest = Vec::with_capacity(steps.len());
            for (op, rhs, _) in steps {
                let Some(r) = const_value(rhs, params, values)? else {
                    return Ok(None);
                };
                rest.push((*op, r));
            }
            fold_chain(first, &rest)?
        }
        _ => None,
    })
}

pub(crate) fn fold_chain(
    first: QValue,
    steps: &[(BinOp, QValue)],
) -> Result<Option<QValue>, ExecError> {
    let mut acc = first;
    for (op, r) in steps {
        if *op == BinOp::Div
            && let (QValue::Number(_), QValue::Number(d)) = (&acc, r)
            && d.is_zero()
        {
            return Err(div_zero());
        }
        match arith(*op, &acc, r) {
            Ok(Folded::Value(v)) => acc = v,
            Ok(Folded::Runtime) | Err(_) => return Ok(None),
        }
    }
    Ok(Some(acc))
}

fn visit_scans(r: &Rel, f: &mut dyn FnMut(u16)) {
    if let Rel::ParamScan { param, .. } = r {
        f(*param);
    }
    let mut next = |c: &Rel| visit_scans(c, f);
    match r {
        Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => {}
        Rel::Subquery { input, .. }
        | Rel::Distinct(input)
        | Rel::Top { input, .. }
        | Rel::Filter { input, .. }
        | Rel::Aggregate { input, .. }
        | Rel::Project { input, .. }
        | Rel::Sort { input, .. }
        | Rel::Hierarchy { input, .. } => next(input),
        Rel::Join { l, r, .. } => {
            next(l);
            next(r);
        }
        Rel::Union { parts, .. } => parts.iter().for_each(next),
        Rel::RegisterVt(_) | Rel::RunningBalance(_) | Rel::Slice(_) => {}
    }
}

pub(crate) fn visit_stmt<'e>(stmt: &'e StmtS, f: &mut dyn FnMut(&'e Expr, bool)) {
    visit_rel(&stmt.root, f);
    for r in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
        visit_rel(&r.scan, f);
        visit_expr(&r.owner, false, f);
        visit_expr(&r.number, false, f);
        r.exprs.iter().for_each(|e| visit_expr(e, false, f));
    }
}

pub(crate) fn visit_rel<'e>(r: &'e Rel, f: &mut dyn FnMut(&'e Expr, bool)) {
    match r {
        Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => {}
        Rel::RegisterVt(v) | Rel::RunningBalance(v) | Rel::Slice(v) => {
            v.bounds
                .iter()
                .flatten()
                .for_each(|b| visit_expr(b, false, f));
            v.cond
                .iter()
                .chain(v.area.iter().map(|a| &a.pred))
                .chain(v.access.iter())
                .for_each(|c| visit_expr(c, false, f));
        }
        Rel::Subquery { input, .. } | Rel::Distinct(input) | Rel::Top { input, .. } => {
            visit_rel(input, f)
        }
        Rel::Join { l, r, on, .. } => {
            visit_rel(l, f);
            visit_rel(r, f);
            visit_expr(on, false, f);
        }
        Rel::Filter { input, pred } => {
            visit_rel(input, f);
            visit_expr(pred, false, f);
        }
        Rel::Aggregate {
            input, keys, aggs, ..
        } => {
            visit_rel(input, f);
            keys.iter().for_each(|k| visit_expr(k, false, f));
            for AggCall { arg, .. } in aggs {
                if let Some(a) = arg {
                    visit_expr(a, false, f);
                }
            }
        }
        Rel::Project { input, exprs } => {
            visit_rel(input, f);
            exprs.iter().for_each(|e| visit_expr(e, false, f));
        }
        Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
            visit_rel(input, f);
            keys.iter().for_each(|k| visit_expr(&k.expr, false, f));
        }
        Rel::Union { parts, .. } => parts.iter().for_each(|p| visit_rel(p, f)),
    }
}

pub(crate) fn visit_expr<'e>(e: &'e Expr, in_set: bool, f: &mut dyn FnMut(&'e Expr, bool)) {
    f(e, in_set);
    match &e.kind {
        ExprKind::Col { .. }
        | ExprKind::Lit(_)
        | ExprKind::Param(_)
        | ExprKind::Value(_)
        | ExprKind::KeyRef(_)
        | ExprKind::AggRef(_)
        | ExprKind::OuterKey { .. }
        | ExprKind::Out(_)
        | ExprKind::Unit(_) => {}
        ExprKind::Deref { base: x, .. }
        | ExprKind::Neg(x)
        | ExprKind::Not(x)
        | ExprKind::IsNull { l: x, .. }
        | ExprKind::Refs { l: x, .. }
        | ExprKind::Cast { x, .. } => visit_expr(x, false, f),
        ExprKind::Arith { first, steps } => {
            visit_expr(first, false, f);
            steps.iter().for_each(|(_, x, _)| visit_expr(x, false, f));
        }
        ExprKind::And(xs) | ExprKind::Or(xs) | ExprKind::Func { args: xs, .. } => {
            xs.iter().for_each(|x| visit_expr(x, false, f))
        }
        ExprKind::Compare(_, a, b) => {
            visit_expr(a, false, f);
            visit_expr(b, false, f);
        }
        ExprKind::InList { l, rhs, .. } => {
            l.iter().for_each(|x| visit_expr(x, false, f));
            rhs.iter().for_each(|x| visit_expr(x, true, f));
        }
        ExprKind::InQuery { l, q, .. } => {
            l.iter().for_each(|x| visit_expr(x, false, f));
            visit_rel(q, f);
        }
        ExprKind::SectionAgg { owner, rel, .. } => {
            visit_expr(owner, false, f);
            visit_rel(rel, f);
        }
        ExprKind::Between { l, lo, hi, .. } => {
            visit_expr(l, false, f);
            visit_expr(lo, false, f);
            visit_expr(hi, false, f);
        }
        ExprKind::Like { l, pat, .. } => {
            visit_expr(l, false, f);
            visit_expr(pat, false, f);
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                visit_expr(o, false, f);
            }
            for (w, t) in whens {
                visit_expr(w, false, f);
                visit_expr(t, false, f);
            }
            if let Some(x) = else_ {
                visit_expr(x, false, f);
            }
        }
    }
}
