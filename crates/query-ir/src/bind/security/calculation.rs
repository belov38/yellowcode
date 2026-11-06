use super::*;
use crate::access::PredicateForm;
use crate::tir::{Col, TExpr, TKind};

pub(crate) fn code(kind: VtKind) -> Option<&'static str> {
    match kind {
        VtKind::ActualActionPeriod => Some("Q6c-ActualActionPeriodRls"),
        VtKind::ScheduleData => Some("Q6c-ScheduleDataRls"),
        _ => None,
    }
}
pub(crate) fn refusal(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q7b" }
}
pub(super) fn occurrence_code(key: &Occurrence, source: &SourceTable) -> Option<&'static str> {
    if matches!(key, Occurrence::Schedule(_)) {
        return Some("Q6c-ScheduleDataScheduleRls");
    }
    match source.part {
        TablePart::Virtual(kind) => code(kind),
        _ => None,
    }
}
pub(super) fn validate(
    access: &Access,
    table: QTableId,
    what: &'static str,
    ctx: &BindCtx<'_>,
) -> Result<(), ExecError> {
    let Access::Restricted(predicates) = access else {
        return Ok(());
    };
    let owner = if matches!(ctx.schema.table(table).kind, TableKind::Virtual(_)) {
        ctx.schema.table(table).owner.ok_or_else(|| refusal(what))?
    } else {
        table
    };
    let proof = Proof {
        schema: ctx.schema,
        storage: ctx.storage,
        map: ctx.map,
    };
    if ctx
        .storage
        .table(owner)
        .and_then(|id| ctx.map.try_table(id))
        .is_none_or(|t| !t.separators.is_empty())
    {
        return Err(refusal(what));
    }
    for predicate in predicates {
        let PredicateForm::Short(expr) = &predicate.form else {
            return Err(refusal(what));
        };
        if predicate.table() != owner
            || !query_syntax::with_deep_stack(predicate.max_depth, || proved(expr, owner, &proof))
        {
            return Err(refusal(what));
        }
    }
    Ok(())
}
struct Proof<'a> {
    schema: &'a QuerySchema,
    storage: &'a crate::StorageBinding,
    map: &'a ibschema::storage::StorageMap,
}

fn proved(e: &TExpr, owner: QTableId, ctx: &Proof<'_>) -> bool {
    let all = |es: &[TExpr]| es.iter().all(|e| proved(e, owner, ctx));
    match &e.kind {
        TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => true,
        TKind::Field {
            level: 0,
            source: 0,
            col: Col::Field(id),
            derefs,
        } => {
            let Some(field) = ctx.schema.field(owner, *id) else {
                return false;
            };
            if field.nested.is_some() {
                return false;
            }
            let Some(table) = ctx.storage.table(owner) else {
                return false;
            };
            if ctx.storage.field(owner, *id).is_none() {
                return false;
            }
            if ctx
                .map
                .try_table(table)
                .is_none_or(|t| !t.separators.is_empty())
            {
                return false;
            }
            let mut ty = &field.ty;
            for step in derefs {
                let [(target, field)] = step.targets.as_slice() else {
                    return false;
                };
                if ty.atom_count() != 1
                    || ty.refs.len() != 1
                    || ty
                        .refs
                        .first()
                        .is_none_or(|atom| ctx.schema.ref_info(*atom).table != *target)
                {
                    return false;
                }
                let Some(table) = ctx
                    .storage
                    .table(*target)
                    .and_then(|id| ctx.map.try_table(id))
                else {
                    return false;
                };
                let Some(f) = ctx.schema.field(*target, *field) else {
                    return false;
                };
                if !table.separators.is_empty() || ctx.storage.field(*target, *field).is_none() {
                    return false;
                }
                ty = &f.ty;
            }
            true
        }
        TKind::Neg(x) | TKind::Not(x) | TKind::IsNull { l: x, .. } | TKind::Refs { l: x, .. } => {
            proved(x, owner, ctx)
        }
        TKind::Cast { e, derefs, .. } => derefs.is_empty() && proved(e, owner, ctx),
        TKind::Arith { first, steps } => {
            proved(first, owner, ctx) && steps.iter().all(|s| proved(&s.rhs, owner, ctx))
        }
        TKind::And(es) | TKind::Or(es) => all(es),
        TKind::Func { f, args } => {
            !matches!(
                f,
                query_syntax::Func::Presentation
                    | query_syntax::Func::RefPresentation
                    | query_syntax::Func::RecordAutoNumber
                    | query_syntax::Func::GroupedBy
            ) && all(args)
        }
        TKind::Compare(_, a, b) => proved(a, owner, ctx) && proved(b, owner, ctx),
        TKind::InList {
            l,
            rhs,
            hierarchy: false,
            ..
        } => all(l) && all(rhs),
        TKind::Between { l, lo, hi, .. } => [l, lo, hi].iter().all(|x| proved(x, owner, ctx)),
        TKind::Like { l, pattern, .. } => proved(l, owner, ctx) && proved(pattern, owner, ctx),
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand.iter().chain(else_).all(|x| proved(x, owner, ctx))
                && whens
                    .iter()
                    .all(|(a, b)| proved(a, owner, ctx) && proved(b, owner, ctx))
        }
        _ => false,
    }
}

pub(super) fn membership_guard(
    stmt: &StmtS,
    decisions: &[(Occurrence, SourceTable, Access)],
    aliases: &std::collections::HashMap<SrcId, SrcId>,
) -> Result<(), ExecError> {
    fn restricted(
        src: SrcId,
        decisions: &[(Occurrence, SourceTable, Access)],
        aliases: &std::collections::HashMap<SrcId, SrcId>,
    ) -> Option<&'static str> {
        let src = aliases.get(&src).copied().unwrap_or(src);
        decisions.iter().find_map(|(key, source, access)| {
            if !matches!(access, Access::Restricted(_)) {
                return None;
            }
            let direct = match key {
                Occurrence::Direct(id) => Some(*id),
                Occurrence::Schedule(parent) => match &**parent {
                    Occurrence::Direct(id) => Some(*id),
                    _ => None,
                },
                _ => None,
            };
            (direct == Some(src))
                .then(|| occurrence_code(key, source))
                .flatten()
        })
    }
    fn rel(r: &Rel, f: &mut dyn FnMut(SrcId) -> Option<&'static str>) -> Option<&'static str> {
        match r {
            Rel::RegisterVt(v) => f(v.src),
            Rel::Subquery { input, .. }
            | Rel::Filter { input, .. }
            | Rel::Project { input, .. }
            | Rel::Aggregate { input, .. }
            | Rel::Sort { input, .. }
            | Rel::Top { input, .. }
            | Rel::Hierarchy { input, .. }
            | Rel::Distinct(input) => rel(input, f),
            Rel::Join { l, r, .. } => rel(l, f).or_else(|| rel(r, f)),
            Rel::Union { parts, .. } => parts.iter().find_map(|p| rel(p, f)),
            _ => None,
        }
    }
    let mut denied = None;
    super::super::visit_stmt(stmt, &mut |e, _| {
        if let crate::ir::ExprKind::InQuery { q, .. } = &e.kind {
            denied = denied.or_else(|| rel(q, &mut |src| restricted(src, decisions, aliases)));
        }
    });
    match denied {
        Some(what) => Err(refusal(what)),
        None => Ok(()),
    }
}

pub(super) fn reserve_expansion(
    stats: &mut crate::bind::PolicyCompileStats,
    decisions: &[(Occurrence, SourceTable, Access)],
    schema: &QuerySchema,
) -> Result<(), ExecError> {
    const COPIES: u64 = (6 + 2 + 4) + (1 + 1 + 5) + 1 + (6 + 2 + 4);
    let add = |a: u64, b: u64| a.checked_add(b).ok_or_else(super::payload::overflow);
    let mul = |a: u64, b: u64| a.checked_mul(b).ok_or_else(super::payload::overflow);
    let mut fields = 0_u64;
    let mut occurrences = 0_u64;
    for (key, source, access) in decisions {
        if !matches!(access, Access::Restricted(_)) || occurrence_code(key, source).is_none() {
            continue;
        }
        occurrences = add(occurrences, 1)?;
        let table = schema.table(source.table);
        let owner = table.owner.unwrap_or(source.table);
        fields = add(
            fields,
            add(
                table.fields.len() as u64,
                schema.table(owner).fields.len() as u64,
            )?,
        )?;
    }
    if occurrences == 0 {
        return Ok(());
    }
    stats.statement.rels = add(stats.statement.rels, mul(occurrences, 128)?)?;
    stats.statement.exprs = add(
        stats.statement.exprs,
        mul(add(fields, mul(occurrences, 16)?)?, COPIES)?,
    )?;
    let p = &mut stats.predicates;
    p.nodes = mul(p.nodes, COPIES)?;
    p.columns = mul(p.columns, COPIES)?;
    p.fanout_sum = mul(p.fanout_sum, COPIES)?;
    p.joins = mul(p.joins, COPIES)?;
    p.sources = mul(p.sources, COPIES)?;
    p.subqueries = mul(p.subqueries, COPIES)?;
    p.type_refs = mul(p.type_refs, COPIES)?;
    Ok(())
}
