use super::{EntryData, EntryMeta, decode_stored_budgeted};
use crate::{ManagedRootError, RootInfo};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget, Shared};
use std::cell::RefCell;

pub trait ManagedEntrySource {
    type Error;
    fn recoverable_content_error(&self, _error: &Self::Error) -> bool {
        false
    }
    fn list(&self) -> &[EntryMeta];
    fn memory_budget(&self) -> &MemoryBudget;
    fn stored(&self, name: &str) -> std::result::Result<Option<Charged<Vec<u8>>>, Self::Error>;
    fn stored_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> std::result::Result<Option<Charged<Vec<u8>>>, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedEntriesError<E> {
    #[error("entry source: {0}")]
    Source(#[source] E),
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Root(#[from] ManagedRootError),
    #[error("{0}")]
    Inconsistent(&'static str),
}
type Result<T, E> = std::result::Result<T, ManagedEntriesError<E>>;

pub struct ManagedEntries<'a, S: ManagedEntrySource + ?Sized> {
    source: &'a S,
    by_name: BudgetedVec<usize>,
    cache: RefCell<BudgetedVec<Option<Shared<EntryData>>>>,
    root: RefCell<Option<Shared<RootInfo>>>,
}
impl<'a, S: ManagedEntrySource + ?Sized> ManagedEntries<'a, S> {
    pub fn new(source: &'a S) -> Result<Self, S::Error> {
        let budget = source.memory_budget();
        let count = source.list().len();
        let mut by_name = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
        let mut cache = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
        for index in 0..count {
            by_name.try_push(index)?;
            cache.try_push(None)?;
        }
        by_name.sort_unstable_by(|a, b| {
            source
                .list()
                .get(*a)
                .map(|meta| meta.name.as_str())
                .cmp(&source.list().get(*b).map(|meta| meta.name.as_str()))
                .then_with(|| a.cmp(b))
        });
        Ok(Self {
            source,
            by_name,
            cache: RefCell::new(cache),
            root: RefCell::new(None),
        })
    }
    pub fn recoverable_content_error(&self, error: &ManagedEntriesError<S::Error>) -> bool {
        use crate::root::ErrorKind;
        use zraw::ManagedInflateError;
        match error {
            ManagedEntriesError::Source(error) => self.source.recoverable_content_error(error),
            ManagedEntriesError::Memory(_) | ManagedEntriesError::Inconsistent(_) => false,
            ManagedEntriesError::Root(error) => match error {
                ManagedRootError::Memory(_) | ManagedRootError::StoredTooLarge => false,
                ManagedRootError::Inflate(error) => match error {
                    ManagedInflateError::Corrupt
                    | ManagedInflateError::Truncated
                    | ManagedInflateError::Trailing => true,
                    ManagedInflateError::Memory(_)
                    | ManagedInflateError::TooLarge(_)
                    | ManagedInflateError::Zlib(_) => false,
                },
                ManagedRootError::Parse { kind, .. } => match kind {
                    ErrorKind::InputLimit | ErrorKind::DecodedLimit => false,
                    ErrorKind::Expected(_)
                    | ErrorKind::UnsupportedVersion(_)
                    | ErrorKind::InvalidBase64
                    | ErrorKind::TrailingContent => true,
                },
            },
        }
    }
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn begin_trace(&self) {
        trace::begin();
    }
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    pub fn end_trace(&self) -> Vec<trace::Event> {
        trace::end()
    }
    pub fn list(&self) -> &[EntryMeta] {
        self.source.list()
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        self.source.memory_budget()
    }
    fn find(&self, name: &str) -> Option<usize> {
        let at = self.by_name.partition_point(|&index| {
            self.list()
                .get(index)
                .is_some_and(|meta| meta.name.as_str() < name)
        });
        self.by_name
            .get(at)
            .copied()
            .filter(|&index| self.list().get(index).is_some_and(|meta| meta.name == name))
    }
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }
    fn validate(&self, stored: &Charged<Vec<u8>>) -> Result<(), S::Error> {
        if !self
            .source
            .memory_budget()
            .same_scope(stored.reservation().budget())
            || stored.reservation().category() != Category::Metadata
        {
            return Err(ManagedEntriesError::Inconsistent(
                "stored entry has unrelated memory ownership",
            ));
        }
        Ok(())
    }
    pub fn root_info(&self) -> Result<Shared<RootInfo>, S::Error> {
        if let Some(root) = self.root.borrow().as_ref() {
            return Ok(root.clone());
        }
        let meta = self
            .find("root")
            .and_then(|index| self.list().get(index))
            .ok_or(ManagedEntriesError::Inconsistent("missing root entry"))?;
        let stored = self
            .source
            .stored_bounded("root", crate::root::STORED_LIMIT)
            .map_err(ManagedEntriesError::Source)?
            .ok_or(ManagedEntriesError::Inconsistent("missing root entry"))?;
        self.validate(&stored)?;
        let root = RootInfo::read_budgeted(meta, stored)?.try_into_shared()?;
        *self.root.borrow_mut() = Some(root.clone());
        Ok(root)
    }
    pub fn get(&self, name: &str) -> Result<Option<Shared<EntryData>>, S::Error> {
        #[cfg(any(test, feature = "testing"))]
        trace::record("get", name);
        let Some(index) = self.find(name) else {
            return Ok(None);
        };
        if let Some(data) = self.cache.borrow().get(index).and_then(Option::as_ref) {
            return Ok(Some(data.clone()));
        }
        let Some(stored) = self
            .source
            .stored(name)
            .map_err(ManagedEntriesError::Source)?
        else {
            return Ok(None);
        };
        self.validate(&stored)?;
        let data = decode_stored_budgeted(
            &stored,
            self.source.memory_budget(),
            Category::Metadata,
            zraw::DEFAULT_LIMIT,
        )?
        .try_into_shared()?;
        let mut cache = self.cache.borrow_mut();
        let slot = cache
            .get_mut(index)
            .ok_or(ManagedEntriesError::Inconsistent(
                "invalid decoded entry cache slot",
            ))?;
        *slot = Some(data.clone());
        Ok(Some(data))
    }
    pub fn doc(&self, name: &str) -> Result<Option<Shared<EntryData>>, S::Error> {
        #[cfg(any(test, feature = "testing"))]
        trace::record("doc", name);
        Ok(self
            .get(name)?
            .filter(|data| matches!(**data, EntryData::Brace(_))))
    }
    pub fn forget(&self, name: &str) {
        #[cfg(any(test, feature = "testing"))]
        trace::record("forget", name);
        if let Some(index) = self.find(name)
            && let Some(slot) = self.cache.borrow_mut().get_mut(index)
        {
            *slot = None;
        }
    }
    pub fn raw(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>, S::Error> {
        #[cfg(any(test, feature = "testing"))]
        trace::record("raw", name);
        if !self.contains(name) {
            return Ok(None);
        }
        let Some(stored) = self
            .source
            .stored(name)
            .map_err(ManagedEntriesError::Source)?
        else {
            return Ok(None);
        };
        self.validate(&stored)?;
        match zraw::inflate_budgeted(
            &stored,
            zraw::DEFAULT_LIMIT,
            self.source.memory_budget(),
            Category::Metadata,
        ) {
            Ok(raw) => Ok(Some(raw)),
            Err(zraw::ManagedInflateError::Memory(error)) => Err(error.into()),
            Err(_) => Ok(Some(stored)),
        }
    }
}

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod trace {
    use memory_budget::{Lock, LockGuard};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Clone, Copy, Debug)]
    pub struct Event {
        pub kind: &'static str,
        text: [u8; 96],
        len: usize,
    }
    impl Event {
        const EMPTY: Self = Self {
            kind: "",
            text: [0; 96],
            len: 0,
        };
        pub fn name(&self) -> &str {
            std::str::from_utf8(self.text.get(..self.len).unwrap_or_default()).unwrap_or("")
        }
    }
    #[derive(Clone, Copy)]
    struct Slot {
        kind: Option<&'static str>,
        text: [u8; 96],
        len: usize,
    }
    impl Slot {
        const EMPTY: Self = Self {
            kind: None,
            text: [0; 96],
            len: 0,
        };
        fn event(&self) -> Event {
            Event {
                kind: self.kind.unwrap_or(Event::EMPTY.kind),
                text: self.text,
                len: self.len,
            }
        }
    }
    struct Trace {
        enabled: bool,
        used: usize,
        overflow: bool,
        events: [Slot; 8192],
    }
    const SLOTS: usize = 32;
    static CLAIMED: [AtomicBool; SLOTS] = [const { AtomicBool::new(false) }; SLOTS];
    static TRACES: [Lock<Trace>; SLOTS] = [const {
        Lock::new(Trace {
            enabled: false,
            used: 0,
            overflow: false,
            events: [Slot::EMPTY; 8192],
        })
    }; SLOTS];
    thread_local! {
        static SLOT: Cell<Option<usize>> = const { Cell::new(None) };
    }
    fn lock(slot: usize) -> Option<LockGuard<'static, Trace>> {
        TRACES.get(slot).map(Lock::lock)
    }
    pub fn record(kind: &'static str, name: &str) {
        let Some(slot) = SLOT.with(Cell::get) else {
            return;
        };
        let Some(mut trace) = lock(slot) else {
            return;
        };
        if !trace.enabled {
            return;
        }
        let at = trace.used;
        let Some(event) = trace.events.get_mut(at) else {
            trace.overflow = true;
            return;
        };
        let Some(bytes) = name.as_bytes().get(..name.len().min(96)) else {
            return;
        };
        let long = name.len() > 96;
        event.kind = Some(kind);
        event.len = bytes.len();
        if let Some(dest) = event.text.get_mut(..bytes.len()) {
            dest.copy_from_slice(bytes);
        }
        if long {
            trace.overflow = true;
        }
        trace.used += 1;
    }
    pub fn inline_bytes() -> usize {
        std::mem::size_of::<Trace>()
    }
    pub fn begin() {
        let slot = SLOT.with(Cell::get).or_else(|| {
            let free = CLAIMED.iter().position(|c| {
                c.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
            SLOT.with(|s| s.set(free));
            free
        });
        assert!(
            slot.is_some(),
            "test observer: more than {SLOTS} threads trace at once"
        );
        if let Some(mut trace) = slot.and_then(lock) {
            trace.used = 0;
            trace.overflow = false;
            trace.enabled = true;
        }
    }
    pub fn end() -> Vec<Event> {
        let Some(slot) = SLOT.with(Cell::take) else {
            return Vec::new();
        };
        let events = lock(slot).map(|mut trace| {
            trace.enabled = false;
            let overflow = trace.overflow;
            let events: Vec<Event> = trace
                .events
                .get(..trace.used)
                .unwrap_or_default()
                .iter()
                .map(Slot::event)
                .collect();
            (overflow, events)
        });
        if let Some(claimed) = CLAIMED.get(slot) {
            claimed.store(false, Ordering::Release);
        }
        let (overflow, events) = events.unwrap_or_default();
        assert!(!overflow, "test observer capacity exceeded");
        events
    }
}
