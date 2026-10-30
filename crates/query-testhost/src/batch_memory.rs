use std::sync::Arc;

use memory_budget::{Category, Evictor, MemoryBudget, MemoryLimit, RequestMemory, Reservation};
use query_exec::{BatchMem, CompileModel};

use crate::host::TestBase;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchemaBounds {
    pub max_ref_fanout: u32,
    pub max_predefined_bytes: u64,
    pub max_table_width: u32,
}

impl SchemaBounds {
    pub(crate) fn of(base: &TestBase) -> SchemaBounds {
        SchemaBounds {
            max_ref_fanout: max_ref_fanout(base),
            max_predefined_bytes: max_predefined_bytes(base),
            max_table_width: max_table_width(base),
        }
    }
}

fn max_ref_fanout(base: &TestBase) -> u32 {
    let max = base
        .schema
        .tables()
        .iter()
        .flat_map(|t| t.fields.iter())
        .map(|f| f.ty.refs.len())
        .max()
        .unwrap_or(0);
    u32::try_from(max).unwrap_or(u32::MAX)
}

fn max_predefined_bytes(base: &TestBase) -> u64 {
    let block = |n: u64| if n == 0 { 0 } else { n.div_ceil(16) * 16 + 16 };
    base.predefined_ids
        .values()
        .map(|items| {
            block(items.len() as u64 * std::mem::size_of::<([u8; 16], String)>() as u64)
                + items
                    .iter()
                    .map(|(_, n)| block(n.len() as u64))
                    .sum::<u64>()
        })
        .max()
        .unwrap_or(0)
}

const C_PARAM_BYTES: u64 = 8;
const C_PARAM: u64 = 2048;

pub struct BatchMemory {
    pub memory: RequestMemory,
    pub compile: Reservation,
    pub params: Reservation,
    pub outer: Reservation,
    pub exec: Arc<Reservation>,
    pub evictor: Arc<onecd::Database>,
    pub max_ref_fanout: u32,
    pub max_predefined_bytes: u64,
    pub max_table_width: u32,
    pub model: CompileModel,
}

fn max_table_width(base: &TestBase) -> u32 {
    let max = base
        .schema
        .tables()
        .iter()
        .map(|t| t.fields.len())
        .max()
        .unwrap_or(0);
    u32::try_from(max).unwrap_or(u32::MAX)
}

impl BatchMemory {
    pub fn new(base: &TestBase) -> Option<Result<BatchMemory, MemoryLimit>> {
        let budget = base.memory_budget()?;
        Some(Self::over(base, budget, true))
    }

    pub fn with_budget(base: &TestBase, budget: &MemoryBudget) -> Result<BatchMemory, MemoryLimit> {
        Self::over(base, budget, false)
    }

    fn over(
        base: &TestBase,
        budget: &MemoryBudget,
        evicting: bool,
    ) -> Result<BatchMemory, MemoryLimit> {
        let bounds = base.schema_bounds();
        let evictor = base.evictor();
        let reserve = || {
            if evicting {
                budget.try_reserve(Category::Execution, 0)
            } else {
                budget.try_reserve_or_evict(Category::Execution, 0, evictor.as_ref())
            }
        };
        let compile = reserve()?;
        let exec = reserve()?;
        let params = reserve()?;
        let outer = reserve()?;
        let row_limit = budget
            .snapshot()
            .categories
            .get(Category::Row as usize)
            .map_or(u64::MAX, |c| c.limit);
        Ok(BatchMemory {
            memory: RequestMemory {
                budget: budget.clone(),
                row_limit,
            },
            compile,
            params,
            outer,
            exec: Arc::new(exec),
            evictor,
            max_ref_fanout: bounds.max_ref_fanout,
            max_predefined_bytes: bounds.max_predefined_bytes,
            max_table_width: bounds.max_table_width,
            model: CompileModel::DEFAULT,
        })
    }

    pub fn params_stage(&self, bytes: u64, n: usize) -> Result<(), MemoryLimit> {
        self.params.try_grow_or_evict(
            C_PARAM_BYTES
                .saturating_mul(bytes)
                .saturating_add(C_PARAM.saturating_mul(n as u64)),
            self.evictor.as_ref(),
        )
    }

    pub fn mem(&self) -> BatchMem<'_> {
        BatchMem {
            compile: &self.compile,
            evictor: self.evictor.as_ref(),
            max_ref_fanout: self.max_ref_fanout,
            max_predefined_bytes: self.max_predefined_bytes,
            max_table_width: self.max_table_width,
            model: self.model,
        }
    }

    pub fn evictor(&self) -> &dyn Evictor {
        self.evictor.as_ref()
    }

    pub fn held(&self) -> u64 {
        self.compile.bytes() + self.exec.bytes() + self.params.bytes() + self.outer.bytes()
    }
}
