use super::{Error, FEATURE_BYTES, Result};
use memory_budget::{Category, MemoryBudget, Shared};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub const MAX_WORK: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[repr(u8)]
pub enum Stop {
    #[error("extension scan cancelled")]
    Cancelled = 1,
    #[error("extension scan deadline exceeded")]
    Deadline = 2,
    #[error("extension scan physical-slot limit exceeded")]
    Slots = 3,
    #[error("extension scan read-work limit exceeded")]
    Reads = 4,
}
struct State {
    parent: Option<Arc<AtomicBool>>,
    stopped: AtomicU8,
    slots: AtomicU64,
    reads: AtomicU64,
    slot_limit: u64,
    read_limit: u64,
    deadline: Instant,
}
#[derive(Clone)]
pub struct Control {
    state: Shared<State>,
    budget: MemoryBudget,
}
impl Control {
    pub fn new(budget: &MemoryBudget) -> Result<Self> {
        Self::limited(
            budget,
            Instant::now() + Duration::from_secs(60),
            MAX_WORK,
            MAX_WORK,
        )
    }
    pub fn with_cancellation(budget: &MemoryBudget, parent: Arc<AtomicBool>) -> Result<Self> {
        Self::build(
            budget,
            Instant::now() + Duration::from_secs(60),
            MAX_WORK,
            MAX_WORK,
            Some(parent),
        )
    }
    pub fn limited(
        budget: &MemoryBudget,
        deadline: Instant,
        slots: u64,
        reads: u64,
    ) -> Result<Self> {
        Self::build(budget, deadline, slots, reads, None)
    }
    fn build(
        budget: &MemoryBudget,
        deadline: Instant,
        slots: u64,
        reads: u64,
        parent: Option<Arc<AtomicBool>>,
    ) -> Result<Self> {
        let budget = match budget.scope_snapshot() {
            Some(scope) if scope.limit <= FEATURE_BYTES => budget.clone(),
            Some(_) => return Err(Error::Unsupported("inventory scope exceeds feature policy")),
            None if budget.snapshot().admission <= FEATURE_BYTES => budget.clone(),
            None => budget.try_scope(Category::Metadata, FEATURE_BYTES)?,
        };
        let parent_bytes = if parent.is_some() {
            let header = std::alloc::Layout::new::<[usize; 2]>();
            let (layout, _) = header
                .extend(std::alloc::Layout::new::<AtomicBool>())
                .map_err(|_| memory_budget::BudgetError::Overflow)?;
            layout.pad_to_align().size() as u64
        } else {
            0
        };
        let charge = budget
            .try_reserve(Category::Metadata, parent_bytes)
            .map_err(memory_budget::BudgetError::from)?;
        let state = Shared::try_new(
            State {
                parent,
                stopped: AtomicU8::new(0),
                slots: AtomicU64::new(0),
                reads: AtomicU64::new(0),
                slot_limit: slots.min(MAX_WORK),
                read_limit: reads.min(MAX_WORK),
                deadline: deadline.min(Instant::now() + Duration::from_secs(60)),
            },
            charge,
        )?;
        Ok(Self { state, budget })
    }
    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn stop_reason(&self) -> Option<Stop> {
        match self.state.stopped.load(Ordering::Acquire) {
            1 => Some(Stop::Cancelled),
            2 => Some(Stop::Deadline),
            3 => Some(Stop::Slots),
            4 => Some(Stop::Reads),
            _ => None,
        }
    }
    fn stop(&self, reason: Stop) -> Stop {
        let _ = self.state.stopped.compare_exchange(
            0,
            reason as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.stop_reason().unwrap_or(reason)
    }
    pub fn cancel(&self) {
        self.stop(Stop::Cancelled);
    }
    pub fn check(&self) -> std::result::Result<(), Stop> {
        if let Some(reason) = self.stop_reason() {
            return Err(reason);
        }
        if self
            .state
            .parent
            .as_ref()
            .is_some_and(|parent| parent.load(Ordering::Acquire))
        {
            return Err(self.stop(Stop::Cancelled));
        }
        if Instant::now() >= self.state.deadline {
            return Err(self.stop(Stop::Deadline));
        }
        Ok(())
    }
    fn charge(
        &self,
        counter: &AtomicU64,
        limit: u64,
        reason: Stop,
    ) -> std::result::Result<(), Stop> {
        self.check()?;
        let mut observed = counter.load(Ordering::Relaxed);
        loop {
            let next = observed
                .checked_add(1)
                .filter(|n| *n <= limit)
                .ok_or_else(|| self.stop(reason))?;
            match counter.compare_exchange_weak(
                observed,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(now) => observed = now,
            }
        }
        self.check()
    }
    pub fn slot(&self) -> std::result::Result<(), Stop> {
        self.charge(&self.state.slots, self.state.slot_limit, Stop::Slots)
    }
    pub(crate) fn read_chunk(&self) -> std::result::Result<(), Stop> {
        self.charge(&self.state.reads, self.state.read_limit, Stop::Reads)
    }
    pub fn work(&self) -> (u64, u64) {
        (
            self.state.slots.load(Ordering::Relaxed),
            self.state.reads.load(Ordering::Relaxed),
        )
    }
}

pub fn path_open_scratch(
    budget: &MemoryBudget,
    path: &std::path::Path,
) -> Result<memory_budget::Reservation> {
    let valid = match budget.scope_snapshot() {
        Some(s) => s.limit <= FEATURE_BYTES,
        None => budget.snapshot().admission <= FEATURE_BYTES,
    };
    if !valid {
        return Err(Error::Unsupported(
            "inventory path scope exceeds feature policy",
        ));
    }
    #[cfg(unix)]
    {
        let bytes = u64::try_from(path.as_os_str().as_encoded_bytes().len())
            .map_err(|_| memory_budget::BudgetError::Overflow)?
            .checked_add(1)
            .ok_or(memory_budget::BudgetError::Overflow)?;
        Ok(budget
            .try_reserve(Category::ReaderWorking, bytes)
            .map_err(memory_budget::BudgetError::from)?)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Error::Unsupported(
            "inventory path conversion is not qualified on this platform",
        ))
    }
}
