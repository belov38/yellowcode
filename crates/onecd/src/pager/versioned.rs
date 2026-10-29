use crate::catalog::Metadata;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock};

use super::{PageBuf, PageKind, PageRead};
use crate::catalog::Catalog;
use crate::db::{OpenOptions, SyncMode};
use crate::error::{Error, Location, Result};
use crate::format::header::{DbHeader, parse_header};

const HEADER_PROBE_LEN: usize = 0x18;

pub(crate) const POISONED: &str = "poisoned by failed commit; reopen the database";

pub(crate) struct DirtyPage {
    pub no: u32,
    pub kind: PageKind,
    pub data: PageBuf,
    pub base: Option<PageBuf>,
}

struct Outgoing {
    no: u32,
    kind: PageKind,
    data: PageBuf,
    base: Option<PageBuf>,
}

struct PoisonGuard<'a>(Option<&'a AtomicBool>);

impl PoisonGuard<'_> {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for PoisonGuard<'_> {
    fn drop(&mut self) {
        if let Some(flag) = self.0 {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

struct Entry {
    versions: Vec<(u64, PageBuf)>,
    unflushed: bool,
    used: u64,
}

impl Entry {
    fn is_clean(&self) -> bool {
        self.versions.len() == 1 && !self.unflushed
    }
}

#[derive(Default)]
struct Counts {
    clean: usize,
    retained: usize,
    multi: HashSet<u32>,
}

impl Counts {
    fn forget(&mut self, no: u32, e: &Entry) {
        if e.is_clean() {
            self.clean = self.clean.saturating_sub(1);
        }
        self.retained = self
            .retained
            .saturating_sub(e.versions.len().saturating_sub(1));
        if e.versions.len() > 1 {
            self.multi.remove(&no);
        }
    }

    fn count(&mut self, no: u32, e: &Entry) {
        if e.is_clean() {
            self.clean = self.clean.saturating_add(1);
        }
        self.retained = self
            .retained
            .saturating_add(e.versions.len().saturating_sub(1));
        if e.versions.len() > 1 {
            self.multi.insert(no);
        }
    }
}

struct Meta {
    num_pages: u32,
    header: DbHeader,
    catalog: Metadata<Catalog>,
    free_pages: u32,
}

struct State {
    epoch: u64,
    num_pages: u32,
    trailing_pages: u64,
    header: DbHeader,
    catalog: Metadata<Catalog>,
    free_pages: u32,
    readers: BTreeMap<u64, usize>,
    too_old_below: u64,
    cache: HashMap<u32, Entry>,
    counts: Counts,
    tick: u64,
    #[cfg(test)]
    collects: u64,
}

impl State {
    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    fn has_readers(&self) -> bool {
        self.readers.range(self.too_old_below..).next().is_some()
    }

    fn cached(&mut self, no: u32, epoch: u64) -> Result<Option<PageBuf>> {
        if epoch < self.too_old_below {
            return Err(Error::SnapshotTooOld);
        }
        let tick = self.next_tick();
        let Some(e) = self.cache.get_mut(&no) else {
            return Ok(None);
        };
        e.used = tick;
        match e.versions.iter().rev().find(|(from, _)| *from <= epoch) {
            Some((_, page)) => Ok(Some(page.clone())),
            None => Err(Error::corrupt(
                Location::Page(no),
                "no version of the page for this snapshot",
            )),
        }
    }

    fn insert_loaded(&mut self, no: u32, page: &PageBuf, cap: usize) {
        if cap == 0 || self.cache.contains_key(&no) {
            return;
        }
        self.make_room(1, cap);
        let e = Entry {
            versions: vec![(0, page.clone())],
            unflushed: false,
            used: self.next_tick(),
        };
        self.counts.count(no, &e);
        self.cache.insert(no, e);
    }

    fn make_room(&mut self, incoming: usize, cap: usize) {
        let want = self.counts.clean.saturating_add(incoming);
        if want <= cap {
            return;
        }
        let mut by_age: Vec<(u64, u32)> = self
            .cache
            .iter()
            .filter(|(_, e)| e.is_clean())
            .map(|(&no, e)| (e.used, no))
            .collect();
        let n = (want - cap).max(cap / 8).min(by_age.len());
        if n == 0 {
            return;
        }
        by_age.select_nth_unstable(n - 1);
        for &(_, no) in by_age.iter().take(n) {
            if let Some(e) = self.cache.remove(&no) {
                self.counts.forget(no, &e);
            }
        }
    }

    fn collect(&mut self) {
        #[cfg(test)]
        {
            self.collects += 1;
        }
        let live: Vec<u64> = (self.readers.range(self.too_old_below..))
            .map(|(&e, _)| e)
            .collect();
        let pages: Vec<u32> = self.counts.multi.iter().copied().collect();
        for no in pages {
            let Some(e) = self.cache.get_mut(&no) else {
                continue;
            };
            let n = e.versions.len();
            let mut keep = vec![false; n];
            if let Some(newest) = keep.last_mut() {
                *newest = true;
            }
            let mut i = 0;
            for &epoch in &live {
                while e
                    .versions
                    .get(i + 1)
                    .is_some_and(|(from, _)| *from <= epoch)
                {
                    i += 1;
                }
                if let (Some((from, _)), Some(k)) = (e.versions.get(i), keep.get_mut(i))
                    && *from <= epoch
                {
                    *k = true;
                }
            }
            if keep.iter().all(|&k| k) {
                continue;
            }
            self.counts.forget(no, e);
            let mut kept = keep.into_iter();
            e.versions.retain(|_| kept.next().unwrap_or(true));
            self.counts.count(no, e);
        }
    }

    fn enforce_retained(&mut self, page_size: usize, max_bytes: usize) {
        while self.counts.retained.saturating_mul(page_size) > max_bytes {
            let Some((&oldest, _)) = self.readers.range(self.too_old_below..).next() else {
                break;
            };
            if oldest >= self.epoch {
                break;
            }
            self.too_old_below = oldest.saturating_add(1);
            self.collect();
        }
    }

    fn publish(&mut self, epoch: u64, out: &[Outgoing], meta: Meta) {
        let keep_base = self.has_readers();
        for p in out {
            let tick = self.next_tick();
            match self.cache.get_mut(&p.no) {
                Some(e) => {
                    self.counts.forget(p.no, e);
                    e.versions.push((epoch, p.data.clone()));
                    e.unflushed = true;
                    e.used = tick;
                    self.counts.count(p.no, e);
                }
                None => {
                    let mut versions = Vec::with_capacity(2);
                    if keep_base && let Some(b) = &p.base {
                        versions.push((0, b.clone()));
                    }
                    versions.push((epoch, p.data.clone()));
                    let e = Entry {
                        versions,
                        unflushed: true,
                        used: tick,
                    };
                    self.counts.count(p.no, &e);
                    self.cache.insert(p.no, e);
                }
            }
        }
        if meta.num_pages > self.num_pages {
            self.trailing_pages = 0;
        }
        self.epoch = epoch;
        self.num_pages = meta.num_pages;
        self.header = meta.header;
        self.catalog = meta.catalog;
        self.free_pages = meta.free_pages;
        self.collect();
    }

    fn mark_flushed(&mut self, out: &[Outgoing]) {
        for p in out {
            if let Some(e) = self.cache.get_mut(&p.no) {
                self.counts.forget(p.no, e);
                e.unflushed = false;
                self.counts.count(p.no, e);
            }
        }
    }
}

#[derive(Default)]
struct CleanState {
    cache: Option<super::budgeted::CleanCache>,
    hits: u64,
    misses: u64,
    evictions: u64,
}
impl CleanState {
    fn clear(&mut self) {
        if let Some(mut cache) = self.cache.take() {
            let stats = cache.stats();
            self.evictions = self
                .evictions
                .saturating_add(stats.evictions)
                .saturating_add(stats.cache_pages);
        }
    }
    fn stats(&mut self) -> super::ReaderStats {
        let mut stats = self
            .cache
            .as_mut()
            .map_or_else(super::ReaderStats::default, |cache| cache.stats());
        stats.hits = self.hits;
        stats.misses = self.misses;
        stats.evictions = stats.evictions.saturating_add(self.evictions);
        stats
    }
}

struct CleanCell(Mutex<CleanState>, super::files::FileCache);
impl std::ops::Deref for CleanCell {
    type Target = Mutex<CleanState>;
    fn deref(&self) -> &Mutex<CleanState> {
        &self.0
    }
}
impl memory_budget::Evictor for CleanCell {
    fn evict_clean(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.1.clear();
    }
}
fn arc_bytes<T>() -> u64 {
    std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<T>())
        .ok()
        .and_then(|(layout, _)| u64::try_from(layout.pad_to_align().size()).ok())
        .unwrap_or(u64::MAX)
}

pub(crate) struct Pager {
    storage: super::storage::StorageOwner,
    page_size: u32,
    cache_pages: usize,
    max_retained_bytes: usize,
    sync: SyncMode,
    state: Mutex<State>,
    io_gate: RwLock<()>,
    commit_lock: Mutex<()>,
    poisoned: AtomicBool,
    budget: Option<memory_budget::MemoryBudget>,
    clean_cache: Arc<CleanCell>,
    _clean_charge: Option<memory_budget::Reservation>,
}

impl fmt::Debug for Pager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pager")
            .field("page_size", &self.page_size)
            .field("poisoned", &self.poisoned())
            .finish_non_exhaustive()
    }
}

pub(crate) struct Snapshot {
    pub epoch: u64,
    pub num_pages: u32,
    pub trailing_pages: u64,
    pub header: DbHeader,
    pub catalog: Metadata<Catalog>,
    pub free_pages: u32,
    pager: Metadata<Pager>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("epoch", &self.epoch)
            .field("num_pages", &self.num_pages)
            .finish_non_exhaustive()
    }
}

fn file_corrupt(what: String) -> Error {
    Error::corrupt(Location::File, what)
}

impl Pager {
    pub(crate) fn file_close_observer(&self) -> Option<super::FileCloseObserver> {
        self.storage.file_close_observer()
    }

    pub fn open(
        storage: impl Into<super::storage::StorageOwner>,
        opts: &OpenOptions,
    ) -> Result<(Metadata<Pager>, DbHeader)> {
        let storage = storage.into();
        if opts.memory_budget.is_some() && opts.mode != crate::db::Mode::ReadOnly {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed budget requires read-only mode",
            )));
        }
        let budget_structure = opts
            .memory_budget
            .as_ref()
            .map(|budget| {
                budget.try_reserve(
                    memory_budget::Category::Metadata,
                    std::mem::size_of::<Pager>() as u64,
                )
            })
            .transpose()
            .map_err(memory_budget::BudgetError::from)?;
        let len = storage.len()?;
        if len < HEADER_PROBE_LEN as u64 {
            return Err(file_corrupt(format!(
                "file is {len} bytes, shorter than the file header"
            )));
        }
        let mut head = [0u8; HEADER_PROBE_LEN];
        storage.read_at(0, &mut head)?;
        let header = parse_header(&head)?;

        let ps = u64::from(header.page_size);
        if len % ps != 0 {
            return Err(file_corrupt(format!(
                "file length {len} is not a multiple of the page size {ps}"
            )));
        }
        let pages = len / ps;
        let Some(trailing_pages) = pages.checked_sub(u64::from(header.length_pages)) else {
            return Err(file_corrupt(format!(
                "header says {} pages, file has {pages}",
                header.length_pages
            )));
        };
        let state = State {
            epoch: 0,
            num_pages: header.length_pages,
            trailing_pages,
            header: header.clone(),
            catalog: Metadata::try_new(Catalog::empty(), opts.memory_budget.as_ref())?,
            free_pages: 0,
            readers: BTreeMap::new(),
            too_old_below: 0,
            cache: HashMap::new(),
            counts: Counts::default(),
            tick: 0,
            #[cfg(test)]
            collects: 0,
        };
        let clean_charge = opts
            .memory_budget
            .as_ref()
            .map(|budget| {
                budget.try_reserve(memory_budget::Category::Metadata, arc_bytes::<CleanCell>())
            })
            .transpose()
            .map_err(memory_budget::BudgetError::from)?;
        let clean_cache = Arc::new(CleanCell(
            Mutex::new(CleanState::default()),
            super::files::FileCache::new(
                opts.memory_budget
                    .clone()
                    .filter(|_| opts.cache_bytes / header.page_size as usize != 0),
            ),
        ));
        let evictor: std::sync::Weak<dyn memory_budget::Evictor + Send> =
            Arc::<CleanCell>::downgrade(&clean_cache);
        let budget = opts
            .memory_budget
            .as_ref()
            .map(|budget| budget.try_with_evictor(evictor))
            .transpose()?;
        let pager = Pager {
            storage,
            page_size: header.page_size,
            cache_pages: opts.cache_bytes / header.page_size as usize,
            max_retained_bytes: opts.max_retained_bytes,
            sync: opts.sync,
            state: Mutex::new(state),
            io_gate: RwLock::new(()),
            commit_lock: Mutex::new(()),
            poisoned: AtomicBool::new(false),
            budget,
            clean_cache,
            _clean_charge: clean_charge,
        };
        let pager = match budget_structure {
            Some(lease) => {
                lease
                    .shrink(lease.bytes())
                    .map_err(memory_budget::BudgetError::from)?;
                Metadata::Managed(memory_budget::Shared::try_new(pager, lease)?)
            }
            None => Metadata::new(pager),
        };
        Ok((pager, header))
    }

    pub fn set_opened(&self, catalog: Metadata<Catalog>, free_pages: u32) {
        let mut st = self.state();
        st.catalog = catalog;
        st.free_pages = free_pages;
    }

    fn snapshot_handle(pager: &Metadata<Self>) -> Snapshot {
        let mut guard = pager.state();
        let st = &mut *guard;
        if pager.budget.is_none() {
            let n = st.readers.entry(st.epoch).or_insert(0);
            *n = n.saturating_add(1);
        }
        Snapshot {
            epoch: st.epoch,
            num_pages: st.num_pages,
            trailing_pages: st.trailing_pages,
            header: st.header.clone(),
            catalog: st.catalog.clone(),
            free_pages: st.free_pages,
            pager: pager.clone(),
        }
    }

    pub(crate) fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.budget.as_ref()
    }
    pub(crate) fn reader_stats(&self) -> Option<super::ReaderStats> {
        let budget = self.budget.as_ref()?;
        let mut stats = self
            .clean_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats();
        stats.reader_working_bytes = budget
            .snapshot()
            .categories
            .get(memory_budget::Category::ReaderWorking as usize)
            .map_or(0, |c| c.current);
        Some(stats)
    }
    pub(crate) fn evict_clean_cache(&self) {
        let mut cache = self
            .clean_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.clear();
        drop(cache);
        self.clean_cache.1.clear();
    }
    pub(crate) fn keep_opened_files(&self) {
        self.clean_cache.1.keep();
    }

    pub fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    pub fn commit(
        &self,
        pages: Vec<DirtyPage>,
        new_num_pages: u32,
        catalog: Metadata<Catalog>,
        header: DbHeader,
        free_pages: u32,
    ) -> Result<u64> {
        let _one = self
            .commit_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.poisoned() {
            return Err(Error::corrupt(Location::File, POISONED));
        }
        let (cur, old_num_pages, old_header) = {
            let st = self.state();
            (st.epoch, st.num_pages, st.header.clone())
        };
        let epoch = cur
            .checked_add(1)
            .ok_or_else(|| file_corrupt("epoch counter overflows".into()))?;
        let mut out = self.outgoing(pages, old_num_pages, new_num_pages, &header, &old_header)?;
        let meta = Meta {
            num_pages: new_num_pages,
            header,
            catalog,
            free_pages,
        };

        loop {
            let mut st = self.state();
            let need: Vec<usize> = if st.has_readers() {
                out.iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.no < old_num_pages && p.base.is_none() && !st.cache.contains_key(&p.no)
                    })
                    .map(|(i, _)| i)
                    .collect()
            } else {
                Vec::new()
            };
            if need.is_empty() {
                st.publish(epoch, &out, meta);
                st.enforce_retained(self.page_size as usize, self.max_retained_bytes);
                break;
            }
            drop(st);
            for i in need {
                if let Some(p) = out.get_mut(i) {
                    p.base = Some(self.read_at(cur, p.no)?);
                }
            }
        }

        let guard = PoisonGuard(Some(&self.poisoned));
        drop(self.io_gate.write().unwrap_or_else(PoisonError::into_inner));
        self.write_out(&out, old_num_pages, new_num_pages)?;
        let mut st = self.state();
        st.mark_flushed(&out);
        st.make_room(0, self.cache_pages);
        guard.disarm();
        Ok(epoch)
    }

    fn outgoing(
        &self,
        pages: Vec<DirtyPage>,
        old_num_pages: u32,
        new_num_pages: u32,
        header: &DbHeader,
        old_header: &DbHeader,
    ) -> Result<Vec<Outgoing>> {
        if new_num_pages < old_num_pages {
            return Err(file_corrupt(format!(
                "commit would shrink the file from {old_num_pages} to {new_num_pages} pages"
            )));
        }
        if header.length_pages != new_num_pages || header.page_size != self.page_size {
            return Err(file_corrupt(format!(
                "commit header ({} pages of {}) does not match the new file ({new_num_pages} pages of {})",
                header.length_pages, header.page_size, self.page_size
            )));
        }
        let ps = self.page_size as usize;
        let mut seen = HashSet::with_capacity(pages.len());
        let mut out = Vec::with_capacity(pages.len());
        for p in pages {
            let bad =
                |what: String| Error::corrupt(Location::Page(p.no), format!("commit: {what}"));
            if p.data.len() != ps {
                return Err(bad(format!("page of {} bytes", p.data.len())));
            }
            if p.no >= new_num_pages {
                return Err(bad(format!("beyond the new end ({new_num_pages} pages)")));
            }
            if !seen.insert(p.no) {
                return Err(bad("page given twice".into()));
            }
            if (p.no == 0) != (p.kind == PageKind::DbHeader) {
                return Err(bad(format!(
                    "page of kind {:?}; page 0 and only page 0 is the file header",
                    p.kind
                )));
            }
            if p.no == 0 {
                match parse_header(&p.data) {
                    Ok(h) if h == *header => {}
                    Ok(h) => {
                        return Err(bad(format!(
                            "the file header page holds {h:?}, the commit says {header:?}"
                        )));
                    }
                    Err(e) => return Err(bad(format!("the file header page: {e}"))),
                }
            }
            out.push(Outgoing {
                no: p.no,
                kind: p.kind,
                data: p.data,
                base: p.base.filter(|_| p.no < old_num_pages),
            });
        }
        if !seen.contains(&0) && (new_num_pages != old_num_pages || header != old_header) {
            return Err(Error::corrupt(
                Location::Page(0),
                "commit: the file header changes but page 0 is not written",
            ));
        }
        for no in old_num_pages..new_num_pages {
            if !seen.contains(&no) {
                out.push(Outgoing {
                    no,
                    kind: PageKind::Content,
                    data: PageBuf::from(vec![0u8; ps]),
                    base: None,
                });
            }
        }
        out.sort_by_key(|p| (p.kind, p.no == header.freelist_page, p.no));
        Ok(out)
    }

    fn write_out(
        &self,
        out: &[Outgoing],
        old_num_pages: u32,
        new_num_pages: u32,
    ) -> io::Result<()> {
        if new_num_pages > old_num_pages {
            self.storage.set_len(self.offset(new_num_pages))?;
        }
        for p in out {
            self.storage.write_at(self.offset(p.no), &p.data)?;
        }
        if self.sync == SyncMode::Full {
            self.storage.sync()?;
        }
        Ok(())
    }

    fn offset(&self, no: u32) -> u64 {
        u64::from(no) * u64::from(self.page_size)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read_at(&self, epoch: u64, no: u32) -> Result<PageBuf> {
        if let Some(budget) = &self.budget {
            let over = budget.over_budget();
            {
                let mut cache = self
                    .clean_cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if over {
                    cache.clear();
                }
                if let Some(page) = cache.cache.as_mut().and_then(|cache| cache.get(no)) {
                    cache.hits = cache.hits.saturating_add(1);
                    return Ok(page);
                }
                cache.misses = cache.misses.saturating_add(1);
            }
            let page = match self.load(no) {
                Err(Error::Memory(memory_budget::BudgetError::Limit(_))) => {
                    let mut cache = self
                        .clean_cache
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    cache.clear();
                    drop(cache);
                    self.load(no)?
                }
                result => result?,
            };
            let mut cache = self
                .clean_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if cache.cache.is_none() && self.cache_pages != 0 {
                if let Ok(created) = super::budgeted::CleanCache::new(
                    budget,
                    self.cache_pages.saturating_mul(self.page_size as usize),
                    self.page_size as usize,
                ) {
                    cache.cache = created;
                }
            }
            if let Some(cache) = cache.cache.as_mut() {
                cache.insert(no, &page)?;
            }
            return Ok(page);
        }
        let hit = self.state().cached(no, epoch)?;
        if let Some(page) = hit {
            return Ok(page);
        }
        let _gate = self.io_gate.read().unwrap_or_else(PoisonError::into_inner);
        let hit = self.state().cached(no, epoch)?;
        if let Some(page) = hit {
            return Ok(page);
        }
        let page = self.load(no)?;
        self.state().insert_loaded(no, &page, self.cache_pages);
        Ok(page)
    }

    fn load(&self, no: u32) -> Result<PageBuf> {
        if let Some(budget) = &self.budget {
            let mut page = PageBuf::try_zeroed(budget, self.page_size as usize)?;
            let Some(bytes) = page.unique_mut() else {
                return Err(Error::Io(io::ErrorKind::Other.into()));
            };
            match self.storage.read_at(self.offset(no), bytes) {
                Ok(()) => return Ok(page),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(Error::corrupt(
                        Location::Page(no),
                        "page is cut off by the end of the file",
                    ));
                }
                Err(error) => return Err(Error::Io(error)),
            }
        }
        let mut buf = vec![0u8; self.page_size as usize];
        match self.storage.read_at(self.offset(no), &mut buf) {
            Ok(()) => Ok(PageBuf::from(buf)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(Error::corrupt(
                Location::Page(no),
                "page is cut off by the end of the file",
            )),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn release(&self, epoch: u64) {
        let mut guard = self.state();
        let st = &mut *guard;
        let mut ended = false;
        if let Some(n) = st.readers.get_mut(&epoch) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                st.readers.remove(&epoch);
                ended = true;
            }
        }
        if ended && epoch >= st.too_old_below && epoch < st.epoch {
            st.collect();
            st.make_room(0, self.cache_pages);
        }
    }
}

impl Metadata<Pager> {
    pub fn snapshot(&self) -> Snapshot {
        Pager::snapshot_handle(self)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.pager.budget.is_none() {
            self.pager.release(self.epoch);
        }
    }
}

impl PageRead for Snapshot {
    fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.pager.budget.as_ref()
    }
    fn file_cache(&self) -> Option<(&super::files::FileCache, u64)> {
        self.pager.budget.as_ref()?;
        Some((&self.pager.clean_cache.1, self.epoch))
    }
    fn page_size(&self) -> u32 {
        self.pager.page_size
    }

    fn num_pages(&self) -> u32 {
        self.num_pages
    }

    fn read_page(&self, no: u32) -> Result<PageBuf> {
        if no >= self.num_pages {
            if self.pager.budget.is_some() {
                return Err(Error::CorruptStatic {
                    location: Location::Page(no),
                    what: "page is beyond the end of the database",
                });
            }
            return Err(Error::corrupt(
                Location::Page(no),
                format!(
                    "page is beyond the end of the file ({} pages)",
                    self.num_pages
                ),
            ));
        }
        self.pager.read_at(self.epoch, no)
    }
}
