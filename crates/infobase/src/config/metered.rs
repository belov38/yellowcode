use super::managed::ManagedConfigTxnSource;
use crate::{Error, Result};
use mdconf::{EntryMeta, EntrySource, Inflated, LoadMeter, MeterRefusal};
use memory_budget::{BudgetError, Category, Evictor, MemoryBudget, MemoryLimit, Reservation};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod model {
    pub const FIXED_OPEN: u64 = 32 << 20;
    pub const PER_ENTRY: u64 = 512;
    pub const CONFIG_FIXED: u64 = 1 << 20;
    pub const PER_INFLATED_SIXTEENTHS: u64 = 10;
    pub const PER_DECODED_ENTRY: u64 = 12_160;
    pub const PER_DBNAMES_BYTE: u64 = 2;
    pub const PER_RULES_BYTE: u64 = 16;
    pub const PER_MAP_BYTE: u64 = 10;
    pub const PER_PHYS_BYTE: u64 = 1;
    pub const PER_STORE_TABLE: u64 = 8 << 10;

    pub fn decoded_entry(inflated: u64) -> u64 {
        (inflated / 16)
            .saturating_mul(PER_INFLATED_SIXTEENTHS)
            .saturating_add(PER_DECODED_ENTRY)
    }

    pub fn copy(bytes: u64) -> u64 {
        bytes.div_ceil(16).saturating_mul(16).saturating_add(16)
    }
}

pub struct OpenLease {
    lease: Reservation,
    evictor: Arc<dyn Evictor + Send>,
    retained: AtomicU64,
    entries: AtomicU64,
    inflated: AtomicU64,
    copied: AtomicU64,
    holding: AtomicU64,
    held_max: AtomicU64,
}

impl std::fmt::Debug for OpenLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenLease")
            .field("bytes", &self.lease.bytes())
            .field("retained", &self.retained())
            .finish()
    }
}

impl OpenLease {
    pub fn reserve_fixed(budget: &MemoryBudget) -> Result<Reservation> {
        budget
            .try_reserve(Category::Metadata, model::FIXED_OPEN)
            .map_err(limit)
    }

    pub fn new(fixed: Reservation, evictor: Arc<dyn Evictor + Send>) -> OpenLease {
        let retained = fixed.bytes();
        OpenLease {
            lease: fixed,
            evictor,
            retained: AtomicU64::new(retained),
            entries: AtomicU64::new(0),
            inflated: AtomicU64::new(0),
            copied: AtomicU64::new(0),
            holding: AtomicU64::new(0),
            held_max: AtomicU64::new(0),
        }
    }

    pub fn grow(&self, bytes: u64, retained: bool) -> std::result::Result<(), MemoryLimit> {
        self.lease.try_grow_or_evict(bytes, &*self.evictor)?;
        crate::open::probe_input("grow", bytes);
        if retained {
            self.retained.fetch_add(bytes, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn shrink(&self, bytes: u64) -> std::result::Result<(), MemoryLimit> {
        crate::open::probe_input("shrink", bytes);
        self.lease.shrink(bytes)
    }

    pub fn bytes(&self) -> u64 {
        self.lease.bytes()
    }

    pub fn retained(&self) -> u64 {
        self.retained.load(Ordering::Relaxed)
    }

    pub fn admitted(&self) -> (u64, u64) {
        (
            self.entries.load(Ordering::Relaxed),
            self.inflated.load(Ordering::Relaxed),
        )
    }

    pub fn admitted_shapes(&self) -> (u64, u64) {
        (
            self.held_max.load(Ordering::Relaxed),
            self.copied.load(Ordering::Relaxed),
        )
    }

    pub fn into_reservation(self) -> Reservation {
        self.lease
    }
}

impl LoadMeter for OpenLease {
    fn admit(&self, _entry: &str, inflated: u64) -> std::result::Result<(), MeterRefusal> {
        self.grow(model::decoded_entry(inflated), false)
            .map_err(MeterRefusal)?;
        self.entries.fetch_add(1, Ordering::Relaxed);
        self.inflated.fetch_add(inflated, Ordering::Relaxed);
        Ok(())
    }

    fn hold(&self, _entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.grow(bytes, false).map_err(MeterRefusal)?;
        let now = self.holding.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.held_max.fetch_max(now, Ordering::Relaxed);
        Ok(())
    }

    fn release(&self, bytes: u64) {
        let _ = self.shrink(bytes);
        self.holding.fetch_sub(bytes, Ordering::Relaxed);
    }

    fn admit_copy(&self, _entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.grow(model::copy(bytes), false).map_err(MeterRefusal)?;
        self.copied.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }
}

fn limit(error: MemoryLimit) -> Error {
    Error::Memory(BudgetError::Limit(error))
}

fn to_mdconf(name: &str, error: Error) -> mdconf::Error {
    match error {
        Error::Memory(BudgetError::Limit(m))
        | Error::Inflate(zraw::ManagedInflateError::Memory(BudgetError::Limit(m))) => {
            mdconf::Error::Memory(m)
        }
        Error::Engine(onecd::Error::Memory(BudgetError::Limit(m))) => mdconf::Error::Memory(m),
        other => mdconf::Error::Io(std::io::Error::other(format!("{name}: {other}"))),
    }
}

pub struct MeteredConfigSource<'a> {
    inner: ManagedConfigTxnSource<'a>,
    budget: MemoryBudget,
}

impl<'a> MeteredConfigSource<'a> {
    pub fn new(inner: ManagedConfigTxnSource<'a>, budget: MemoryBudget) -> Self {
        Self { inner, budget }
    }

    pub fn len(&self) -> usize {
        self.inner.list().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn stored_charged(
        &self,
        name: &str,
        limit: u64,
    ) -> mdconf::Result<Option<memory_budget::Charged<Vec<u8>>>> {
        let cap = usize::try_from(limit.min(zraw::DEFAULT_LIMIT)).unwrap_or(usize::MAX);
        self.inner
            .stored_bounded(name, cap)
            .map_err(|e| to_mdconf(name, e))
    }
}

impl EntrySource for MeteredConfigSource<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Ok(self.inner.list().to_vec())
    }

    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        Err(mdconf::Error::Format {
            entry: name.to_string(),
            what: "metered CONFIG source reads entries inflated only".into(),
        })
    }

    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        Ok(self
            .stored_charged(name, u64::try_from(limit).unwrap_or(u64::MAX))?
            .map(|bytes| bytes.to_vec()))
    }

    fn stored_inflated_bounded(&self, name: &str, limit: u64) -> mdconf::Result<Option<Inflated>> {
        let Some(stored) = self.stored_charged(name, limit)? else {
            return Ok(None);
        };
        let limit = limit.min(zraw::DEFAULT_LIMIT);
        match zraw::inflate_budgeted(&stored, limit, &self.budget, Category::Metadata) {
            Ok(bytes) => Ok(Some(Inflated {
                bytes,
                was_deflated: true,
            })),
            Err(zraw::ManagedInflateError::Memory(BudgetError::Limit(m))) => {
                Err(mdconf::Error::Memory(m))
            }
            Err(zraw::ManagedInflateError::Memory(e)) => Err(mdconf::Error::Io(
                std::io::Error::new(std::io::ErrorKind::OutOfMemory, format!("{name}: {e}")),
            )),
            Err(_) => Ok(Some(Inflated {
                bytes: stored,
                was_deflated: false,
            })),
        }
    }
}
