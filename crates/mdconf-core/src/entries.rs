use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{Error, LoadMeter, MeterRefusal, Result};

mod cache_managed;
mod managed;
pub use cache_managed::{ManagedEntries, ManagedEntriesError, ManagedEntrySource};
pub use managed::decode_stored_budgeted;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMeta {
    pub name: String,
    pub ctime: v8cont::V8Time,
    pub mtime: v8cont::V8Time,
    pub attrs: u32,
}

#[derive(Debug, Clone)]
pub enum EntryBacking<T> {
    Native(T),
    Charged(memory_budget::Shared<T>),
}
impl<T> EntryBacking<T> {
    pub fn from_charged(
        value: memory_budget::Charged<T>,
    ) -> std::result::Result<Self, memory_budget::BudgetError> {
        Ok(Self::Charged(value.try_into_shared()?))
    }
    pub fn budget(&self) -> Option<&memory_budget::MemoryBudget> {
        match self {
            Self::Native(_) => None,
            Self::Charged(value) => Some(value.reservation().budget()),
        }
    }
}
impl<T> std::ops::Deref for EntryBacking<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Native(value) => value,
            Self::Charged(value) => value,
        }
    }
}
impl<T: PartialEq> PartialEq for EntryBacking<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl<T: Eq> Eq for EntryBacking<T> {}
impl<T: Default> Default for EntryBacking<T> {
    fn default() -> Self {
        Self::Native(T::default())
    }
}
pub type EntryList = EntryBacking<Vec<EntryMeta>>;
pub type StoredBytes = EntryBacking<Vec<u8>>;

pub trait EntrySource {
    fn decoded_cache_capacity(&self) -> Option<std::num::NonZeroUsize> {
        None
    }

    fn list(&self) -> Result<Vec<EntryMeta>>;
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn list_owned(&self) -> Result<EntryList> {
        self.list().map(EntryBacking::Native)
    }
    fn stored_owned(&self, name: &str) -> Result<Option<StoredBytes>> {
        self.stored(name)
            .map(|value| value.map(EntryBacking::Native))
    }
    fn stored_owned_bounded(&self, name: &str, limit: usize) -> Result<Option<StoredBytes>> {
        self.stored_bounded(name, limit)
            .map(|value| value.map(EntryBacking::Native))
    }
    fn stored_range_owned(
        &self,
        name: &str,
        _offset: u64,
        _len: usize,
    ) -> Result<Option<StoredBytes>> {
        Err(bad(name, "stored range read unsupported by source"))
    }
    fn stored_bounded(&self, name: &str, _limit: usize) -> Result<Option<Vec<u8>>> {
        Err(bad(name, "bounded stored read unsupported by source"))
    }
    fn stored_inflated_bounded(&self, name: &str, _limit: u64) -> Result<Option<Inflated>> {
        Err(bad(name, "bounded inflated read unsupported by source"))
    }
    fn stored_len(&self, name: &str) -> Result<Option<u64>> {
        Err(bad(name, "stored size unsupported by source"))
    }
}

#[derive(Debug)]
pub struct Inflated {
    pub bytes: memory_budget::Charged<Vec<u8>>,
    pub was_deflated: bool,
}

pub struct BudgetedEntries<'a> {
    inner: &'a dyn EntrySource,
    budget: memory_budget::MemoryBudget,
}

impl<'a> BudgetedEntries<'a> {
    pub fn new(inner: &'a dyn EntrySource, budget: memory_budget::MemoryBudget) -> Self {
        Self { inner, budget }
    }
}

fn memory_error(name: &str, error: memory_budget::BudgetError) -> Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => Error::Memory(limit),
        other => Error::Io(std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            format!("{name}: {other}"),
        )),
    }
}

impl EntrySource for BudgetedEntries<'_> {
    fn decoded_cache_capacity(&self) -> Option<std::num::NonZeroUsize> {
        self.inner.decoded_cache_capacity()
    }
    fn list(&self) -> Result<Vec<EntryMeta>> {
        self.inner.list()
    }
    fn list_owned(&self) -> Result<EntryList> {
        self.inner.list_owned()
    }
    fn stored_owned_bounded(&self, name: &str, limit: usize) -> Result<Option<StoredBytes>> {
        self.inner.stored_owned_bounded(name, limit)
    }
    fn stored_range_owned(
        &self,
        name: &str,
        offset: u64,
        len: usize,
    ) -> Result<Option<StoredBytes>> {
        self.inner.stored_range_owned(name, offset, len)
    }
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Err(bad(name, "budgeted source reads entries inflated only"))
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        self.inner.stored_bounded(name, limit)
    }
    fn stored_inflated_bounded(&self, name: &str, limit: u64) -> Result<Option<Inflated>> {
        let limit = limit.min(zraw::DEFAULT_LIMIT);
        let Some(len) = self.inner.stored_len(name)? else {
            return Ok(None);
        };
        if len > limit {
            return Err(bad(name, "stored size exceeds product limit"));
        }
        let category = memory_budget::Category::Metadata;
        let held = self
            .budget
            .try_reserve(category, len)
            .map_err(Error::Memory)?;
        let cap = usize::try_from(len).unwrap_or(usize::MAX);
        let Some(stored) = self.inner.stored_owned_bounded(name, cap)? else {
            return Ok(None);
        };
        let out = match zraw::inflate_budgeted(&stored, limit, &self.budget, category) {
            Ok(bytes) => Inflated {
                bytes,
                was_deflated: true,
            },
            Err(zraw::ManagedInflateError::Memory(e)) => return Err(memory_error(name, e)),
            Err(_) => {
                let mut copy = memory_budget::BudgetedVec::try_with_capacity(
                    &self.budget,
                    category,
                    stored.len(),
                )
                .map_err(|e| memory_error(name, e))?;
                copy.try_extend_from_slice(&stored)
                    .map_err(|e| memory_error(name, e))?;
                Inflated {
                    bytes: copy.into_charged_vec(),
                    was_deflated: false,
                }
            }
        };
        drop(stored);
        drop(held);
        Ok(Some(out))
    }
}

pub trait EntrySink {
    fn put(&mut self, meta: &EntryMeta, stored: &[u8]) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
}

pub struct CfFile {
    c: v8cont::Container<File>,
}

impl CfFile {
    pub fn open(path: &Path) -> Result<CfFile> {
        let c = v8cont::Container::open(File::open(path)?)?;
        Ok(CfFile { c })
    }

    pub(crate) fn is_header_only(&self) -> bool {
        let format = self.c.format();
        self.c.file_len() == format.base() + format.file_header_size()
    }
}

impl EntrySource for CfFile {
    fn list(&self) -> Result<Vec<EntryMeta>> {
        Ok(self.c.entries().iter().map(meta_of).collect())
    }
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.c.find(name) {
            Some(e) => Ok(Some(self.c.read_data(e)?)),
            None => Ok(None),
        }
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let Some(e) = self.c.find(name) else {
            return Ok(None);
        };
        if e.data_len > limit as u64 {
            return Err(bad(name, "stored size exceeds product limit"));
        }
        let mut reader = self.c.data_reader(e);
        Ok(Some(read_bounded(&mut reader, name, limit)?))
    }
    fn stored_len(&self, name: &str) -> Result<Option<u64>> {
        Ok(self.c.find(name).map(|e| e.data_len))
    }
}

fn meta_of(e: &v8cont::EntryInfo) -> EntryMeta {
    EntryMeta {
        name: e.name.clone(),
        ctime: e.ctime,
        mtime: e.mtime,
        attrs: e.attrs,
    }
}

pub struct CfWriter {
    path: PathBuf,
    format: v8cont::Format,
    specs: Vec<v8cont::EntrySpec>,
    datas: Vec<Vec<u8>>,
    finished: bool,
}

impl CfWriter {
    pub fn create(path: &Path, format: v8cont::Format) -> CfWriter {
        CfWriter {
            path: path.to_path_buf(),
            format,
            specs: Vec::new(),
            datas: Vec::new(),
            finished: false,
        }
    }
}

fn temp_file(dir: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    #[cfg(unix)]
    let mut b = tempfile::Builder::new();
    #[cfg(not(unix))]
    let b = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        b.permissions(std::fs::Permissions::from_mode(0o666));
    }
    b.tempfile_in(dir)
}

impl EntrySink for CfWriter {
    fn put(&mut self, meta: &EntryMeta, stored: &[u8]) -> Result<()> {
        if self.finished {
            return Err(Error::Format {
                entry: meta.name.clone(),
                what: "put after finish".into(),
            });
        }
        self.specs.push(v8cont::EntrySpec {
            name: meta.name.clone(),
            ctime: meta.ctime,
            mtime: meta.mtime,
            attrs: meta.attrs,
            data_len: Some(stored.len() as u64),
        });
        self.datas.push(stored.to_vec());
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let dir = match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut tmp = temp_file(dir)?;
        {
            let mut w = std::io::BufWriter::new(tmp.as_file_mut());
            v8cont::write_container(
                &mut w,
                &self.specs,
                &mut v8cont::VecData(&self.datas),
                &v8cont::WriteOptions::top_level(self.format),
            )?;
            std::io::Write::flush(&mut w)?;
        }
        tmp.persist(&self.path).map_err(|e| e.error)?;
        self.finished = true;
        self.datas = Vec::new();
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemEntries {
    items: Vec<(EntryMeta, Vec<u8>)>,
}

impl MemEntries {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, meta: EntryMeta, stored: Vec<u8>) {
        self.items.push((meta, stored));
    }
}

impl EntrySource for MemEntries {
    fn list(&self) -> Result<Vec<EntryMeta>> {
        Ok(self.items.iter().map(|(m, _)| m.clone()).collect())
    }
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .items
            .iter()
            .find(|(m, _)| m.name == name)
            .map(|(_, d)| d.clone()))
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        self.items
            .iter()
            .find(|(m, _)| m.name == name)
            .map(|(_, bytes)| {
                if bytes.len() > limit {
                    Err(bad(name, "stored size exceeds product limit"))
                } else {
                    Ok(bytes.clone())
                }
            })
            .transpose()
    }
    fn stored_len(&self, name: &str) -> Result<Option<u64>> {
        Ok(self
            .items
            .iter()
            .find(|(m, _)| m.name == name)
            .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX)))
    }
}

impl EntrySink for MemEntries {
    fn put(&mut self, meta: &EntryMeta, stored: &[u8]) -> Result<()> {
        self.items.push((meta.clone(), stored.to_vec()));
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

pub use md_model::EntryData;

pub fn decode_stored(stored: &[u8]) -> Result<EntryData> {
    if stored.is_empty() {
        return Ok(EntryData::Bytes(Vec::new()));
    }
    let Some(raw) = zraw::try_inflate(stored, zraw::DEFAULT_LIMIT) else {
        return Ok(EntryData::Bytes(stored.to_vec()));
    };
    decode_raw(Cow::Owned(raw))
}

pub fn decode_inflated(bytes: &[u8], was_deflated: bool) -> Result<EntryData> {
    if !was_deflated {
        return Ok(EntryData::Bytes(bytes.to_vec()));
    }
    decode_raw(Cow::Borrowed(bytes))
}

fn decode_raw(raw: Cow<'_, [u8]>) -> Result<EntryData> {
    if v8cont::is_container(&raw) {
        if let Some(items) = read_nested(&raw) {
            return Ok(EntryData::Nested(items));
        }
        return Ok(EntryData::Bytes(raw.into_owned()));
    }
    let text = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    if text.first() == Some(&b'{') {
        if brace::is_json_entry(&raw) {
            return Ok(EntryData::Json(raw.into_owned()));
        }
        if let Ok(doc) = brace::parse_doc(&raw) {
            return Ok(EntryData::Brace(doc));
        }
    }
    Ok(EntryData::Bytes(raw.into_owned()))
}

fn read_nested(raw: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let c = v8cont::Container::open(raw).ok()?;
    c.entries()
        .iter()
        .map(|e| Some((e.name.clone(), c.read_data(e).ok()?)))
        .collect()
}

#[doc(hidden)]
pub struct Entries<'a> {
    src: &'a dyn EntrySource,
    #[allow(dead_code)]
    names: HashSet<String>,
    list: EntryList,
    cache: RefCell<HashMap<String, Arc<EntryData>>>,
    root: RefCell<Option<Arc<crate::RootInfo>>>,
    meter: Option<Arc<dyn LoadMeter>>,
    held: RefCell<HashMap<String, Hold>>,
    orphans: RefCell<Orphans>,
    refused: std::cell::Cell<Option<memory_budget::MemoryLimit>>,
    #[cfg(any(test, feature = "testing"))]
    visits: std::cell::Cell<u64>,
}

impl<'a> Entries<'a> {
    #[allow(dead_code)]
    pub fn new(src: &'a dyn EntrySource) -> Result<Self> {
        Self::with_meter(src, None)
    }

    pub fn with_meter(src: &'a dyn EntrySource, meter: Option<Arc<dyn LoadMeter>>) -> Result<Self> {
        let list = src.list_owned()?;
        let names = list.iter().map(|m| m.name.clone()).collect();
        Ok(Entries {
            src,
            list,
            names,
            cache: RefCell::new(HashMap::new()),
            root: RefCell::new(None),
            meter,
            held: RefCell::new(HashMap::new()),
            orphans: RefCell::new(Orphans::default()),
            refused: std::cell::Cell::new(None),
            #[cfg(any(test, feature = "testing"))]
            visits: std::cell::Cell::new(0),
        })
    }

    pub fn check_refused(&self) -> Result<()> {
        match self.refused.get() {
            Some(limit) => Err(Error::Memory(limit)),
            None => Ok(()),
        }
    }

    fn refuse(&self, limit: memory_budget::MemoryLimit) -> Error {
        if self.refused.get().is_none() {
            self.refused.set(Some(limit));
        }
        Error::Memory(limit)
    }

    fn remember_refusal<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|error| match error {
            Error::Memory(limit) => self.refuse(limit),
            other => other,
        })
    }

    fn settle(&self, meter: &dyn LoadMeter, hold: Hold) {
        #[cfg(any(test, feature = "testing"))]
        self.visits.set(self.visits.get() + 1);
        if hold.data.strong_count() == 0 {
            meter.release(hold.bytes);
        } else {
            self.orphans.borrow_mut().list.push(hold);
        }
    }

    fn sweep_orphans(&self, meter: &dyn LoadMeter, full: bool) {
        let mut orphans = self.orphans.borrow_mut();
        let len = orphans.list.len();
        if len == 0 || (!full && len > ORPHANS_EAGER && len < orphans.survivors.saturating_mul(2)) {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        self.visits.set(self.visits.get() + len as u64);
        orphans.list.retain(|hold| {
            let alive = hold.data.strong_count() > 0;
            if !alive {
                meter.release(hold.bytes);
            }
            alive
        });
        orphans.survivors = orphans.list.len();
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn hold_visits(&self) -> u64 {
        self.visits.get()
    }

    fn metered(
        &self,
        meter: &dyn LoadMeter,
        name: &str,
        copy: bool,
    ) -> Result<Option<(Inflated, u64, u64)>> {
        self.check_refused()?;
        self.sweep_orphans(meter, false);
        let inflated = match self.src.stored_inflated_bounded(name, zraw::DEFAULT_LIMIT) {
            Ok(Some(inflated)) => inflated,
            Ok(None) => return Ok(None),
            Err(Error::Memory(limit)) => return Err(self.refuse(limit)),
            Err(e) => return Err(e),
        };
        let bytes: &[u8] = &inflated.bytes;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        #[cfg(any(test, feature = "testing"))]
        cache_managed::trace::record("admit", name);
        let (hold, transient) = if copy {
            (0, 0)
        } else {
            decoded_bound(bytes, inflated.was_deflated)
        };
        let admitted = if copy {
            meter.admit_copy(name, len)
        } else {
            meter.admit(name, len).and_then(|()| meter.hold(name, hold))
        };
        admitted.map_err(|MeterRefusal(limit)| self.refuse(limit))?;
        Ok(Some((inflated, hold, transient)))
    }

    pub fn root_info(&self) -> Result<Arc<crate::RootInfo>> {
        self.check_refused()?;
        if let Some(info) = self.root.borrow().as_ref() {
            return Ok(Arc::clone(info));
        }
        let meta = self
            .list
            .iter()
            .find(|m| m.name == "root")
            .ok_or_else(|| Error::MissingEntry("root".into()))?
            .clone();
        if let Some(meter) = &self.meter {
            let bound = crate::root::STORED_LIMIT
                .saturating_add(crate::root::INFLATED_LIMIT)
                .saturating_add(crate::root::DECODED_LIMIT);
            meter
                .admit_copy("root", u64::try_from(bound).unwrap_or(u64::MAX))
                .map_err(|MeterRefusal(limit)| self.refuse(limit))?;
        }
        let stored = self
            .remember_refusal(
                self.src
                    .stored_owned_bounded("root", crate::root::STORED_LIMIT),
            )?
            .ok_or_else(|| Error::MissingEntry("root".into()))?;
        if stored.len() > crate::root::STORED_LIMIT {
            return Err(bad("root", "stored size exceeds product limit"));
        }
        let info =
            Arc::new(self.remember_refusal(crate::root::RootInfo::read_owned(meta, stored))?);
        *self.root.borrow_mut() = Some(Arc::clone(&info));
        Ok(info)
    }

    #[allow(dead_code)]
    pub fn list(&self) -> &[EntryMeta] {
        &self.list
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn get(&self, name: &str) -> Result<Option<Arc<EntryData>>> {
        self.check_refused()?;
        #[cfg(any(test, feature = "testing"))]
        cache_managed::trace::record("get", name);
        if let Some(d) = self.cache.borrow().get(name) {
            return Ok(Some(Arc::clone(d)));
        }
        if !self.contains(name) {
            return Ok(None);
        }
        if let Some(capacity) = self.src.decoded_cache_capacity() {
            while self.cache.borrow().len() >= capacity.get() {
                let victim = self.cache.borrow().keys().next().cloned();
                let Some(victim) = victim else { break };
                self.forget(&victim);
            }
        }
        let data = if let Some(meter) = &self.meter {
            let Some((inflated, hold, transient)) = self.metered(&**meter, name, false)? else {
                return Ok(None);
            };
            let data = match decode_inflated(&inflated.bytes, inflated.was_deflated) {
                Ok(data) => Arc::new(data),
                Err(e) => {
                    meter.release(hold);
                    return Err(e);
                }
            };
            drop(inflated);
            meter.release(transient);
            let kept = Hold {
                data: Arc::downgrade(&data),
                bytes: hold.saturating_sub(transient),
            };
            if let Some(earlier) = self.held.borrow_mut().insert(name.to_string(), kept) {
                self.settle(&**meter, earlier);
            }
            data
        } else {
            let Some(stored) = self.remember_refusal(self.src.stored_owned(name))? else {
                return Ok(None);
            };
            Arc::new(self.remember_refusal(decode_stored(&stored))?)
        };
        self.cache
            .borrow_mut()
            .insert(name.to_string(), Arc::clone(&data));
        Ok(Some(data))
    }

    pub fn doc(&self, name: &str) -> Result<Option<Arc<EntryData>>> {
        #[cfg(any(test, feature = "testing"))]
        cache_managed::trace::record("doc", name);
        Ok(self
            .get(name)?
            .filter(|d| matches!(**d, EntryData::Brace(_))))
    }

    #[allow(dead_code)]
    pub fn forget(&self, name: &str) {
        #[cfg(any(test, feature = "testing"))]
        cache_managed::trace::record("forget", name);
        let removed = self.cache.borrow_mut().remove(name);
        drop(removed);
        let Some(meter) = &self.meter else { return };
        let hold = self.held.borrow_mut().remove(name);
        if let Some(hold) = hold {
            self.settle(&**meter, hold);
        }
        self.sweep_orphans(&**meter, false);
    }

    #[doc(hidden)]
    pub fn raw(&self, name: &str) -> Result<Option<StoredBytes>> {
        self.check_refused()?;
        #[cfg(any(test, feature = "testing"))]
        cache_managed::trace::record("raw", name);
        if !self.contains(name) {
            return Ok(None);
        }
        if let Some(meter) = &self.meter {
            return Ok(self
                .metered(&**meter, name, true)?
                .map(|(inflated, _, _)| EntryBacking::Native(inflated.bytes.to_vec())));
        }
        let Some(stored) = self.remember_refusal(self.src.stored_owned(name))? else {
            return Ok(None);
        };
        if let Some(budget) = stored.budget() {
            let result = match zraw::inflate_budgeted(
                &stored,
                zraw::DEFAULT_LIMIT,
                budget,
                memory_budget::Category::Metadata,
            ) {
                Ok(bytes) => EntryBacking::from_charged(bytes)
                    .map(Some)
                    .map_err(|e| memory_error(name, e)),
                Err(zraw::ManagedInflateError::Memory(error)) => Err(memory_error(name, error)),
                Err(_) => Ok(Some(stored)),
            };
            return self.remember_refusal(result);
        }
        Ok(Some(
            match zraw::try_inflate(&stored, zraw::DEFAULT_LIMIT) {
                Some(bytes) => EntryBacking::Native(bytes),
                None => stored,
            },
        ))
    }
}

impl Drop for Entries<'_> {
    fn drop(&mut self) {
        self.cache.borrow_mut().clear();
        let Some(meter) = self.meter.clone() else {
            return;
        };
        let held = std::mem::take(&mut *self.held.borrow_mut());
        for hold in held.into_values() {
            self.settle(&*meter, hold);
        }
        self.sweep_orphans(&*meter, true);
    }
}

struct Hold {
    data: std::sync::Weak<EntryData>,
    bytes: u64,
}

#[derive(Default)]
struct Orphans {
    list: Vec<Hold>,
    survivors: usize,
}

const ORPHANS_EAGER: usize = 64;

fn brace_shape(text: &[u8]) -> (u64, u64) {
    let (mut opens, mut commas) = (0u64, 0u64);
    for &b in text {
        match b {
            b'{' => opens += 1,
            b',' => commas += 1,
            _ => {}
        }
    }
    (opens.saturating_add(commas).saturating_add(1), opens)
}

fn decoded_bound(bytes: &[u8], was_deflated: bool) -> (u64, u64) {
    const FIXED: u64 = 4096;
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let container = was_deflated && v8cont::is_container(bytes);
    let (slots, transient) = if was_deflated && !container && text.first() == Some(&b'{') {
        let node = u64::try_from(std::mem::size_of::<brace::Node>()).unwrap_or(u64::MAX);
        let (nodes, lists) = brace_shape(text);
        let kept = nodes
            .saturating_mul(2)
            .saturating_add(lists.saturating_mul(4))
            .saturating_mul(node);
        (kept, nodes.saturating_mul(node))
    } else if container {
        (len / 4, 0)
    } else {
        (0, 0)
    };
    let kept = slots.saturating_add(len).saturating_add(FIXED);
    (kept.saturating_add(transient), transient)
}

fn brace_doc(e: &Entries, name: &str) -> Result<Arc<EntryData>> {
    e.doc(name)?.ok_or_else(|| {
        if e.contains(name) {
            Error::Format {
                entry: name.into(),
                what: "not a brace document".into(),
            }
        } else {
            Error::MissingEntry(name.into())
        }
    })
}

fn bad(entry: &str, what: &str) -> Error {
    Error::Format {
        entry: entry.into(),
        what: what.into(),
    }
}

pub(crate) fn root_uuid(e: &Entries) -> Result<brace::Guid> {
    Ok(e.root_info()?.id())
}

pub(crate) fn configuration_version(e: &Entries) -> Result<(mdschema::ConfigFormat, u32)> {
    let d = brace_doc(e, "version")?;
    let EntryData::Brace(doc) = &*d else {
        return Err(bad("version", "not a brace document"));
    };
    let number = |path: &[usize], what| {
        doc.root
            .get(path)
            .and_then(brace::Node::as_u32)
            .ok_or_else(|| bad("version", what))
    };
    let format = mdschema::ConfigFormat {
        version: number(&[0, 0], "no configuration format")?,
        revision: number(&[0, 1], "no configuration format revision")?,
    };
    Ok((format, number(&[0, 2, 0], "no compatibility version")?))
}

fn read_bounded(reader: &mut impl Read, name: &str, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        if n > limit.saturating_sub(out.len()) {
            return Err(bad(name, "stored size exceeds product limit"));
        }
        out.extend_from_slice(
            buf.get(..n)
                .ok_or_else(|| bad(name, "invalid reader length"))?,
        );
    }
}

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub use cache_managed::trace as managed_trace;
