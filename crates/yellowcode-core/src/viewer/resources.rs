use memory_budget::{BudgetError, Category, Lock, MemoryBudget, Reservation};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub enum GuiResourceError {
    Memory(BudgetError),
    Platform(platform_catalog::ManagedSelectError),
    Schema(mdconf::ManagedSchemaError),
}
impl std::fmt::Display for GuiResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => std::fmt::Display::fmt(e, f),
            Self::Platform(e) => std::fmt::Display::fmt(e, f),
            Self::Schema(e) => std::fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for GuiResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Memory(e) => e,
            Self::Platform(e) => e,
            Self::Schema(e) => e,
        })
    }
}
impl From<BudgetError> for GuiResourceError {
    fn from(value: BudgetError) -> Self {
        Self::Memory(value)
    }
}
impl From<platform_catalog::ManagedSelectError> for GuiResourceError {
    fn from(value: platform_catalog::ManagedSelectError) -> Self {
        Self::Platform(value)
    }
}

struct Payload<T> {
    value: T,
    owner: Lock<Option<Reservation>>,
}
pub(super) struct Graph<T> {
    value: OnceLock<Payload<T>>,
    claimed: AtomicBool,
}
struct Claim<'a>(&'a AtomicBool);
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl<T> Graph<T> {
    pub const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            claimed: AtomicBool::new(false),
        }
    }
    pub fn prepare(
        &self,
        budget: Option<&MemoryBudget>,
        count: impl FnOnce() -> Result<u64, BudgetError>,
        backing: impl FnOnce(&T) -> Result<u64, BudgetError>,
        build: impl FnOnce() -> Result<T, BudgetError>,
    ) -> Result<&T, BudgetError> {
        self.prepare_inner(budget, count, backing, build, |_| {})
    }
    fn prepare_inner(
        &self,
        budget: Option<&MemoryBudget>,
        count: impl FnOnce() -> Result<u64, BudgetError>,
        backing: impl FnOnce(&T) -> Result<u64, BudgetError>,
        build: impl FnOnce() -> Result<T, BudgetError>,
        before_publish: impl FnOnce(&T),
    ) -> Result<&T, BudgetError> {
        while self
            .claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
        let _claim = Claim(&self.claimed);
        if let Some(payload) = self.value.get() {
            if let Some(root) = budget {
                let unowned = payload.owner.lock().is_none();
                if unowned {
                    let owner = root.try_reserve(Category::Metadata, backing(&payload.value)?)?;
                    *payload.owner.lock() = Some(owner);
                }
            }
            return Ok(&payload.value);
        }
        let owner = budget
            .map(|root| -> Result<_, BudgetError> {
                Ok(root.try_reserve(Category::Metadata, count()?)?)
            })
            .transpose()?;
        let value = build()?;
        let candidate = Payload {
            value,
            owner: Lock::new(owner),
        };
        before_publish(&candidate.value);
        let published = self.value.get_or_init(|| candidate);
        Ok(&published.value)
    }
}

pub(super) fn vec<T>(len: usize) -> Result<Vec<T>, BudgetError> {
    let mut value = Vec::new();
    value
        .try_reserve_exact(len)
        .map_err(|_| BudgetError::AllocationFailed)?;
    Ok(value)
}
pub(super) fn text(value: &str) -> Result<String, BudgetError> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| BudgetError::AllocationFailed)?;
    result.push_str(value);
    Ok(result)
}
pub(super) fn add(total: &mut u64, bytes: usize) -> Result<(), BudgetError> {
    *total = total
        .checked_add(u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?)
        .ok_or(BudgetError::Overflow)?;
    Ok(())
}
pub(super) fn rows<T>(total: &mut u64, len: usize) -> Result<(), BudgetError> {
    add(
        total,
        len.checked_mul(std::mem::size_of::<T>())
            .ok_or(BudgetError::Overflow)?,
    )
}

impl From<mdconf::ManagedSchemaError> for GuiResourceError {
    fn from(value: mdconf::ManagedSchemaError) -> Self {
        Self::Schema(value)
    }
}
