use mdconf::ConfigurationExt;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use dtfile::elements::{RawEvent, RawReader, Source};
use ibschema::values::ColumnValue;
use mdconf::{Configuration, EntryMeta, EntrySource, LoadOptions};

use super::OpenMemory;
use crate::Result;
use memory_budget::{BudgetedVec, Category, MemoryBudget};

#[derive(Clone, Copy)]
struct Limits {
    entry: u64,
    aggregate: u64,
    count: usize,
    prefix: u64,
    retained: u64,
    nodes: u64,
}
impl Limits {
    fn from_budget(budget: u64) -> Result<Self> {
        let entry = (budget / 16).min(256 << 20);
        let retained = entry
            .checked_add(64 << 10)
            .ok_or("DT scratch size overflow")?;
        Ok(Self {
            entry,
            aggregate: budget
                .checked_mul(8)
                .ok_or("DT aggregate size overflow")?
                .min(64 << 30),
            count: usize::try_from((budget / 512).min(1_048_576))?,
            prefix: budget
                .checked_mul(32)
                .ok_or("DT prefix size overflow")?
                .min(256 << 30),
            retained,
            nodes: (retained / 32).min(65_536),
        })
    }
    fn scratch(self) -> Result<u64> {
        self.retained
            .checked_mul(8)
            .and_then(|n| {
                self.nodes
                    .checked_mul(16)?
                    .checked_mul(std::mem::size_of::<brace::binary::BinNode>() as u64)?
                    .checked_add(n)
            })
            .and_then(|n| n.checked_add(1 << 20))
            .ok_or_else(|| "DT scratch size overflow".into())
    }
}
#[derive(Debug, thiserror::Error)]
enum QuotaRefusal {
    #[error("MEMORY_LIMIT: DT CONFIG {0}; increase --memory-limit")]
    Budget(&'static str),
    #[error("DT CONFIG {0} exceeds independent safety ceiling")]
    Safety(&'static str),
}
fn quota(resource: &'static str, hard: bool) -> crate::Error {
    Box::new(if hard {
        QuotaRefusal::Safety(resource)
    } else {
        QuotaRefusal::Budget(resource)
    })
}
fn reader_error(error: dtfile::Error, limits: Limits) -> crate::Error {
    match error {
        dtfile::Error::Limit { what, .. } => {
            let hard = match what {
                "atom bytes" | "retained bytes" => limits.entry == 256 << 20,
                "nodes" => limits.nodes == 65_536,
                _ => true,
            };
            quota(what, hard)
        }
        other => other.into(),
    }
}
#[derive(Debug, thiserror::Error)]
#[error("{source}; increase --memory-limit")]
struct MemoryRefusal {
    #[source]
    source: crate::Error,
}
pub(super) fn memory_hint(error: crate::Error) -> crate::Error {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&*error);
    let mut limit = false;
    while let Some(cause) = current {
        if cause.downcast_ref::<QuotaRefusal>().is_some()
            || cause.downcast_ref::<MemoryRefusal>().is_some()
        {
            return error;
        }
        limit |= cause.downcast_ref::<memory_budget::MemoryLimit>().is_some()
            || matches!(
                cause.downcast_ref::<memory_budget::BudgetError>(),
                Some(memory_budget::BudgetError::Limit(_))
            )
            || matches!(
                cause.downcast_ref::<mdconf::Error>(),
                Some(mdconf::Error::Memory(_))
            );
        current = cause.source();
    }
    if limit {
        Box::new(MemoryRefusal { source: error })
    } else {
        error
    }
}
fn source_memory(name: &str, error: memory_budget::BudgetError) -> mdconf::Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => mdconf::Error::Memory(limit),
        other => mdconf::Error::Io(io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("{name}: {other}"),
        )),
    }
}
fn copy_name(name: &str) -> mdconf::Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(name.len())
        .map_err(|_| source_memory(name, memory_budget::BudgetError::AllocationFailed))?;
    copy.push_str(name);
    Ok(copy)
}
fn name_order(first: &str, second: &str) -> std::cmp::Ordering {
    first
        .bytes()
        .map(|b| b.to_ascii_lowercase())
        .cmp(second.bytes().map(|b| b.to_ascii_lowercase()))
}

struct Counted<R> {
    inner: R,
    bytes: Rc<Cell<u64>>,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        let next = self
            .bytes
            .get()
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("DT compressed byte counter overflow"))?;
        self.bytes.set(next);
        Ok(n)
    }
}

struct Guarded<'a, R, F> {
    inner: R,
    bytes: u64,
    limit: u64,
    exhaustion: Option<(&'static str, bool)>,
    progress: &'a mut F,
    cancelled: Rc<RefCell<Option<crate::Error>>>,
}
impl<R: Read, F: FnMut(&'static str) -> Result<()>> Read for Guarded<'_, R, F> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if let Err(e) = (self.progress)("dt-config") {
            *self.cancelled.borrow_mut() = Some(e);
            return Err(io::Error::other("DT metadata extraction cancelled"));
        }
        let remaining = self
            .limit
            .checked_sub(self.bytes)
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                if let Some((resource, hard)) = self.exhaustion {
                    *self.cancelled.borrow_mut() = Some(quota(resource, hard));
                }
                io::Error::other("DT metadata read byte limit")
            })?;
        let len = out
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let slice = out
            .get_mut(..len)
            .ok_or_else(|| io::Error::other("DT read range"))?;
        let n = self.inner.read(slice)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

struct Item {
    offset: u64,
    len: u64,
}
pub(super) struct Spool {
    file: Mutex<File>,
    temp_dir: PathBuf,
    _metadata: memory_budget::Reservation,
    metadata: BudgetedVec<EntryMeta>,
    items: BudgetedVec<Item>,
    index: BudgetedVec<usize>,
    budget: MemoryBudget,
    limits: Limits,
    bytes: u64,
}
impl Spool {
    fn new(memory: &OpenMemory, limits: Limits) -> Result<Self> {
        let temp_dir = std::env::temp_dir();
        let metadata = memory.budget.try_reserve(
            Category::Metadata,
            std::mem::size_of::<Self>() as u64 + 128 + temp_dir.as_os_str().len() as u64 * 4,
        )?;
        let file = super::dt_spill::create(&temp_dir)?;
        Ok(Self {
            file: Mutex::new(file),
            temp_dir,
            _metadata: metadata,
            metadata: BudgetedVec::new(&memory.budget, Category::Metadata)?,
            items: BudgetedVec::new(&memory.budget, Category::Metadata)?,
            index: BudgetedVec::new(&memory.budget, Category::Metadata)?,
            budget: memory.budget.clone(),
            limits,
            bytes: 0,
        })
    }
    pub(super) fn summary(&self) -> super::ConfigEntrySummary {
        super::ConfigEntrySummary {
            entries: self.items.len(),
            stored_bytes: self.bytes,
            largest_entry: self.items.iter().map(|i| i.len).max().unwrap_or(0),
        }
    }
    pub(super) fn largest_range(
        &self,
        offset: u64,
        len: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        match self
            .items
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| item.len)
        {
            Some((at, _)) => match self.metadata.get(at) {
                Some(meta) => self.stored_range_owned(&meta.name, offset, len),
                None => Err(source_memory(
                    "CONFIG",
                    memory_budget::BudgetError::Overflow,
                )),
            },
            None => Ok(None),
        }
    }
    fn next_extent(&self, len: u64) -> Result<u64> {
        if self.items.len() >= self.limits.count {
            return Err(quota("entry count", self.limits.count == 1_048_576));
        }
        self.bytes
            .checked_add(len)
            .filter(|n| *n <= self.limits.aggregate)
            .ok_or_else(|| quota("stored aggregate", self.limits.aggregate == 64 << 30))
    }
    fn prepare(&mut self, row: &[brace::binary::BinNode], offset: u64) -> Result<(EntryMeta, u64)> {
        let columns = dtfile::layout::folder();
        let values = dtfile::rows::decode_row(&columns, row, offset)?;
        for (column, value) in columns.iter().zip(&values) {
            column
                .check(value)
                .map_err(|e| format!("DT CONFIG {}: {e}", column.name))?;
        }
        let [
            ColumnValue::Str(name),
            ColumnValue::DateTime(created),
            ColumnValue::DateTime(modified),
            ColumnValue::Number(attrs),
            ColumnValue::Number(size),
            ColumnValue::Bytes(_),
        ] = values.as_slice()
        else {
            return Err("DT CONFIG row has missing or invalid fields".into());
        };
        if name.is_empty() || name.contains('\0') {
            return Err("DT CONFIG invalid entry name".into());
        }
        let declared = size
            .to_string()
            .parse::<u64>()
            .map_err(|_| "DT CONFIG invalid DATASIZE")?;
        let attrs = attrs
            .to_string()
            .parse::<u32>()
            .map_err(|_| "DT CONFIG invalid ATTRIBUTES")?;
        self.next_extent(declared)?;
        self.metadata
            .reservation()
            .try_grow(u64::try_from(name.len())?)?;
        self.metadata.try_reserve(1)?;
        self.items.try_reserve(1)?;
        Ok((
            EntryMeta {
                name: copy_name(name)?,
                ctime: infobase::config::v8time_of(created),
                mtime: infobase::config::v8time_of(modified),
                attrs,
            },
            declared,
        ))
    }
    fn append(&mut self, meta: EntryMeta, len: u64) -> Result<()> {
        let next = self.next_extent(len)?;
        self.metadata.try_push(meta)?;
        self.items.try_push(Item {
            offset: self.bytes,
            len,
        })?;
        self.bytes = next;
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        if self.metadata.len() != self.items.len() {
            return Err("DT CONFIG metadata/location length mismatch".into());
        }
        self.index.try_reserve(self.items.len())?;
        for index in 0..self.items.len() {
            self.index.try_push(index)?;
        }
        let name = |index: usize| {
            self.metadata
                .get(index)
                .map(|meta| meta.name.as_str())
                .unwrap_or_default()
        };
        self.index
            .sort_unstable_by(|a, b| name_order(name(*a), name(*b)));
        if self
            .index
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if name_order(name(*a), name(*b)).is_eq()))
        {
            return Err("DT CONFIG duplicate entry name".into());
        }
        Ok(())
    }
    fn find(&self, name: &str) -> Option<&Item> {
        let index = self
            .index
            .binary_search_by(|i| {
                name_order(
                    self.metadata
                        .get(*i)
                        .map(|meta| meta.name.as_str())
                        .unwrap_or_default(),
                    name,
                )
            })
            .ok()?;
        self.items.get(*self.index.get(index)?)
    }
}
#[derive(Debug)]
pub enum ManagedSpoolError {
    Memory(memory_budget::BudgetError),
    Io(io::Error),
    Poisoned,
}
impl std::fmt::Display for ManagedSpoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Poisoned => f.write_str("DT CONFIG spool lock poisoned"),
        }
    }
}
impl std::error::Error for ManagedSpoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Memory(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Poisoned => None,
        }
    }
}
impl From<memory_budget::BudgetError> for ManagedSpoolError {
    fn from(error: memory_budget::BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<io::Error> for ManagedSpoolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl mdconf::ManagedEntrySource for Spool {
    type Error = ManagedSpoolError;
    fn list(&self) -> &[EntryMeta] {
        &self.metadata
    }
    fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    fn stored(
        &self,
        name: &str,
    ) -> std::result::Result<Option<memory_budget::Charged<Vec<u8>>>, Self::Error> {
        mdconf::ManagedEntrySource::stored_bounded(
            self,
            name,
            usize::try_from(self.limits.entry).unwrap_or(usize::MAX),
        )
    }
    fn stored_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> std::result::Result<Option<memory_budget::Charged<Vec<u8>>>, Self::Error> {
        let Some(item) = self.find(name) else {
            return Ok(None);
        };
        let limit = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(self.limits.entry);
        if item.len > limit {
            return Err(
                memory_budget::BudgetError::Limit(memory_budget::MemoryLimit {
                    category: Category::Metadata,
                    requested: item.len,
                    reason: memory_budget::LimitReason::Global,
                })
                .into(),
            );
        }
        let len = usize::try_from(item.len).map_err(|_| memory_budget::BudgetError::Overflow)?;
        let mut out = BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, len)?;
        out.try_resize(len, 0)?;
        let mut file = self.file.lock().map_err(|_| ManagedSpoolError::Poisoned)?;
        file.seek(SeekFrom::Start(item.offset))?;
        file.read_exact(&mut out)?;
        Ok(Some(out.into_charged_vec()))
    }
}

impl EntrySource for Spool {
    fn decoded_cache_capacity(&self) -> Option<std::num::NonZeroUsize> {
        std::num::NonZeroUsize::new(1)
    }

    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Err(mdconf::Error::Format {
            entry: "CONFIG".into(),
            what: "charged source requires list_owned".into(),
        })
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        Err(mdconf::Error::Format {
            entry: name.into(),
            what: "charged source requires stored_owned".into(),
        })
    }
    fn list_owned(&self) -> mdconf::Result<mdconf::EntryList> {
        let mut list =
            BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, self.items.len())
                .map_err(|e| source_memory("CONFIG", e))?;
        let extra = self
            .metadata
            .iter()
            .try_fold(0u64, |n, meta| {
                u64::try_from(meta.name.len())
                    .ok()?
                    .checked_mul(16)?
                    .checked_add(1024)?
                    .checked_add(n)
            })
            .ok_or_else(|| source_memory("CONFIG", memory_budget::BudgetError::Overflow))?;
        list.reservation()
            .try_grow(extra)
            .map_err(mdconf::Error::Memory)?;
        for meta in self.metadata.iter() {
            list.try_push(EntryMeta {
                name: copy_name(&meta.name)?,
                ctime: meta.ctime,
                mtime: meta.mtime,
                attrs: meta.attrs,
            })
            .map_err(|e| source_memory("CONFIG", e))?;
        }
        mdconf::EntryBacking::from_charged(list.into_charged_vec())
            .map_err(|e| source_memory("CONFIG", e))
    }
    fn stored_owned(&self, name: &str) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        self.stored_owned_bounded(
            name,
            usize::try_from(self.limits.entry).unwrap_or(usize::MAX),
        )
    }
    fn stored_owned_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        let Some(item) = self.find(name) else {
            return Ok(None);
        };
        let bad = |what: &str| mdconf::Error::Format {
            entry: name.into(),
            what: what.into(),
        };
        if item.len > limit as u64 {
            return Err(mdconf::Error::Memory(memory_budget::MemoryLimit {
                category: Category::Metadata,
                requested: item.len,
                reason: memory_budget::LimitReason::Global,
            }));
        }
        let len =
            usize::try_from(item.len).map_err(|_| bad("stored size exceeds address space"))?;
        let mut out = BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, len)
            .map_err(|e| source_memory(name, e))?;
        out.try_resize(len, 0).map_err(|e| source_memory(name, e))?;
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(item.offset))?;
        file.read_exact(&mut out)?;
        Ok(Some(
            mdconf::EntryBacking::from_charged(out.into_charged_vec())
                .map_err(|e| source_memory(name, e))?,
        ))
    }
    fn stored_range_owned(
        &self,
        name: &str,
        offset: u64,
        requested: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        let Some(item) = self.find(name) else {
            return Ok(None);
        };
        let bad = |what: &str| mdconf::Error::Format {
            entry: name.into(),
            what: what.into(),
        };
        if requested as u64 > self.limits.entry {
            return Err(bad("range size exceeds in-memory limit"));
        }
        let remaining = item
            .len
            .checked_sub(offset)
            .ok_or_else(|| bad("range offset exceeds stored size"))?;
        let len = requested.min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let mut out = BudgetedVec::try_with_capacity(&self.budget, Category::Metadata, len)
            .map_err(|e| source_memory(name, e))?;
        out.try_resize(len, 0).map_err(|e| source_memory(name, e))?;
        let at = item
            .offset
            .checked_add(offset)
            .ok_or_else(|| bad("range offset overflow"))?;
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut out)?;
        Ok(Some(
            mdconf::EntryBacking::from_charged(out.into_charged_vec())
                .map_err(|e| source_memory(name, e))?,
        ))
    }
    fn stored_len(&self, name: &str) -> mdconf::Result<Option<u64>> {
        Ok(self.find(name).map(|item| item.len))
    }
}

pub(super) fn open(
    path: &Path,
    opts: &LoadOptions,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    let memory = OpenMemory::resolve(None)?;
    open_with_memory(path, opts, &memory, progress)
}
pub(super) fn open_with_memory(
    path: &Path,
    opts: &LoadOptions,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    let spool = extract_with_memory(path, memory, progress)?;
    progress("metadata")?;
    Configuration::load(&spool, opts).map_err(|error| memory_hint(error.into()))
}

pub(super) fn extract(
    path: &Path,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Spool> {
    let memory = OpenMemory::resolve(None)?;
    extract_with_memory(path, &memory, progress)
}

pub(super) fn extract_with_memory(
    path: &Path,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Spool> {
    extract_inner(path, memory, progress).map_err(memory_hint)
}
fn extract_inner(
    path: &Path,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Spool> {
    let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::DtExtract);
    let limits = Limits::from_budget(memory.startup)?;
    let _scratch = memory
        .budget
        .try_reserve(Category::ReaderWorking, limits.scratch()?)?;
    let file = File::open(path)?;
    let before = file.metadata()?;
    let compressed = Rc::new(Cell::new(0));
    let cancelled = Rc::new(RefCell::new(None));
    let shared_progress = RefCell::new(progress);
    let mut compressed_progress = |stage| (shared_progress.borrow_mut())(stage);
    let mut inflated_progress = |stage| (shared_progress.borrow_mut())(stage);
    let compressed_guard = Guarded {
        inner: file,
        bytes: 0,
        limit: before.len(),
        exhaustion: None,
        progress: &mut compressed_progress,
        cancelled: cancelled.clone(),
    };
    let mut input = Counted {
        inner: compressed_guard,
        bytes: compressed.clone(),
    };
    let header = match dtfile::container::read_header(&mut input) {
        Ok(header) => header,
        Err(error) => {
            if let Some(error) = cancelled.borrow_mut().take() {
                return Err(error);
            }
            return Err(error.into());
        }
    };
    if header.ev {
        return Err("EV DT metadata opening unsupported".into());
    }
    let guarded = Guarded {
        inner: dtfile::container::inflate_reader(input)?,
        bytes: 0,
        limit: limits.prefix,
        exhaustion: Some(("inflated prefix", limits.prefix == 256 << 30)),
        progress: &mut inflated_progress,
        cancelled: cancelled.clone(),
    };
    let mut reader = RawReader::from_payload(guarded, header);
    reader.set_read_limits(brace::binary::ReadLimits {
        atom_bytes: limits.retained,
        retained_bytes: limits.retained,
        nodes: limits.nodes,
    });
    let mut spool = Spool::new(memory, limits)?;
    let mut selected = false;
    loop {
        (shared_progress.borrow_mut())("dt-config")?;
        if selected {
            let header = match reader.folder_header() {
                Ok(Some(mut fields)) => {
                    fields.push(brace::binary::BinNode::Atom(brace::binary::Atom::int(1)));
                    fields.push(brace::binary::BinNode::List(vec![
                        brace::binary::BinNode::Atom(brace::binary::Atom::int(0)),
                    ]));
                    fields
                }
                Ok(None) => break,
                Err(error) => {
                    if let Some(error) = cancelled.borrow_mut().take() {
                        return Err(error);
                    }
                    return Err(reader_error(error, limits));
                }
            };
            let (meta, declared) = spool.prepare(&header, reader.offset())?;
            let file = spool.file.get_mut().unwrap_or_else(|e| e.into_inner());
            super::dt_spill::admit(file, &spool.temp_dir, declared)?;
            let mut sink = io::BufWriter::with_capacity(64 << 10, file);
            let copied = reader.copy_folder_binary(&mut sink, declared);
            if let Some(error) = cancelled.borrow_mut().take() {
                return Err(error);
            }
            let copied = copied.map_err(|e| reader_error(e, limits))?;
            sink.flush()?;
            drop(sink);
            if copied != declared {
                return Err("DT CONFIG DATASIZE differs from binary length".into());
            }
            spool.append(meta, copied)?;
            continue;
        }
        let event = match reader.next() {
            Ok(event) => event,
            Err(error) => {
                if let Some(error) = cancelled.borrow_mut().take() {
                    return Err(error);
                }
                return Err(reader_error(error, limits));
            }
        };
        match event {
            Some(RawEvent::TableStart {
                source: Source::Folder(name),
                ..
            }) => {
                selected = name.eq_ignore_ascii_case("CONFIG");
            }
            Some(RawEvent::TableEnd) if selected => break,
            None => {
                return Err(
                    "DT has no complete CONFIG folder (CONFIGSAVE is not a substitute)".into(),
                );
            }
            _ => {}
        }
    }
    drop(reader);
    {
        let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::SpoolIndex);
        spool.finish()?;
    }
    let after = std::fs::metadata(path)?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err("DT input changed during metadata extraction".into());
    }
    Ok(spool)
}
