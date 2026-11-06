use super::*;
use crate::access::PredicateForm;
use crate::schema::FieldRole;
use crate::tir::{Col, TExpr, TKind};

pub(crate) fn refusal(balance: bool) -> ExecError {
    ExecError::Later {
        what: if balance {
            "Q6a-AccountingBalanceRls"
        } else {
            "Q6b-TurnoversRls"
        },
        stage: "Q7b",
    }
}

pub(super) fn validate(
    access: &Access,
    table: QTableId,
    chart: bool,
    balance: bool,
    ctx: &BindCtx<'_>,
) -> Result<(), ExecError> {
    let Access::Restricted(predicates) = access else {
        return Ok(());
    };
    let owner = if chart {
        table
    } else {
        ctx.schema
            .table(table)
            .owner
            .ok_or_else(|| refusal(balance))?
    };
    let physical = ctx
        .storage
        .table(owner)
        .and_then(|id| ctx.map.try_table(id));
    if physical.is_none_or(|t| !t.separators.is_empty()) {
        return Err(refusal(balance));
    }
    let proof = Proof {
        schema: ctx.schema,
        storage: ctx.storage,
        map: ctx.map,
    };
    for predicate in predicates {
        if predicate.table() != owner {
            return Err(refusal(balance));
        }
        let PredicateForm::Short(expr) = &predicate.form else {
            return Err(refusal(balance));
        };
        if !query_syntax::with_deep_stack(predicate.max_depth, || {
            proved(expr, owner, chart, &proof)
        }) {
            return Err(refusal(balance));
        }
    }
    Ok(())
}

struct Proof<'a> {
    schema: &'a QuerySchema,
    storage: &'a crate::StorageBinding,
    map: &'a ibschema::storage::StorageMap,
}

fn proved(e: &TExpr, owner: QTableId, chart: bool, ctx: &Proof<'_>) -> bool {
    let all = |es: &[TExpr]| es.iter().all(|e| proved(e, owner, chart, ctx));
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
            let dimension = field.role == FieldRole::Dimension
                && field.md.is_some_and(|id| {
                    ctx.schema
                        .meta(owner)
                        .is_some_and(|m| m.dims.iter().any(|d| d.id == id && d.balance))
                });
            if !chart && !dimension {
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
            proved(x, owner, chart, ctx)
        }
        TKind::Cast { e, derefs, .. } => derefs.is_empty() && proved(e, owner, chart, ctx),
        TKind::Arith { first, steps } => {
            proved(first, owner, chart, ctx)
                && steps.iter().all(|s| proved(&s.rhs, owner, chart, ctx))
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
        TKind::Compare(_, a, b) => proved(a, owner, chart, ctx) && proved(b, owner, chart, ctx),
        TKind::InList {
            l,
            rhs,
            hierarchy: false,
            ..
        } => all(l) && all(rhs),
        TKind::Between { l, lo, hi, .. } => {
            [l, lo, hi].iter().all(|x| proved(x, owner, chart, ctx))
        }
        TKind::Like { l, pattern, .. } => {
            proved(l, owner, chart, ctx) && proved(pattern, owner, chart, ctx)
        }
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand
                .iter()
                .chain(else_)
                .all(|x| proved(x, owner, chart, ctx))
                && whens
                    .iter()
                    .all(|(a, b)| proved(a, owner, chart, ctx) && proved(b, owner, chart, ctx))
        }
        _ => false,
    }
}

pub(super) fn membership_guard(
    stmt: &StmtS,
    decisions: &[(Occurrence, SourceTable, Access)],
    aliases: &std::collections::HashMap<SrcId, SrcId>,
    schema: &QuerySchema,
) -> Result<(), ExecError> {
    fn restricted(
        src: SrcId,
        decisions: &[(Occurrence, SourceTable, Access)],
        aliases: &std::collections::HashMap<SrcId, SrcId>,
        schema: &QuerySchema,
    ) -> Option<bool> {
        let src = aliases.get(&src).copied().unwrap_or(src);
        decisions.iter().find_map(|(key,source,access)| {
            if !matches!(access,Access::Restricted(_)) { return None; }
            match key {
                Occurrence::Direct(id) if *id==src => {
                    if crate::lower::is_accounting_balance(schema,source.table) {Some(true)}
                    else if crate::lower::is_accounting_turnovers(schema,source.table) {Some(false)} else {None}
                }
                Occurrence::BalanceChart(key) if matches!(&**key,Occurrence::Direct(id) if *id==src) => Some(true),
                _=>None,
            }
        })
    }
    fn rel(r: &Rel, f: &mut dyn FnMut(SrcId) -> Option<bool>) -> Option<bool> {
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
            denied =
                denied.or_else(|| rel(q, &mut |src| restricted(src, decisions, aliases, schema)));
        }
    });
    match denied {
        Some(balance) => Err(refusal(balance)),
        None => Ok(()),
    }
}

pub(super) fn user_dereference_guard(
    accesses: &[(SourceTable, FieldSet)],
    keys: &[Occurrence],
    decisions: &[Access],
    schema: &QuerySchema,
    temp_scan_seen: bool,
    mut proof: Option<&mut super::provenance::Proof>,
    aliases: &std::collections::HashMap<SrcId, SrcId>,
) -> Result<(), ExecError> {
    let statement_has_accounting_vt = accesses.iter().any(|(source, _)| {
        matches!(source.part, TablePart::Virtual(_))
            && schema.table(source.table).owner.is_some_and(|owner| {
                schema.table(owner).kind == TableKind::Object(mdschema::MdKind::AccountingRegister)
            })
    });
    let accounting_context = statement_has_accounting_vt || temp_scan_seen;
    let restricted_accounting =
        keys.iter()
            .zip(accesses)
            .zip(decisions)
            .any(|((key, (source, _)), access)| {
                matches!(access, Access::Restricted(_))
                    && (crate::lower::is_accounting_balance(schema, source.table)
                        || crate::lower::is_accounting_turnovers(schema, source.table)
                        || matches!(key, Occurrence::BalanceChart(_)))
            });
    for (index, key) in keys.iter().enumerate() {
        let Occurrence::Target(path, target) = key else {
            continue;
        };
        let chart_target = matches!(
            schema.table(*target).kind,
            TableKind::Object(mdschema::MdKind::ChartOfAccounts)
        );
        let direct_owner = match path.root {
            Root::Column(src, _) | Root::Standard(src, _) => keys
                .iter()
                .position(|k| matches!(k,Occurrence::Direct(id) if *id==src)),
            _ => None,
        };
        let unproved_root = match path.root {
            Root::Opaque(_) => true,
            Root::Column(..) | Root::Standard(..) => direct_owner.is_none(),
            _ => false,
        };
        let target_access_is_restricted =
            matches!(decisions.get(index), Some(Access::Restricted(_)));
        if chart_target
            && unproved_root
            && (restricted_accounting || (target_access_is_restricted && accounting_context))
            && ((temp_scan_seen && !proof.as_deref().is_some_and(|p| p.temp_origins_enabled()))
                || !proof.as_deref_mut().is_some_and(|p| {
                    p.permits(
                        &path.root,
                        target_access_is_restricted,
                        keys,
                        decisions,
                        aliases,
                    )
                }))
        {
            return Err(ExecError::Later {
                what: "Q7b-DeferredSecurityRead",
                stage: "Q7c",
            });
        }
        let Some(owner_index) = direct_owner else {
            continue;
        };
        let Some((source, _)) = accesses.get(owner_index) else {
            continue;
        };
        let balance = crate::lower::is_accounting_balance(schema, source.table);
        if !balance && !crate::lower::is_accounting_turnovers(schema, source.table) {
            continue;
        }
        let account = match path.root {
            Root::Column(_, crate::ir::ColSel::Field(id)) => schema
                .field(source.table, id)
                .is_some_and(|f| f.role == FieldRole::Std(crate::StdRole::Account)),
            Root::Standard(_, crate::StdRole::Account) => true,
            _ => false,
        };
        if account
            && (matches!(decisions.get(index), Some(Access::Restricted(_)))
                || matches!(decisions.get(owner_index), Some(Access::Restricted(_))))
        {
            return Err(refusal(balance));
        }
    }
    Ok(())
}
