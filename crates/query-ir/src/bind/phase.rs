use super::{BindCtx, ExecError};
use crate::ir::{ColSel, ExprKind, SrcKind, StmtS};
use crate::{FieldRole, QFieldId, QTableId, QuerySchema};
use memory_budget::{BudgetedVec, Category, RequestMemory};

#[derive(Clone, Copy)]
pub(crate) struct PhaseContext<'a> {
    temp_origins: Option<&'a dyn super::temp_provenance::TempOriginProvider>,
    memory: Option<&'a RequestMemory>,
}

impl<'a> PhaseContext<'a> {
    pub(crate) const fn new(memory: Option<&'a RequestMemory>) -> Self {
        Self::with_admission(None, memory)
    }

    pub(crate) const fn with_admission(
        temp_origins: Option<&'a dyn super::temp_provenance::TempOriginProvider>,
        memory: Option<&'a RequestMemory>,
    ) -> Self {
        Self {
            temp_origins,
            memory,
        }
    }

    pub(crate) const fn temp_origins(
        self,
    ) -> Option<&'a dyn super::temp_provenance::TempOriginProvider> {
        self.temp_origins
    }

    pub(crate) const fn memory(self) -> Option<&'a RequestMemory> {
        self.memory
    }

    pub(crate) const fn enabled(self) -> bool {
        self.memory.is_some()
    }

    fn fields(
        self,
        stmt: &StmtS,
        table: QTableId,
        only: Option<crate::ir::SrcId>,
        mut visit: impl FnMut(QFieldId) + Send,
    ) {
        if !self.enabled() {
            return;
        }
        query_syntax::with_deep_stack(stmt.max_depth, || {
            super::visit_stmt(stmt, &mut |expr, _| {
                let ExprKind::Col { src, col } = expr.kind else {
                    return;
                };
                if only.is_some_and(|selected| selected != src) {
                    return;
                }
                let Some(source) = stmt.sources.get(src.0 as usize) else {
                    return;
                };
                let SrcKind::Virtual {
                    table: actual,
                    ref cols,
                } = source.kind
                else {
                    return;
                };
                if actual != table {
                    return;
                }
                match col {
                    ColSel::Field(field) => visit(field),
                    ColSel::Column(index) => {
                        if let Some(field) = cols.get(usize::from(index)).copied().flatten() {
                            visit(field);
                        }
                    }
                }
            });
        });
    }

    pub(crate) fn uses_ext1(self, stmt: &StmtS, schema: &QuerySchema, table: QTableId) -> bool {
        if !self.enabled() {
            return false;
        }
        let mut used = false;
        self.fields(stmt, table, None, |field| {
            used |= schema.field(table, field).is_some_and(|field| {
                matches!(field.role, FieldRole::Std(role) if crate::lower::is_balance_ext_dimension(role))
            });
        });
        used
    }

    pub(crate) fn section(
        self,
        stmt: &StmtS,
        schema: &QuerySchema,
        table: QTableId,
    ) -> Option<QTableId> {
        if !self.uses_ext1(stmt, schema, table) {
            return None;
        }
        crate::lower::balance_ext1_section_identity(schema, table)
    }

    pub(crate) fn validate_trusted(self, stmt: &StmtS, ctx: &BindCtx<'_>) -> Result<(), ExecError> {
        let Some(memory) = self.memory else {
            return Ok(());
        };
        let schema = ctx.schema;
        let mut inner_to_outer = std::collections::HashMap::new();
        fn collect_inner(
            rel: &crate::ir::Rel,
            map: &mut std::collections::HashMap<crate::ir::SrcId, crate::ir::SrcId>,
        ) {
            match rel {
                crate::ir::Rel::RegisterVt(v)
                | crate::ir::Rel::Slice(v)
                | crate::ir::Rel::RunningBalance(v) => {
                    map.insert(v.inner, v.src);
                }
                crate::ir::Rel::Subquery { input, .. }
                | crate::ir::Rel::Filter { input, .. }
                | crate::ir::Rel::Aggregate { input, .. }
                | crate::ir::Rel::Project { input, .. }
                | crate::ir::Rel::Distinct(input)
                | crate::ir::Rel::Sort { input, .. }
                | crate::ir::Rel::Top { input, .. }
                | crate::ir::Rel::Hierarchy { input, .. } => collect_inner(input, map),
                crate::ir::Rel::Join { l, r, .. } => {
                    collect_inner(l, map);
                    collect_inner(r, map);
                }
                crate::ir::Rel::Union { parts, .. } => {
                    for p in parts {
                        collect_inner(p, map);
                    }
                }
                crate::ir::Rel::Empty
                | crate::ir::Rel::Scan { .. }
                | crate::ir::Rel::TempScan { .. }
                | crate::ir::Rel::ParamScan { .. } => {}
            }
        }
        collect_inner(&stmt.root, &mut inner_to_outer);
        for rows in stmt.nested.iter().filter_map(|n| n.rows.as_ref()) {
            collect_inner(&rows.scan, &mut inner_to_outer);
        }
        super::visit_stmt(stmt, &mut |e, _| match &e.kind {
            ExprKind::InQuery { q, .. } => collect_inner(q, &mut inner_to_outer),
            ExprKind::SectionAgg { rel, .. } => collect_inner(rel, &mut inner_to_outer),
            _ => {}
        });
        for source in &stmt.sources {
            if inner_to_outer.contains_key(&source.id) {
                continue;
            }
            let SrcKind::Virtual { table, .. } = source.kind else {
                continue;
            };
            if !crate::lower::is_accounting_balance(ctx.schema, table) {
                continue;
            }
            let inner_id = inner_to_outer
                .iter()
                .find_map(|(&inner, &outer)| (outer == source.id).then_some(inner));
            let mut has_ext = false;
            for src in std::iter::once(source.id).chain(inner_id) {
                self.fields(stmt, table, Some(src), |field| {
                    has_ext |= schema.field(table, field).is_some_and(|field| {
                        matches!(field.role, FieldRole::Std(role) if crate::lower::is_balance_ext_dimension(role))
                    });
                });
            }
            if !has_ext {
                continue;
            }
            let mut roles = BudgetedVec::new(&memory.budget, Category::Execution)
                .map_err(crate::lower::accounting_balance_payload::memory_error)?;
            let mut resources = BudgetedVec::new(&memory.budget, Category::Execution)
                .map_err(crate::lower::accounting_balance_payload::memory_error)?;
            let mut result = Ok(());
            for src in std::iter::once(source.id).chain(inner_id) {
                self.fields(stmt, table, Some(src), |field| {
                    if result.is_err() {
                        return;
                    }
                    if let Some(field) = schema.field(table, field) {
                        if let FieldRole::Std(role) = field.role
                            && !roles.contains(&role)
                        {
                            result = roles
                                .try_push(role)
                                .map_err(crate::lower::accounting_balance_payload::memory_error);
                        }
                        if result.is_ok()
                            && let Some(md) = field.md
                            && !resources.contains(&md)
                        {
                            result = resources
                                .try_push(md)
                                .map_err(crate::lower::accounting_balance_payload::memory_error);
                        }
                    }
                });
            }
            result?;
            let all = source.all_fields;
            crate::lower::balance_form(ctx.schema, table, all, &roles, &resources, self)?;
            let inputs = crate::lower::balance_inputs(ctx.schema, ctx.storage, ctx.map, table)?;
            crate::lower::balance_dims(
                ctx.schema,
                ctx.storage,
                ctx.map,
                table,
                &inputs,
                &resources,
            )?;
            crate::lower::balance_resources(ctx.schema, ctx.map, table, &inputs, &resources)?;
            crate::lower::balance_ext1_static_inputs(
                ctx.schema,
                ctx.storage,
                ctx.map,
                table,
                &inputs,
                &roles,
                &resources,
            )?;
        }
        Ok(())
    }
}
