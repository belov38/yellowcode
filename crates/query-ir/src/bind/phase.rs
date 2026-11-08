use super::{BindCtx, ExecError};
use crate::ir::{ColSel, ExprKind, SrcKind, StmtS};
use crate::{FieldRole, QFieldId, QTableId, QuerySchema, StdRole};
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
            used |= schema
                .field(table, field)
                .is_some_and(|field| field.role == FieldRole::Std(StdRole::ExtDimension(1)));
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
        for source in &stmt.sources {
            let SrcKind::Virtual { table, .. } = source.kind else {
                continue;
            };
            if !crate::lower::is_accounting_balance(ctx.schema, table) {
                continue;
            }
            let mut first = false;
            self.fields(stmt, table, Some(source.id), |field| {
                first |= schema
                    .field(table, field)
                    .is_some_and(|field| field.role == FieldRole::Std(StdRole::ExtDimension(1)));
            });
            if !first {
                continue;
            }
            let mut roles = BudgetedVec::new(&memory.budget, Category::Execution)
                .map_err(crate::lower::accounting_balance_payload::memory_error)?;
            let mut resources = BudgetedVec::new(&memory.budget, Category::Execution)
                .map_err(crate::lower::accounting_balance_payload::memory_error)?;
            let mut result = Ok(());
            self.fields(stmt, table, Some(source.id), |field| {
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
            result?;
            let all = source.all_fields;
            crate::lower::balance_form(ctx.schema, table, all, &roles, &resources, self)?;
            let inputs = crate::lower::balance_inputs(ctx.schema, ctx.storage, ctx.map, table)?;
            crate::lower::balance_resources(ctx.schema, ctx.map, table, &inputs, &resources)?;
            crate::lower::balance_ext1_static_inputs(
                ctx.schema,
                ctx.storage,
                ctx.map,
                table,
                &inputs,
                &resources,
            )?;
        }
        Ok(())
    }
}
