use crate::WorkerStatement;
use crate::managed::{CompileArena, NativeOwned, ir as paid};
use memory_budget::{BudgetError, RequestBudget, ResourceCause, ResourceFailure, ResourcePhase};

pub struct WorkerBuild<'a, 'r, 'w> {
    arena: CompileArena,
    request: &'a RequestBudget<'r, 'w>,
}

pub struct WorkerBuilt<'a, 'r, 'w> {
    owner: NativeOwned<'a, crate::ir::StmtS>,
    _statement: &'a WorkerStatement<'a, 'r, 'w>,
}

pub struct WorkerBuildFailure<'a, 'r, 'w> {
    body: paid::BuildProblem,
    _request: &'a RequestBudget<'r, 'w>,
}

impl WorkerBuildFailure<'_, '_, '_> {
    pub fn resource(&self) -> Option<ResourceFailure> {
        match self.body {
            paid::BuildProblem::Resource(error) => Some(error),
            _ => None,
        }
    }
    pub fn later(&self) -> Option<(&'static str, &'static str)> {
        match self.body {
            paid::BuildProblem::Later { what, stage } => Some((what, stage)),
            _ => None,
        }
    }
    pub fn internal(&self) -> Option<&'static str> {
        match self.body {
            paid::BuildProblem::Internal(text) => Some(text),
            _ => None,
        }
    }
}

impl WorkerBuilt<'_, '_, '_> {
    pub fn column_count(&self) -> usize {
        self.owner.trusted_borrow().columns.len()
    }
    pub fn column_name(&self, index: usize) -> Option<&str> {
        self.owner
            .trusted_borrow()
            .columns
            .get(index)
            .map(|v| v.name.as_str())
    }
    pub fn column_width(&self, index: usize) -> Option<u32> {
        self.owner
            .trusted_borrow()
            .columns
            .get(index)
            .map(|v| v.width)
    }
    pub fn parameter_count(&self) -> usize {
        self.owner.trusted_borrow().params.len()
    }
    pub fn parameter_name(&self, index: usize) -> Option<&str> {
        self.owner
            .trusted_borrow()
            .params
            .get(index)
            .map(String::as_str)
    }
    pub fn graph_depth(&self) -> u16 {
        self.owner.depth()
    }
}

impl<'a, 'r, 'w> WorkerBuild<'a, 'r, 'w> {
    pub fn new(request: &'a RequestBudget<'r, 'w>) -> Result<Self, ResourceFailure> {
        request.checkpoint(ResourcePhase::Build)?;
        let arena = CompileArena::child(request.memory(), 0)
            .map_err(|error| request.record(resource(error)))?;
        Ok(Self { arena, request })
    }
    pub fn build<'s>(
        &'s self,
        statement: &'s WorkerStatement<'a, 'r, 'w>,
    ) -> Result<WorkerBuilt<'s, 'r, 'w>, WorkerBuildFailure<'s, 'r, 'w>> {
        let ctx = paid::BuildCtx {
            arena: &self.arena,
            request: self.request,
        };
        let result = (|| {
            ctx.checkpoint()?;
            let (input, schema, request) = statement.build_input();
            if !std::ptr::eq(request, self.request) {
                return Err(ctx.refuse(BudgetError::Overflow));
            }
            paid::build(&ctx, input, schema)
        })();
        result
            .map(|owner| WorkerBuilt {
                owner,
                _statement: statement,
            })
            .map_err(|body| WorkerBuildFailure {
                body,
                _request: self.request,
            })
    }
}

pub(crate) fn resource(error: BudgetError) -> ResourceFailure {
    ResourceFailure {
        phase: ResourcePhase::Build,
        cause: ResourceCause::Memory(error),
    }
}
