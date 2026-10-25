use crate::{BudgetError, Category, MemoryBudget, WorkerContext};
use std::cell::Cell;

#[derive(Clone, Debug)]
pub struct RequestMemory {
    pub budget: MemoryBudget,
    pub row_limit: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourcePhase {
    Parse,
    Analyze,
    Build,
    Bind,
    Lower,
    Execute,
    Input,
    Table,
    Retain,
    Serialize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureLimit {
    Depth,
    Statements,
    Columns,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceCause {
    Memory(BudgetError),
    Structure(StructureLimit),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceFailure {
    pub phase: ResourcePhase,
    pub cause: ResourceCause,
}

pub struct RequestBudget<'r, 'w> {
    memory: &'r MemoryBudget,
    _worker: &'r WorkerContext<'w>,
    first_resource: Cell<Option<ResourceFailure>>,
}

impl<'r, 'w> RequestBudget<'r, 'w> {
    pub fn new(memory: &'r MemoryBudget, worker: &'r WorkerContext<'w>) -> Self {
        Self {
            memory,
            _worker: worker,
            first_resource: Cell::new(None),
        }
    }

    pub fn memory(&self) -> &MemoryBudget {
        self.memory
    }

    pub fn failure(&self) -> Option<ResourceFailure> {
        self.first_resource.get()
    }

    pub fn record(&self, failure: ResourceFailure) -> ResourceFailure {
        match self.first_resource.get() {
            Some(first) => first,
            None => {
                self.first_resource.set(Some(failure));
                failure
            }
        }
    }

    pub fn checkpoint(&self, phase: ResourcePhase) -> Result<(), ResourceFailure> {
        if let Some(first) = self.failure() {
            return Err(first);
        }
        self.memory
            .check_admission(Category::Execution)
            .map_err(|error| {
                self.record(ResourceFailure {
                    phase,
                    cause: ResourceCause::Memory(error.into()),
                })
            })
    }
}
