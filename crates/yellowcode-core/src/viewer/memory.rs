mod catalogs;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mdconf::{LoadMeter, MeterRefusal};
use memory_budget::desktop::{Facts, Policy};
use memory_budget::{BudgetError, Category, Limits, MemoryBudget, Reservation};

#[derive(Debug)]
pub enum ProcessOpenError {
    Memory(BudgetError),
    Platform(platform_catalog::ManagedSelectError),
    Resources(super::resources::GuiResourceError),
}
impl std::fmt::Display for ProcessOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => std::fmt::Display::fmt(e, f),
            Self::Platform(e) => std::fmt::Display::fmt(e, f),
            Self::Resources(e) => std::fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for ProcessOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Memory(e) => e,
            Self::Platform(e) => e,
            Self::Resources(e) => e,
        })
    }
}
impl From<BudgetError> for ProcessOpenError {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<platform_catalog::ManagedSelectError> for ProcessOpenError {
    fn from(e: platform_catalog::ManagedSelectError) -> Self {
        Self::Platform(e)
    }
}

impl From<super::resources::GuiResourceError> for ProcessOpenError {
    fn from(e: super::resources::GuiResourceError) -> Self {
        Self::Resources(e)
    }
}

pub struct Process {
    budget: MemoryBudget,
    refresh: Mutex<()>,
    unlimited: bool,
}

impl Process {
    pub fn new(facts: Facts) -> Result<Self, BudgetError> {
        Ok(Self {
            budget: MemoryBudget::try_new(Limits::new(memory_budget::desktop::process_ceiling(
                facts,
            )))?,
            refresh: Mutex::new(()),
            unlimited: false,
        })
    }
    pub fn unlimited() -> Result<Self, BudgetError> {
        Ok(Self {
            budget: MemoryBudget::try_new(Limits::new(u64::MAX))?,
            refresh: Mutex::new(()),
            unlimited: true,
        })
    }
    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn refresh(&self, facts: Facts) {
        if self.unlimited {
            return;
        }
        let _guard = super::consoles::lock(&self.refresh);
        let cap = self
            .budget
            .snapshot()
            .admission
            .min(memory_budget::desktop::process_ceiling(facts));
        self.budget.set_limits(Limits::new(cap));
    }
    pub fn open(&self, policy: Policy) -> Result<Arc<Session>, ProcessOpenError> {
        platform_catalog::prepare_with_memory(&self.budget)?;
        catalogs::ensure(&self.budget)?;
        super::tree::prepare_order_with_memory(&self.budget)?;
        super::tree::prepare_top_with_memory(&self.budget)?;
        super::icons::prepare_with_memory(&self.budget)?;
        super::configurator::prepare_with_memory(&self.budget)?;
        super::names::prepare_with_memory(&self.budget)?;
        super::types::prepare_with_memory(&self.budget)?;
        let budget = policy.create_child(&self.budget)?;
        Ok(Session::new(budget, policy.cache, policy.response)?)
    }
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn diagnostic_prepare_fixed_catalogs(root: &MemoryBudget) -> Result<(), BudgetError> {
    catalogs::ensure(&root.root_budget())
}

pub struct Session {
    console: Arc<super::consoles::ConsoleSlot>,
    cancelled: Arc<AtomicBool>,
    extensions: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    budget: MemoryBudget,
    cache: usize,
    response: u64,
    _ownership: Reservation,
}
impl Session {
    fn new(budget: MemoryBudget, cache: u64, response: u64) -> Result<Arc<Self>, BudgetError> {
        let ownership = budget.try_reserve(Category::Metadata, 8192)?;
        let cache = usize::try_from(cache).map_err(|_| BudgetError::Overflow)?;
        Ok(Arc::new(Self {
            console: Arc::new(super::consoles::ConsoleSlot::with_memory(
                budget.clone(),
                cache,
            )),
            cancelled: Arc::new(AtomicBool::new(false)),
            extensions: Mutex::new(HashMap::new()),
            budget,
            cache,
            response,
            _ownership: ownership,
        }))
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.console.close();
        super::consoles::lock(&self.extensions).clear();
    }
    pub fn close(&self) {
        self.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err("opening cancelled".into())
        } else {
            Ok(())
        }
    }
    pub fn cancellation(&self) -> &AtomicBool {
        &self.cancelled
    }
    pub(crate) fn token(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn cache_bytes(&self) -> usize {
        self.cache
    }
    pub fn response_bytes(&self) -> u64 {
        self.response
    }
    pub fn console(&self) -> Arc<super::consoles::ConsoleSlot> {
        self.console.clone()
    }
    pub fn extension<T: Default + Any + Send + Sync>(&self) -> crate::Result<Arc<T>> {
        let mut owners = super::consoles::lock(&self.extensions);
        self.check()?;
        let owner = owners
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(T::default()))
            .clone();
        match owner.downcast::<T>() {
            Ok(owner) => Ok(owner),
            Err(_) => unreachable!("subsystem owner type mismatch"),
        }
    }
}

pub struct GuiLease {
    charge: Reservation,
    cancelled: Arc<AtomicBool>,
}
struct OpeningFailure {
    error: crate::Error,
    _lease: Arc<GuiLease>,
}
impl std::fmt::Debug for OpeningFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.error, f)
    }
}
impl std::fmt::Display for OpeningFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for OpeningFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}
impl GuiLease {
    pub(crate) fn retain_error(self: Arc<Self>, error: crate::Error) -> crate::Error {
        Box::new(OpeningFailure {
            error,
            _lease: self,
        })
    }

    pub fn new(session: &Session) -> Result<Arc<Self>, BudgetError> {
        Ok(Arc::new(Self {
            charge: session.budget().try_reserve(Category::Metadata, 8 << 20)?,
            cancelled: session.cancelled.clone(),
        }))
    }
    fn check(&self) -> Result<(), MeterRefusal> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(MeterRefusal(memory_budget::MemoryLimit {
                category: Category::Metadata,
                requested: 0,
                reason: memory_budget::LimitReason::InvalidSize,
            }))
        } else {
            Ok(())
        }
    }
    pub fn bytes(&self) -> u64 {
        self.charge.bytes()
    }
    pub fn tree(budget: &MemoryBudget, bytes: u64) -> Result<Arc<Self>, BudgetError> {
        Ok(Arc::new(Self {
            charge: budget.try_reserve(Category::Metadata, bytes)?,
            cancelled: Arc::new(AtomicBool::new(false)),
        }))
    }
    pub fn entries(&self, entries: u64, names: u64) -> Result<(), memory_budget::MemoryLimit> {
        self.charge.try_grow(
            entries
                .saturating_mul(4096)
                .saturating_add(names.saturating_mul(8)),
        )
    }
}
impl LoadMeter for GuiLease {
    fn schema_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn schema_budget(&self) -> Option<MemoryBudget> {
        Some(self.charge.budget().root_budget())
    }

    fn checkpoint(&self) -> mdconf::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(mdconf::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "opening cancelled",
            )))
        } else {
            Ok(())
        }
    }
    fn admit(&self, _: &str, inflated: u64) -> Result<(), MeterRefusal> {
        self.check()?;
        self.charge
            .try_grow((64_u64 << 10).saturating_add(inflated.saturating_mul(8)))
            .map_err(MeterRefusal)
    }
    fn hold(&self, _: &str, bytes: u64) -> Result<(), MeterRefusal> {
        self.check()?;
        self.charge.try_grow(bytes).map_err(MeterRefusal)
    }
    fn release(&self, bytes: u64) {
        let _ = self.charge.shrink(bytes);
    }
    fn admit_copy(&self, _: &str, bytes: u64) -> Result<(), MeterRefusal> {
        self.check()?;
        self.charge
            .try_grow(bytes.saturating_mul(2).saturating_add(64))
            .map_err(MeterRefusal)
    }
}
