use crate::managed::{self as mem, CompileArena, DiagnosticProjection, Failure, NativeOwned};
use crate::{AnalyzeMode, ManagedParamTypes, ManagedTempSchemas, QuerySchema};
use memory_budget::{BudgetError, RequestBudget, ResourceCause, ResourceFailure, ResourcePhase};
use query_syntax::{ErrAt, Func, Kw, MsgId, WorkerParsed};

pub struct WorkerSources<'a, 'r, 'w> {
    request: &'a RequestBudget<'r, 'w>,
    schema: &'a QuerySchema,
    temps: &'a dyn ManagedTempSchemas,
    params: &'a dyn ManagedParamTypes,
}
impl<'a, 'r, 'w> WorkerSources<'a, 'r, 'w> {
    pub unsafe fn from_audited(
        request: &'a RequestBudget<'r, 'w>,
        schema: &'a QuerySchema,
        temps: &'a dyn ManagedTempSchemas,
        params: &'a dyn ManagedParamTypes,
    ) -> Self {
        Self {
            request,
            schema,
            temps,
            params,
        }
    }
}

pub struct WorkerAnalysis<'a, 'r, 'w> {
    arena: CompileArena,
    request: &'a RequestBudget<'r, 'w>,
}
pub struct WorkerStatement<'a, 'r, 'w> {
    owner: NativeOwned<'a, mem::statement::Statement>,
    _parsed: &'a WorkerParsed<'a, 'r, 'w>,
    _sources: &'a WorkerSources<'a, 'r, 'w>,
}
impl<'a, 'r, 'w> WorkerStatement<'a, 'r, 'w> {
    pub(crate) fn build_input(
        &self,
    ) -> (
        &mem::statement::Statement,
        &QuerySchema,
        &'a RequestBudget<'r, 'w>,
    ) {
        (
            self.owner.trusted_borrow(),
            self._sources.schema,
            self._sources.request,
        )
    }
}
impl WorkerStatement<'_, '_, '_> {
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
            .map(|v| v.name.as_str())
    }
    pub fn graph_depth(&self) -> u16 {
        self.owner.depth()
    }
    pub fn temp_output_name(&self) -> Option<&str> {
        match &self.owner.trusted_borrow().temp_out {
            Some(mem::statement::TempOutput::Put(schema)) => Some(&schema.name),
            Some(mem::statement::TempOutput::Add(name)) => Some(name),
            None => None,
        }
    }
    pub fn drop_name(&self) -> Option<&str> {
        self.owner.trusted_borrow().drop.as_deref()
    }
    pub fn read_count(&self) -> usize {
        self.owner.trusted_borrow().reads.len()
    }
    pub fn read_name(&self, index: usize) -> Option<&str> {
        self.owner
            .trusted_borrow()
            .reads
            .get(index)
            .map(String::as_str)
    }
    pub fn value_dependency_count(&self) -> usize {
        self.owner.trusted_borrow().value_deps.len()
    }
    pub fn value_dependency(&self, index: usize) -> Option<&str> {
        self.owner
            .trusted_borrow()
            .value_deps
            .get(index)
            .map(String::as_str)
    }
    pub fn table_parameter_count(&self) -> usize {
        self.owner.trusted_borrow().table_params.len()
    }
    pub fn table_parameter_name(&self, index: usize) -> Option<&str> {
        self.owner
            .trusted_borrow()
            .table_params
            .get(index)
            .map(String::as_str)
    }
    pub fn is_query(&self) -> bool {
        matches!(
            self.owner.trusted_borrow().typed.kind,
            crate::tir::StmtKind::Query(_)
        )
    }
}
enum FailureBody<'a, 'r, 'w> {
    Resource(ResourceFailure),
    Syntax(&'a WorkerParsed<'a, 'r, 'w>),
    Managed(Failure),
}
pub struct WorkerFailure<'a, 'r, 'w> {
    body: FailureBody<'a, 'r, 'w>,
    _request: &'a RequestBudget<'r, 'w>,
}
impl WorkerFailure<'_, '_, '_> {
    pub fn resource(&self) -> Option<ResourceFailure> {
        match self.body {
            FailureBody::Resource(value) => Some(value),
            _ => None,
        }
    }
    pub fn diagnostic(&self) -> Option<WorkerDiagnostic<'_>> {
        let value = match &self.body {
            FailureBody::Resource(_) => None,
            FailureBody::Managed(value) => value.projection(),
            FailureBody::Syntax(parsed) => unsafe {
                parsed.consume_on_worker(|native, _, _, _, _| {
                    native
                        .batch
                        .as_ref()
                        .err()
                        .map(DiagnosticProjection::syntax)
                })
            },
        };
        value.map(|value| WorkerDiagnostic {
            value,
            _confined: std::marker::PhantomData,
        })
    }
}
#[derive(Clone, Copy)]
pub enum WorkerArgument<'a> {
    Text(&'a str),
    Keyword(Kw),
    Function(Func),
    Message(query_syntax::MsgId),
}
#[derive(Clone, Copy)]
pub struct WorkerDiagnostic<'a> {
    value: DiagnosticProjection<'a>,
    _confined: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl<'a> WorkerDiagnostic<'a> {
    pub fn message(self) -> MsgId {
        self.value.message()
    }
    pub fn at(self) -> Option<ErrAt> {
        self.value.at()
    }
    pub fn detail(self) -> Option<&'a str> {
        self.value.detail()
    }
    pub fn argument_count(self) -> usize {
        self.value.count()
    }
    pub fn argument(self, index: usize) -> Option<WorkerArgument<'a>> {
        self.value.argument(index).map(argument)
    }
    pub fn quoted(self) -> Option<WorkerArgument<'a>> {
        self.value.quoted().map(argument)
    }
    pub fn cause(self) -> Option<Self> {
        self.value.cause().map(|value| Self {
            value,
            _confined: std::marker::PhantomData,
        })
    }
}
fn argument(value: mem::ProjectedArgument<'_>) -> WorkerArgument<'_> {
    match value {
        mem::ProjectedArgument::Text(v) => WorkerArgument::Text(v),
        mem::ProjectedArgument::Kw(v) => WorkerArgument::Keyword(v),
        mem::ProjectedArgument::Func(v) => WorkerArgument::Function(v),
        mem::ProjectedArgument::Msg(v) => WorkerArgument::Message(v),
    }
}
impl<'a, 'r, 'w> WorkerAnalysis<'a, 'r, 'w> {
    pub fn new(request: &'a RequestBudget<'r, 'w>) -> Result<Self, ResourceFailure> {
        request.checkpoint(ResourcePhase::Analyze)?;
        let arena = CompileArena::child(request.memory(), 0)
            .map_err(|error| request.record(resource(error)))?;
        Ok(Self { arena, request })
    }
    fn failed<'s>(&'s self, body: FailureBody<'s, 'r, 'w>) -> WorkerFailure<'s, 'r, 'w> {
        WorkerFailure {
            body,
            _request: self.request,
        }
    }
    pub fn analyze<'s>(
        &'s self,
        parsed: &'s WorkerParsed<'a, 'r, 'w>,
        sources: &'s WorkerSources<'a, 'r, 'w>,
        index: usize,
        mode: AnalyzeMode,
        inter: bool,
    ) -> Result<WorkerStatement<'s, 'r, 'w>, WorkerFailure<'s, 'r, 'w>> {
        if let Some(error) = self.arena.error() {
            return Err(self.failed(FailureBody::Resource(self.request.record(resource(error)))));
        }
        self.request
            .checkpoint(ResourcePhase::Analyze)
            .map_err(|e| self.failed(FailureBody::Resource(e)))?;
        let result = unsafe {
            parsed.consume_on_worker(|native, map, text, request, depth| {
                if !std::ptr::eq(request, self.request)
                    || !std::ptr::eq(sources.request, self.request)
                {
                    return mem::allocation_error(Some(&self.arena), BudgetError::Overflow);
                }
                if depth > 128 {
                    return mem::allocation_error(Some(&self.arena), BudgetError::Overflow);
                }
                self.arena.checkpoint()?;
                let syntax_at = native
                    .batch
                    .as_ref()
                    .err()
                    .and_then(|e| e.at.map(|at| at.offset));
                for value in &native.values {
                    let mut parts = mem::ScratchVec::new(Some(&self.arena), 0)?;
                    parts.extend(Some(&self.arena), value.path.split('.'))?;
                    let path = sources.schema.value_path_owned(Some(&self.arena), &parts)?;
                    if matches!(&*path, crate::ValuePath::Unknown)
                        && syntax_at.is_none_or(|offset| value.at < offset)
                    {
                        let Some(map) = map else {
                            return mem::allocation_error(Some(&self.arena), BudgetError::Overflow);
                        };
                        return Err(self.arena.quoted_error(
                            MsgId::SELECT_ERROR_WRONGPARAMETER,
                            map.at(text, value.at),
                            &value.path,
                        ));
                    }
                }
                let Ok(batch) = native.batch.as_ref() else {
                    return Ok(None);
                };
                if let Some(at) = query_syntax::first_unresolved_dcs_tail_at(batch) {
                    let Some(map) = map else {
                        return mem::allocation_error(Some(&self.arena), BudgetError::Overflow);
                    };
                    return Err(self.arena.quoted_error(
                        MsgId::SELECT_ERROR_SYNTAXERROR,
                        map.at(text, at),
                        crate::compile::UNRESOLVED_TAIL,
                    ));
                }
                let mut analyzer = crate::sema::Analyzer::new_scoped(
                    batch,
                    text,
                    sources.schema,
                    crate::sema::ManagedAnalyzeOptions {
                        temp_tables: sources.temps,
                        params: sources.params,
                        mode,
                        inter,
                    },
                    &self.arena,
                )?;
                analyzer.max_depth = depth;
                analyzer.stmt_owned(index).map(Some)
            })
        };
        let result = result.map_err(|error| self.arena.retain_scalar_error(error));
        if let Some(error) = self.arena.error() {
            return Err(self.failed(FailureBody::Resource(self.request.record(resource(error)))));
        }
        match result {
            Ok(Some(owner)) => Ok(WorkerStatement {
                owner,
                _parsed: parsed,
                _sources: sources,
            }),
            Ok(None) => Err(self.failed(FailureBody::Syntax(parsed))),
            Err(_placeholder) => {
                let Some(failure) = self.arena.take_diagnostic() else {
                    return Err(self.failed(FailureBody::Resource(
                        self.request.record(resource(BudgetError::Overflow)),
                    )));
                };
                Err(self.failed(FailureBody::Managed(failure)))
            }
        }
    }
}
fn resource(error: BudgetError) -> ResourceFailure {
    ResourceFailure {
        phase: ResourcePhase::Analyze,
        cause: ResourceCause::Memory(error),
    }
}
