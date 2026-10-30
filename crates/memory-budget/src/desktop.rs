use crate::policy::{CONTROL_RESERVE, ConfigError, GIB, MIB};
use crate::{BudgetError, Category, Limits, MemoryBudget};

mod native;
#[cfg(any(target_os = "windows", test))]
mod windows_job;
pub use native::facts;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    Auto,
    MiB(u64),
    Unlimited,
    Bytes(u64),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub physical: Option<u64>,
    pub available: Option<u64>,
    pub external: Option<u64>,
    pub external_visibility: crate::discovery::Visibility,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub budget: u64,
    pub ceiling: Option<u64>,
    pub fallback: bool,
    pub cache: u64,
    pub response: u64,
    pub external_visibility: crate::discovery::Visibility,
    pub unlimited: bool,
}

impl Policy {
    pub fn create_budget(self) -> Result<MemoryBudget, BudgetError> {
        MemoryBudget::try_new(self.limits())
    }

    pub fn create_child(self, process: &MemoryBudget) -> Result<MemoryBudget, BudgetError> {
        process.try_child(self.limits())
    }

    pub fn limits(self) -> Limits {
        if self.unlimited {
            return Limits::new(u64::MAX)
                .with_category(Category::Cache, self.cache)
                .with_category(Category::Response, u64::MAX);
        }
        Limits::new(self.budget.saturating_sub(CONTROL_RESERVE))
            .with_category(Category::Cache, self.cache)
            .with_category(Category::Response, self.response)
            .with_category(Category::Execution, (self.budget / 2).min(2 * GIB))
            .with_category(Category::Result, (self.budget / 4).min(512 * MIB))
            .with_category(Category::Row, (self.budget / 20).min(32 * MIB))
    }
}

fn fraction(value: u64, n: u64, d: u64) -> u64 {
    ((u128::from(value) * u128::from(n)) / u128::from(d)) as u64
}

pub fn process_ceiling(facts: Facts) -> u64 {
    let capacity = match (facts.physical, facts.external) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    capacity.map_or(GIB, |bytes| fraction(bytes, 7, 10) / MIB * MIB)
}

pub fn resolve(selection: Selection, facts: Facts) -> Result<Policy, ConfigError> {
    let ceiling = process_ceiling(facts);
    let fallback = facts.physical.is_none() && facts.external.is_none();
    if selection == Selection::Unlimited {
        return Ok(Policy {
            budget: u64::MAX,
            ceiling: None,
            fallback,
            cache: 256 * MIB,
            response: u64::MAX,
            external_visibility: facts.external_visibility,
            unlimited: true,
        });
    }
    let (budget, fallback) = match selection {
        Selection::MiB(mib) => (
            mib.checked_mul(MIB)
                .ok_or_else(|| ConfigError("Memory size overflow".into()))?,
            fallback,
        ),
        Selection::Bytes(bytes) => (bytes, fallback),
        Selection::Unlimited => (u64::MAX, fallback),
        Selection::Auto => {
            let mut budget = facts.physical.map_or(GIB, |bytes| bytes / 4).min(4 * GIB);
            if let Some(available) = facts.available {
                budget = budget.min(available / 2);
            }
            budget = budget.min(ceiling);
            (budget / MIB * MIB, fallback)
        }
    };
    if budget < 256 * MIB {
        return Err(ConfigError(
            "Memory budget must be at least 256 MiB; not enough memory for automatic mode".into(),
        ));
    }
    if budget > ceiling {
        return Err(ConfigError(format!(
            "Memory budget exceeds the safe ceiling of {} MiB",
            ceiling / MIB
        )));
    }
    Ok(Policy {
        budget,
        ceiling: Some(ceiling),
        fallback,
        cache: (budget / 5).min(256 * MIB),
        response: (budget / 10).min(64 * MIB),
        external_visibility: facts.external_visibility,
        unlimited: false,
    })
}
