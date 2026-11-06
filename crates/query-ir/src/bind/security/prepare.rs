use super::*;
use crate::ir::{ColSel, Expr, ExprKind as E, SourceS};
use std::collections::HashMap;

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct AccountingPolicySources {
    pub register: Option<SrcId>,
    pub chart: Option<SrcId>,
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct CalculationPolicySources {
    pub main: Option<SrcId>,
    pub graph: Option<SrcId>,
}

#[derive(Default, Debug)]
pub(crate) struct Security {
    pub conditions: HashMap<SrcId, Expr>,
    pub targets: HashMap<(Path, QTableId), SrcId>,
    pub aliases: HashMap<SrcId, SrcId>,
    pub accounting_sources: HashMap<SrcId, AccountingPolicySources>,
    pub calculation_sources: HashMap<SrcId, CalculationPolicySources>,
    pub drcr_areas: HashMap<QTableId, crate::lower::vt::DrCrAreaL>,
    max_depth: u32,
}
impl Drop for Security {
    fn drop(&mut self) {
        let conditions = std::mem::take(&mut self.conditions);
        query_syntax::with_deep_stack(self.max_depth, move || drop(conditions));
    }
}
impl Security {
    pub(crate) fn target(
        &self,
        schema: &QuerySchema,
        base: &Expr,
        prefix: &[crate::tir::Deref],
        table: QTableId,
    ) -> Option<SrcId> {
        let mut c = Collector::new(schema);
        c.aliases = self.aliases.clone();
        let mut path = c.ir_path(base);
        if matches!(path.root, Root::Opaque(_)) {
            return None;
        }
        path.steps.extend(prefix.iter().map(|s| s.targets.clone()));
        self.targets.get(&(path, table)).copied()
    }
}

fn drcr_areas(
    decisions: &[(Occurrence, SourceTable, Access)],
    ctx: &BindCtx<'_>,
) -> Result<HashMap<QTableId, crate::lower::vt::DrCrAreaL>, ExecError> {
    let mut areas = HashMap::new();
    let mut filters = None;
    for (_, source, _) in decisions {
        if source.part != TablePart::Virtual(VtKind::DrCrTurnovers)
            || areas.contains_key(&source.table)
        {
            continue;
        }
        let inputs = crate::lower::drcr_inputs(ctx.schema, ctx.storage, ctx.map, source.table);
        if inputs.area_mapping()?.is_none() {
            continue;
        }
        if filters.is_none() {
            filters = Some(ctx.security.session.separators().map_err(ExecError::Host)?);
        }
        if let Some(area) = inputs.bind_area(filters.as_deref().unwrap_or(&[]))? {
            areas.insert(source.table, area);
        }
    }
    Ok(areas)
}

pub(in crate::bind) fn prepare(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    charge: &mut dyn FnMut(&crate::bind::PolicyCompileStats) -> Result<(), ExecError>,
) -> Result<(Option<StmtS>, Security), ExecError> {
    prepare_with_origins(stmt, ctx, charge, None)
}

pub(in crate::bind) fn prepare_with_origins(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    charge: &mut dyn FnMut(&crate::bind::PolicyCompileStats) -> Result<(), ExecError>,
    temps: Option<&dyn crate::bind::temp_provenance::TempOriginProvider>,
) -> Result<(Option<StmtS>, Security), ExecError> {
    let Discovered { decisions, aliases } = discover(stmt, ctx, temps)?;
    let schema = ctx.schema;
    query_syntax::with_deep_stack(stmt.max_depth, || {
        super::calculation::membership_guard(stmt, &decisions, &aliases)
    })?;
    query_syntax::with_deep_stack(stmt.max_depth, || {
        super::accounting::membership_guard(stmt, &decisions, &aliases, schema)
    })?;
    if decisions.iter().any(|(key, _, access)| {
        let deferred = match key {
            Occurrence::Presentation(..) | Occurrence::HierarchyTarget(..) => true,
            Occurrence::Target(path, _) => matches!(path.root, Root::Opaque(_)),
            _ => false,
        };
        deferred && matches!(access, Access::Restricted(_))
    }) {
        return Err(ExecError::Later {
            what: "Q7b-DeferredSecurityRead",
            stage: "Q7c",
        });
    }
    let (schema, storage, map) = (ctx.schema, ctx.storage, ctx.map);
    query_syntax::with_deep_stack(stmt.max_depth, || {
        super::drcr_ir_guards(stmt, schema, storage, map, &aliases)
    })?;
    query_syntax::with_deep_stack(stmt.max_depth, || {
        super::turnovers_ir_guards(stmt, schema, storage, map, &aliases)
    })?;
    if !decisions
        .iter()
        .any(|(_, _, a)| matches!(a, Access::Restricted(_)))
    {
        return Ok((
            None,
            Security {
                conditions: HashMap::new(),
                targets: HashMap::new(),
                aliases: HashMap::new(),
                accounting_sources: HashMap::new(),
                calculation_sources: HashMap::new(),
                drcr_areas: drcr_areas(&decisions, ctx)?,
                max_depth: 0,
            },
        ));
    }
    let mut stats = crate::bind::PolicyCompileStats {
        statement: crate::ir::stats::stats(stmt),
        ..Default::default()
    };
    let mut max_depth = stmt.max_depth;
    for (_, _, access) in &decisions {
        if let Access::Restricted(predicates) = access {
            for p in predicates {
                max_depth = max_depth.max(p.max_depth.saturating_add(16));
                let s = crate::sema::stats::predicate_stats(&p.form);
                let expansion = s
                    .nodes
                    .saturating_add(s.fanout_sum)
                    .saturating_add(s.sources)
                    .saturating_add(16);
                max_depth = max_depth.saturating_add(u32::try_from(expansion).unwrap_or(u32::MAX));
                let t = &mut stats.predicates;
                t.nodes += s.nodes + 16;
                t.columns += s.columns + 1;
                t.fanout_sum += s.fanout_sum;
                t.joins += s.joins;
                t.subqueries += s.subqueries + 1;
                t.sources += s.sources + 1;
                t.type_refs += s.type_refs;
            }
        }
    }
    super::calculation::reserve_expansion(&mut stats, &decisions, schema)?;
    let drcr_areas = drcr_areas(&decisions, ctx)?;
    let mut pending = Vec::new();
    let mut names_seen = std::collections::HashSet::new();
    let mut copied_payload = 0_u64;
    for (_, _, access) in &decisions {
        if let Access::Restricted(predicates) = access {
            for p in predicates {
                for name in &p.params {
                    let folded = bsl_prim::fold_name(name);
                    if names_seen.insert(folded.clone()) {
                        let value = ctx
                            .security
                            .session
                            .session_param(name)
                            .map_err(ExecError::Host)?;
                        let value_bytes = super::payload::bytes(&value)?;
                        let clone_bytes = super::payload::clone_bytes(&value)?;
                        let name_bytes = super::payload::name_bytes(&folded)?;
                        stats.payload_bytes = stats
                            .payload_bytes
                            .checked_add(value_bytes)
                            .ok_or_else(super::payload::overflow)?;
                        copied_payload = copied_payload
                            .checked_add(clone_bytes)
                            .and_then(|bytes| bytes.checked_add(name_bytes))
                            .ok_or_else(super::payload::overflow)?;
                        pending.push((folded, value));
                    }
                }
            }
        }
    }
    let copies = stats
        .predicates
        .nodes
        .checked_add(stats.predicates.sources)
        .and_then(|n| n.checked_add(4))
        .ok_or_else(super::payload::overflow)?;
    let copied_payload = copied_payload
        .checked_mul(copies)
        .ok_or_else(super::payload::overflow)?;
    stats.payload_bytes = stats
        .payload_bytes
        .checked_add(copied_payload)
        .ok_or_else(super::payload::overflow)?;
    charge(&stats)?;
    let session: HashMap<_, _> = pending.into_iter().collect();
    let (schema, storage, map) = (ctx.schema, ctx.storage, ctx.map);
    query_syntax::with_deep_stack(max_depth, || {
        let mut secured = stmt.clone();
        secured.max_depth = max_depth;
        let mut security = Security::default();
        security.max_depth = max_depth;
        security.aliases = aliases;
        security.drcr_areas = drcr_areas;
        for (key, mut source, decision) in decisions {
            let Access::Restricted(predicates) = decision else {
                continue;
            };
            let src = match key {
                Occurrence::Direct(view)
                    if crate::lower::is_accounting_balance(schema, source.table)
                        || crate::lower::is_accounting_turnovers(schema, source.table) =>
                {
                    let owner = schema
                        .table(source.table)
                        .owner
                        .ok_or_else(|| ExecError::Fatal("accounting policy owner".into()))?;
                    let src = policy_source(&mut secured, owner)?;
                    security
                        .accounting_sources
                        .entry(view)
                        .or_default()
                        .register = Some(src);
                    source.table = owner;
                    src
                }
                Occurrence::BalanceChart(key) => {
                    let Occurrence::Direct(view) = *key else {
                        return Err(super::accounting::refusal(true));
                    };
                    let src = policy_source(&mut secured, source.table)?;
                    security.accounting_sources.entry(view).or_default().chart = Some(src);
                    src
                }
                Occurrence::Direct(view) if matches!(source.part, TablePart::Virtual(kind) if super::calculation::code(kind).is_some()) =>
                {
                    let owner = schema
                        .table(source.table)
                        .owner
                        .ok_or_else(|| ExecError::Fatal("calculation policy owner".into()))?;
                    let src = policy_source(&mut secured, owner)?;
                    security.calculation_sources.entry(view).or_default().main = Some(src);
                    source.table = owner;
                    src
                }
                Occurrence::Schedule(parent) => {
                    let Occurrence::Direct(view) = *parent else {
                        return Err(super::calculation::refusal("Q6c-ScheduleDataScheduleRls"));
                    };
                    let src = policy_source(&mut secured, source.table)?;
                    security.calculation_sources.entry(view).or_default().graph = Some(src);
                    src
                }
                Occurrence::Direct(src) => src,
                Occurrence::Target(path, table) => {
                    let src = SrcId(
                        u32::try_from(secured.sources.len())
                            .map_err(|_| ExecError::Fatal("too many security sources".into()))?,
                    );
                    secured.sources.push(SourceS {
                        id: src,
                        kind: SrcKind::Table(table),
                        all_fields: false,
                    });
                    security.targets.insert((path, table), src);
                    src
                }
                _ => {
                    return Err(ExecError::Later {
                        what: "Q7b-IndirectSecuritySource",
                        stage: "Q7b",
                    });
                }
            };
            let mut conditions = Vec::with_capacity(predicates.len());
            for predicate in predicates {
                if predicate.is_full()
                    && storage
                        .table(predicate.table())
                        .and_then(|t| map.try_table(t))
                        .is_some_and(|t| !t.separators.is_empty())
                {
                    return Err(ExecError::Later {
                        what: "Q7b-FullFormAreaIdentity",
                        stage: "Q7c",
                    });
                }
                let mut policy = predicate.read_stmt(schema)?;
                let mut ids = Vec::with_capacity(policy.sources.len());
                for (i, s) in policy.sources.iter().enumerate() {
                    if i == 0 {
                        ids.push(src);
                    } else {
                        let id = SrcId(
                            u32::try_from(secured.sources.len())
                                .map_err(|_| ExecError::Fatal("too many policy sources".into()))?,
                        );
                        let mut s = s.clone();
                        s.id = id;
                        secured.sources.push(s);
                        ids.push(id);
                    }
                }
                let params = policy
                    .params
                    .iter()
                    .map(|name| {
                        session
                            .get(&bsl_prim::fold_name(name))
                            .cloned()
                            .ok_or_else(|| ExecError::Fatal("unprepared session value".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let value_offset = u16::try_from(secured.values.len())
                    .map_err(|_| ExecError::Fatal("too many policy values".into()))?;
                secured.values.append(&mut policy.values);
                for t in &policy.tables {
                    if !secured.tables.contains(t) {
                        secured.tables.push(*t);
                    }
                }
                let owner = predicate.table();
                let mut remap = |e: &mut Expr| -> Result<(), ExecError> {
                    match &mut e.kind {
                        E::Col { src: s, col } => {
                            let original = *s;
                            *s = *ids.get(s.0 as usize).ok_or_else(|| {
                                ExecError::Fatal("policy source out of range".into())
                            })?;
                            if original == SrcId(0) && owner != source.table {
                                let ColSel::Field(field) = *col else {
                                    return Err(ExecError::Fatal("policy owner column".into()));
                                };
                                let target_field =
                                    schema.table(source.table).fields.iter().enumerate().find(
                                        |(_, f)| {
                                            f.role
                                                == crate::schema::FieldRole::Std(
                                                    crate::schema::StdRole::Ref,
                                                )
                                        },
                                    );
                                if matches!(source.part, TablePart::Tabular(_))
                                    && schema.table(source.table).owner == Some(owner)
                                {
                                    let (i, f) = target_field.ok_or_else(|| {
                                        ExecError::Fatal("section owner reference".into())
                                    })?;
                                    let base = Expr {
                                        kind: E::Col {
                                            src,
                                            col: ColSel::Field(crate::QFieldId(
                                                u16::try_from(i).map_err(|_| {
                                                    ExecError::Fatal(
                                                        "section reference index".into(),
                                                    )
                                                })?,
                                            )),
                                        },
                                        ty: f.ty.clone(),
                                    };
                                    e.kind = E::Deref {
                                        base: Box::new(base),
                                        steps: vec![crate::tir::Deref {
                                            name: schema
                                                .field(owner, field)
                                                .map(|f| f.name_ru.clone())
                                                .unwrap_or_default(),
                                            targets: vec![(owner, field)],
                                        }],
                                    };
                                } else {
                                    let f = schema
                                        .field(owner, field)
                                        .ok_or_else(|| ExecError::Fatal("policy field".into()))?;
                                    let mapped =
                                        schema.table(source.table).fields.iter().position(|g| {
                                            (f.md.is_some() && f.md == g.md)
                                                || (matches!(
                                                    f.role,
                                                    crate::schema::FieldRole::Std(_)
                                                ) && f.role == g.role)
                                        });
                                    let Some(mapped) = mapped else {
                                        return Err(ExecError::Later {
                                            what: "Q7b-VtRestrictionField",
                                            stage: "Q5/Q6",
                                        });
                                    };
                                    *col = ColSel::Field(crate::QFieldId(
                                        u16::try_from(mapped).map_err(|_| {
                                            ExecError::Fatal("VT policy field index".into())
                                        })?,
                                    ));
                                }
                            }
                        }
                        E::Param(i) => {
                            e.kind = E::Lit(
                                params
                                    .get(usize::from(*i))
                                    .cloned()
                                    .ok_or_else(|| ExecError::Fatal("policy parameter".into()))?,
                            );
                        }
                        E::Value(i) => {
                            *i = i
                                .checked_add(value_offset)
                                .ok_or_else(|| ExecError::Fatal("too many policy values".into()))?
                        }
                        E::OuterKey { src, .. } => {
                            *src = *ids
                                .get(src.0 as usize)
                                .ok_or_else(|| ExecError::Fatal("policy outer source".into()))?
                        }
                        _ => {}
                    }
                    Ok(())
                };
                remap_rel(&mut policy.root, &ids, &mut remap)?;
                let Rel::Project { exprs, .. } = &mut policy.root else {
                    return Err(ExecError::Fatal("policy read projection".into()));
                };
                if exprs.len() != 1 {
                    return Err(ExecError::Fatal("policy read width".into()));
                }
                conditions.push(exprs.remove(0));
                secured.max_depth = secured.max_depth.max(policy.max_depth).saturating_add(4);
            }
            security.conditions.insert(
                src,
                Expr {
                    kind: E::Or(conditions),
                    ty: crate::QType::boolean(),
                },
            );
        }
        Ok((Some(secured), security))
    })
}

fn policy_source(stmt: &mut StmtS, table: QTableId) -> Result<SrcId, ExecError> {
    let src = SrcId(
        u32::try_from(stmt.sources.len())
            .map_err(|_| ExecError::Fatal("too many accounting policy sources".into()))?,
    );
    stmt.sources.push(SourceS {
        id: src,
        kind: SrcKind::Table(table),
        all_fields: false,
    });
    if !stmt.tables.contains(&table) {
        stmt.tables.push(table);
    }
    Ok(src)
}

fn remap_expr(
    e: &mut Expr,
    ids: &[SrcId],
    f: &mut dyn FnMut(&mut Expr) -> Result<(), ExecError>,
) -> Result<(), ExecError> {
    match &mut e.kind {
        E::Deref { base: x, .. }
        | E::Neg(x)
        | E::Not(x)
        | E::IsNull { l: x, .. }
        | E::Refs { l: x, .. }
        | E::Cast { x, .. }
        | E::LocalKey { orig: x, .. }
        | E::OuterKey { orig: x, .. } => remap_expr(x, ids, f)?,
        E::Arith { first, steps } => {
            remap_expr(first, ids, f)?;
            for (_, e, _) in steps {
                remap_expr(e, ids, f)?;
            }
        }
        E::And(es) | E::Or(es) | E::Func { args: es, .. } => {
            for e in es {
                remap_expr(e, ids, f)?;
            }
        }
        E::Compare(_, a, b) => {
            remap_expr(a, ids, f)?;
            remap_expr(b, ids, f)?;
        }
        E::InList { l, rhs, .. } => {
            for e in l.iter_mut().chain(rhs) {
                remap_expr(e, ids, f)?;
            }
        }
        E::InQuery { l, q, .. } => {
            for e in l {
                remap_expr(e, ids, f)?;
            }
            remap_rel(q, ids, f)?;
        }
        E::SectionAgg { owner, rel, .. } => {
            remap_expr(owner, ids, f)?;
            remap_rel(rel, ids, f)?;
        }
        E::Between { l, lo, hi, .. } => {
            for e in [l, lo, hi] {
                remap_expr(e, ids, f)?;
            }
        }
        E::Like { l, pat, .. } => {
            remap_expr(l, ids, f)?;
            remap_expr(pat, ids, f)?;
        }
        E::Case {
            operand,
            whens,
            else_,
        } => {
            for e in operand.iter_mut().chain(else_) {
                remap_expr(e, ids, f)?;
            }
            for (a, b) in whens {
                remap_expr(a, ids, f)?;
                remap_expr(b, ids, f)?;
            }
        }
        _ => {}
    }
    f(e)
}
fn remap_rel(
    r: &mut Rel,
    ids: &[SrcId],
    f: &mut dyn FnMut(&mut Expr) -> Result<(), ExecError>,
) -> Result<(), ExecError> {
    let id = |src: &mut SrcId| -> Result<(), ExecError> {
        *src = *ids
            .get(src.0 as usize)
            .ok_or_else(|| ExecError::Fatal("policy relation source".into()))?;
        Ok(())
    };
    match r {
        Rel::Empty => {}
        Rel::Scan { src, .. } | Rel::TempScan { src, .. } | Rel::ParamScan { src, .. } => id(src)?,
        Rel::Subquery { src, input } => {
            id(src)?;
            remap_rel(input, ids, f)?;
        }
        Rel::Filter { input, pred } => {
            remap_rel(input, ids, f)?;
            remap_expr(pred, ids, f)?;
        }
        Rel::Project { input, exprs } => {
            remap_rel(input, ids, f)?;
            for e in exprs {
                remap_expr(e, ids, f)?;
            }
        }
        Rel::Join { l, r, on, .. } => {
            remap_rel(l, ids, f)?;
            remap_rel(r, ids, f)?;
            remap_expr(on, ids, f)?;
        }
        Rel::Aggregate {
            input, keys, aggs, ..
        } => {
            remap_rel(input, ids, f)?;
            for e in keys {
                remap_expr(e, ids, f)?;
            }
            for e in aggs.iter_mut().filter_map(|a| a.arg.as_mut()) {
                remap_expr(e, ids, f)?;
            }
        }
        Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
            remap_rel(input, ids, f)?;
            for k in keys {
                remap_expr(&mut k.expr, ids, f)?;
            }
        }
        Rel::Top { input, .. } | Rel::Distinct(input) => remap_rel(input, ids, f)?,
        Rel::Union { parts, .. } => {
            for r in parts {
                remap_rel(r, ids, f)?;
            }
        }
        Rel::RegisterVt(_) | Rel::Slice(_) | Rel::RunningBalance(_) => {
            return Err(ExecError::Fatal("virtual source in compiled policy".into()));
        }
    }
    Ok(())
}

pub(crate) fn reanchor(
    expr: &Expr,
    from: SrcId,
    to: SrcId,
    sources: usize,
) -> Result<Expr, ExecError> {
    let mut out = expr.clone();
    let ids = (0..sources)
        .map(|i| {
            if i == from.0 as usize {
                to
            } else {
                SrcId(i as u32)
            }
        })
        .collect::<Vec<_>>();
    remap_expr(&mut out, &ids, &mut |e| {
        if let E::Col { src, .. } = &mut e.kind
            && *src == from
        {
            *src = to;
        }
        Ok(())
    })?;
    Ok(out)
}
