use crate::lock::{Guard as MutexGuard, Lock as Mutex};
use std::fmt;
use std::sync::Weak;
mod bootstrap;
use bootstrap::LedgerRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Category {
    Metadata,
    ReaderWorking,
    Cache,
    Execution,
    Result,
    Row,
    Input,
    Response,
}

impl Category {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::ReaderWorking => "reader_working",
            Self::Cache => "cache",
            Self::Execution => "execution",
            Self::Result => "result",
            Self::Row => "row",
            Self::Input => "input",
            Self::Response => "response",
        }
    }

    pub const ALL: [Self; 8] = [
        Self::Metadata,
        Self::ReaderWorking,
        Self::Cache,
        Self::Execution,
        Self::Result,
        Self::Row,
        Self::Input,
        Self::Response,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub admission: u64,
    pub categories: [u64; 8],
}

impl Limits {
    pub fn new(admission: u64) -> Self {
        let mut categories = [admission; 8];
        categories[Category::Metadata as usize] = u64::MAX;
        categories[Category::ReaderWorking as usize] = u64::MAX;
        Self {
            admission,
            categories,
        }
    }

    pub fn with_category(mut self, category: Category, bytes: u64) -> Self {
        self.categories[category as usize] = bytes;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitReason {
    Global,
    Category,
    Overflow,
    InvalidSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimit {
    pub category: Category,
    pub requested: u64,
    pub reason: LimitReason,
}

impl fmt::Display for MemoryLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MEMORY_LIMIT: {} ({:?})",
            self.category.as_str(),
            self.reason
        )
    }
}
impl std::error::Error for MemoryLimit {}

impl MemoryLimit {
    pub const fn public_text(self) -> &'static str {
        if matches!(self.reason, LimitReason::Global) {
            return "MEMORY_LIMIT: global";
        }
        match self.category {
            Category::Metadata | Category::ReaderWorking => "MEMORY_LIMIT: global",
            Category::Cache => "MEMORY_LIMIT: cache",
            Category::Execution => "MEMORY_LIMIT: execution",
            Category::Result => "MEMORY_LIMIT: result",
            Category::Row => "MEMORY_LIMIT: row",
            Category::Input => "MEMORY_LIMIT: input",
            Category::Response => "MEMORY_LIMIT: response",
        }
    }
}

pub trait Evictor: Sync {
    fn evict_clean(&self);
}

#[derive(Clone, Debug)]
pub struct MemoryBudget {
    inner: LedgerRef,
    local: Option<crate::Shared<Local>>,
}

#[derive(Debug)]
struct Local {
    scope: Option<Mutex<ScopeSnapshot>>,
    evictor: Option<EvictorRef>,
}

#[derive(Clone, Debug)]
struct EvictorRef(Weak<dyn Evictor + Send>);
impl std::panic::RefUnwindSafe for EvictorRef {}
impl std::panic::UnwindSafe for EvictorRef {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeSnapshot {
    pub category: Category,
    pub current: u64,
    pub peak: u64,
    pub limit: u64,
}

#[derive(Debug)]
struct State {
    limits: Limits,
    current: u64,
    peak: u64,
    categories: [CategorySnapshot; 8],
    mutations: u64,
    reads: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerOperations {
    pub mutations: u64,
    pub reads: u64,
}

#[derive(Debug)]
pub struct Reservation {
    budget: MemoryBudget,
    record: Mutex<Record>,
}
#[derive(Debug)]
pub struct ScopeBorrow {
    budget: MemoryBudget,
    bytes: u64,
}
impl ScopeBorrow {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }
}
impl Drop for ScopeBorrow {
    fn drop(&mut self) {
        if let Some(scope) = self.budget.scope() {
            lock(scope).current -= self.bytes;
        }
    }
}

#[derive(Debug)]
struct Record {
    category: Category,
    bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub current: u64,
    pub peak: u64,
    pub admission: u64,
    pub over_budget: bool,
    pub categories: [CategorySnapshot; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategorySnapshot {
    pub current: u64,
    pub peak: u64,
    pub limit: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock()
}

impl State {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            current: 0,
            peak: 0,
            categories: std::array::from_fn(|i| CategorySnapshot {
                current: 0,
                peak: 0,
                limit: limits.categories[i],
            }),
            mutations: 0,
            reads: 0,
        }
    }
    fn over_budget(&self) -> bool {
        self.current > self.limits.admission || self.categories.iter().any(|c| c.current > c.limit)
    }
    fn add(&mut self, category: Category, bytes: u64) -> Result<(), MemoryLimit> {
        self.mutations = self.mutations.wrapping_add(1);
        let error = |reason| MemoryLimit {
            category,
            requested: bytes,
            reason,
        };
        let current = self
            .current
            .checked_add(bytes)
            .ok_or(error(LimitReason::Overflow))?;
        let category_current = self.categories[category as usize]
            .current
            .checked_add(bytes)
            .ok_or(error(LimitReason::Overflow))?;
        if bytes != 0 && (self.over_budget() || current > self.limits.admission) {
            return Err(error(LimitReason::Global));
        }
        let counter = &mut self.categories[category as usize];
        if bytes != 0 && category_current > counter.limit {
            return Err(error(LimitReason::Category));
        }
        self.current = current;
        self.peak = self.peak.max(current);
        counter.current = category_current;
        counter.peak = counter.peak.max(category_current);
        Ok(())
    }
    fn release(&mut self, category: Category, bytes: u64) {
        self.mutations = self.mutations.wrapping_add(1);
        self.current -= bytes;
        self.categories[category as usize].current -= bytes;
    }
}

impl MemoryBudget {
    pub fn same_instance(&self, other: &Self) -> bool {
        LedgerRef::ptr_eq(&self.inner, &other.inner)
    }
    pub fn new(limits: Limits) -> Self {
        Self {
            inner: LedgerRef::new(limits),
            local: None,
        }
    }
    pub fn try_new(limits: Limits) -> Result<Self, crate::BudgetError> {
        Ok(Self {
            inner: LedgerRef::try_new(limits)?,
            local: None,
        })
    }
    pub fn try_scope(&self, category: Category, limit: u64) -> Result<Self, crate::BudgetError> {
        if self.scope().is_some() {
            return Err(MemoryLimit {
                category,
                requested: 0,
                reason: LimitReason::InvalidSize,
            }
            .into());
        }
        let charge = self.try_reserve(Category::Execution, 0)?;
        let local = crate::Shared::try_new(
            Local {
                scope: Some(Mutex::new(ScopeSnapshot {
                    category,
                    current: 0,
                    peak: 0,
                    limit,
                })),
                evictor: self.local.as_ref().and_then(|l| l.evictor.clone()),
            },
            charge,
        )?;
        Ok(Self {
            inner: self.inner.clone(),
            local: Some(local),
        })
    }
    pub fn try_with_evictor(
        &self,
        evictor: Weak<dyn Evictor + Send>,
    ) -> Result<Self, crate::BudgetError> {
        if self.scope().is_some() {
            return Err(MemoryLimit {
                category: Category::Metadata,
                requested: 0,
                reason: LimitReason::InvalidSize,
            }
            .into());
        }
        let charge = self.try_reserve(Category::Metadata, 0)?;
        let local = crate::Shared::try_new(
            Local {
                scope: None,
                evictor: Some(EvictorRef(evictor)),
            },
            charge,
        )?;
        Ok(Self {
            inner: self.inner.clone(),
            local: Some(local),
        })
    }
    pub fn evicts(&self) -> bool {
        self.local.as_ref().is_some_and(|l| l.evictor.is_some())
    }
    fn evict_for(&self, error: &MemoryLimit) -> bool {
        if error.reason != LimitReason::Global || error.category == Category::Cache {
            return false;
        }
        let Some(evictor) = self
            .local
            .as_ref()
            .and_then(|l| l.evictor.as_ref())
            .and_then(|e| e.0.upgrade())
        else {
            return false;
        };
        evictor.evict_clean();
        true
    }
    fn scope(&self) -> Option<&Mutex<ScopeSnapshot>> {
        self.local.as_ref().and_then(|l| l.scope.as_ref())
    }
    fn scope_node(&self) -> Option<&crate::Shared<Local>> {
        self.local.as_ref().filter(|l| l.scope.is_some())
    }
    pub fn scope_snapshot(&self) -> Option<ScopeSnapshot> {
        self.scope().map(|scope| *lock(scope))
    }
    pub fn try_borrow_scope(&self, bytes: u64) -> Result<ScopeBorrow, MemoryLimit> {
        let scope = self.scope().ok_or(MemoryLimit {
            category: Category::Row,
            requested: bytes,
            reason: LimitReason::InvalidSize,
        })?;
        let mut counter = lock(scope);
        let error = |reason| MemoryLimit {
            category: counter.category,
            requested: bytes,
            reason,
        };
        let next = counter
            .current
            .checked_add(bytes)
            .ok_or_else(|| error(LimitReason::Overflow))?;
        if bytes != 0 && next > counter.limit {
            return Err(error(LimitReason::Category));
        }
        counter.current = next;
        counter.peak = counter.peak.max(next);
        Ok(ScopeBorrow {
            budget: self.clone(),
            bytes,
        })
    }
    pub fn same_scope(&self, other: &Self) -> bool {
        self.same_instance(other)
            && match (self.scope_node(), other.scope_node()) {
                (None, None) => true,
                (Some(first), Some(second)) => crate::Shared::ptr_eq(first, second),
                _ => false,
            }
    }
    fn account_add(&self, category: Category, bytes: u64) -> Result<(), MemoryLimit> {
        if bytes == 0 {
            return Ok(());
        }
        let mut local = self.scope().map(|scope| lock(scope));
        let next = if let Some(scope) = local.as_ref() {
            let error = |reason| MemoryLimit {
                category: scope.category,
                requested: bytes,
                reason,
            };
            let next = scope
                .current
                .checked_add(bytes)
                .ok_or_else(|| error(LimitReason::Overflow))?;
            if bytes != 0 && next > scope.limit {
                return Err(error(LimitReason::Category));
            }
            Some(next)
        } else {
            None
        };
        {
            let mut state = lock(&self.inner);
            let added = state.add(category, bytes);
            self.publish_over(&state);
            added?;
        }
        if let (Some(scope), Some(next)) = (local.as_mut(), next) {
            scope.current = next;
            scope.peak = scope.peak.max(next);
        }
        Ok(())
    }
    fn account_release(&self, category: Category, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut local = self.scope().map(|scope| lock(scope));
        {
            let mut state = lock(&self.inner);
            state.release(category, bytes);
            self.publish_over(&state);
        }
        if let Some(scope) = local.as_mut() {
            scope.current -= bytes;
        }
    }
    pub fn try_reserve(&self, category: Category, bytes: u64) -> Result<Reservation, MemoryLimit> {
        match self.account_add(category, bytes) {
            Err(error) if self.evict_for(&error) => self.account_add(category, bytes)?,
            other => other?,
        }
        Ok(Reservation {
            budget: self.clone(),
            record: Mutex::new(Record { category, bytes }),
        })
    }
    pub fn try_reserve_or_evict(
        &self,
        category: Category,
        bytes: u64,
        evictor: &dyn Evictor,
    ) -> Result<Reservation, MemoryLimit> {
        match self.try_reserve(category, bytes) {
            Err(error) if error.reason == LimitReason::Global => {
                evictor.evict_clean();
                self.try_reserve(category, bytes)
            }
            other => other,
        }
    }
    pub fn check_admission(&self, category: Category) -> Result<(), MemoryLimit> {
        if self.over_budget() {
            return Err(MemoryLimit {
                category,
                requested: 0,
                reason: LimitReason::Global,
            });
        }
        Ok(())
    }
    pub fn set_limits(&self, limits: Limits) {
        let mut state = lock(&self.inner);
        state.mutations = state.mutations.wrapping_add(1);
        state.limits = limits;
        for (counter, limit) in state.categories.iter_mut().zip(limits.categories) {
            counter.limit = limit;
        }
        self.publish_over(&state);
    }
    pub fn over_budget(&self) -> bool {
        self.inner.over().load(std::sync::atomic::Ordering::Acquire)
    }
    fn publish_over(&self, state: &State) {
        use std::sync::atomic::Ordering;
        let over = state.over_budget();
        let flag = self.inner.over();
        if flag.load(Ordering::Relaxed) != over {
            flag.store(over, Ordering::Release);
        }
    }
    pub fn ledger_operations(&self) -> LedgerOperations {
        let state = lock(&self.inner);
        LedgerOperations {
            mutations: state.mutations,
            reads: state.reads,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut state = lock(&self.inner);
        state.reads = state.reads.wrapping_add(1);
        Snapshot {
            current: state.current,
            peak: state.peak,
            admission: state.limits.admission,
            over_budget: state.over_budget(),
            categories: state.categories,
        }
    }
}

impl Reservation {
    pub fn bytes(&self) -> u64 {
        lock(&self.record).bytes
    }
    pub fn category(&self) -> Category {
        lock(&self.record).category
    }
    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn try_into_shared(self) -> Result<crate::Shared<()>, crate::BudgetError> {
        crate::Shared::try_new((), self)
    }
    pub fn try_grow(&self, additional: u64) -> Result<(), MemoryLimit> {
        match self.grow_once(additional) {
            Err(error) if self.budget.evict_for(&error) => self.grow_once(additional),
            other => other,
        }
    }
    fn grow_once(&self, additional: u64) -> Result<(), MemoryLimit> {
        let mut record = lock(&self.record);
        let bytes = record.bytes.checked_add(additional).ok_or(MemoryLimit {
            category: record.category,
            requested: additional,
            reason: LimitReason::Overflow,
        })?;
        self.budget.account_add(record.category, additional)?;
        record.bytes = bytes;
        Ok(())
    }
    pub fn try_grow_or_evict(
        &self,
        additional: u64,
        evictor: &dyn Evictor,
    ) -> Result<(), MemoryLimit> {
        match self.try_grow(additional) {
            Err(error) if error.reason == LimitReason::Global => {
                evictor.evict_clean();
                self.try_grow(additional)
            }
            other => other,
        }
    }
    pub fn split(&mut self, bytes: u64) -> Result<Self, MemoryLimit> {
        let mut record = lock(&self.record);
        if bytes > record.bytes {
            return Err(MemoryLimit {
                category: record.category,
                requested: bytes,
                reason: LimitReason::InvalidSize,
            });
        }
        record.bytes -= bytes;
        Ok(Self {
            budget: self.budget.clone(),
            record: Mutex::new(Record {
                category: record.category,
                bytes,
            }),
        })
    }
    pub(crate) fn same_owner_category(&self, other: &Self) -> bool {
        self.budget.same_scope(&other.budget) && self.category() == other.category()
    }
    pub(crate) fn merge_same(&mut self, other: Self) {
        let mut destination = lock(&self.record);
        let mut source = lock(&other.record);
        if !self.budget.same_scope(&other.budget) || destination.category != source.category {
            std::process::abort();
        }
        let Some(bytes) = destination.bytes.checked_add(source.bytes) else {
            std::process::abort();
        };
        destination.bytes = bytes;
        source.bytes = 0;
    }
    pub fn transfer(&self, category: Category) -> Result<(), MemoryLimit> {
        let mut record = lock(&self.record);
        if category == record.category {
            return Ok(());
        }
        let mut state = lock(&self.budget.inner);
        state.mutations = state.mutations.wrapping_add(1);
        let target = &state.categories[category as usize];
        let current = target
            .current
            .checked_add(record.bytes)
            .ok_or(MemoryLimit {
                category,
                requested: record.bytes,
                reason: LimitReason::Overflow,
            })?;
        if current > target.limit {
            return Err(MemoryLimit {
                category,
                requested: record.bytes,
                reason: LimitReason::Category,
            });
        }
        state.categories[record.category as usize].current -= record.bytes;
        let target = &mut state.categories[category as usize];
        target.current = current;
        target.peak = target.peak.max(current);
        record.category = category;
        self.budget.publish_over(&state);
        Ok(())
    }
    pub fn shrink(&self, bytes: u64) -> Result<(), MemoryLimit> {
        let mut record = lock(&self.record);
        if bytes > record.bytes {
            return Err(MemoryLimit {
                category: record.category,
                requested: bytes,
                reason: LimitReason::InvalidSize,
            });
        }
        self.budget.account_release(record.category, bytes);
        record.bytes -= bytes;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let record = lock(&self.record);
        self.budget.account_release(record.category, record.bytes);
    }
}
