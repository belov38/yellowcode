use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use brace::binary::{BinNode, ReadLimits};
use dtfile::container::{DtVersion, inflate_reader, read_header};
use dtfile::elements::RawReader;
use dtfile::{DtReader, Event, Source, TableHead};
use ibschema::columns::ColType;
use ibschema::logical::LogicalSchema;
use ibschema::values::ColumnValue;
use onecd::{BlobVal, Database, FieldType, Locking, Numeric, Row, SyncMode, TableDescr, Value};
use sha2::{Digest, Sha256};

use crate::colval::col_type_of;
use crate::physical::{NameCase, descrs};
use crate::raw::FolderFile;
use crate::seed::seed;
use crate::{Error, Infobase, OpenMode, OpenOptions, Result, SchemaCheck, Warning, folder};

mod ibcmd;
mod users;

const SYSTEM: [&str; 14] = [
    "IBVERSION",
    "CONFIG",
    "CONFIGSAVE",
    "PARAMS",
    "FILES",
    "DEPOTFILES",
    "CONFIGCAS",
    "CONFIGCASSAVE",
    "DBSCHEMA",
    "SCHEMASTORAGE",
    "V8USERS",
    "V8USERSMATKEYS",
    "V8USERPWDPLCS",
    "V8CMSDPWDS",
];
const FOLDERS: [&str; 7] = [
    "Config",
    "ConfigSave",
    "Params",
    "Files",
    "DepotFiles",
    "ConfigCAS",
    "ConfigCASSave",
];
const MIB: u64 = 1024 * 1024;
const PAGE: usize = 8192;
const COMPAT: std::ops::RangeInclusive<i64> = 80311..=80501;
const SCHEMA_TEXT_MAX: usize = 64 * MIB as usize;
const TABLES_MAX: usize = 65_536;
const FILES_MAX: u64 = 1_000_000;
const FOLDER_KEYS_MAX: usize = 64 * MIB as usize;
const ROW_NODES: u64 = 1_000_000;
const FOLDER_HEADER_SHARE: u64 = 64 * 1024;
const SCHEMA_NODES_CAP: u64 = 16 << 20;
const ROW_ATOM_CAP: u64 = 64 * MIB;
const ROW_RETAINED_CAP: u64 = 128 * MIB;
const STAGING_PREFIX: &str = ".yellowcode-restore-";
const LOCK_NAME: &str = ".lock";
const DATABASE_NAME: &str = "1Cv8.1CD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreLimits {
    pub cache_bytes: usize,
    pub max_dirty_bytes: usize,
    pub row_bytes: u64,
    pub atom_bytes: u64,
    pub schema_nodes: u64,
}

impl RestoreLimits {
    pub fn from_budget(b: u64) -> RestoreLimits {
        let share = |percent: u64, cap: u64| (b / 100 * percent).min(cap) / MIB * MIB;
        let row = share(10, ROW_RETAINED_CAP);
        RestoreLimits {
            cache_bytes: usize::try_from(share(15, 256 * MIB)).unwrap_or(usize::MAX),
            max_dirty_bytes: usize::try_from(share(20, 512 * MIB)).unwrap_or(usize::MAX),
            row_bytes: row,
            atom_bytes: row.min(ROW_ATOM_CAP),
            schema_nodes: (b / 100 * 25 / 100).min(SCHEMA_NODES_CAP),
        }
    }
}

impl Default for RestoreLimits {
    fn default() -> RestoreLimits {
        RestoreLimits::from_budget(1024 * MIB)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Read,
    Metadata,
    Sync,
    Verify,
    Publish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub source_read: u64,
    pub source_len: u64,
    pub table: String,
    pub rows: u64,
    pub files: u64,
    pub written: u64,
    pub elapsed: std::time::Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreEvent {
    Progress(Progress),
    Note(String),
}

#[derive(Default)]
pub struct RestoreOptions<'a> {
    pub limits: RestoreLimits,
    pub memory_budget: Option<memory_budget::MemoryBudget>,
    pub without_users: bool,
    pub events: Option<&'a mut dyn FnMut(&RestoreEvent)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub source_bytes: u64,
    pub files: u64,
    pub rows: u64,
    pub tables: usize,
    pub users_dropped: u64,
    pub batches: u64,
    pub peak_pending_bytes: usize,
    pub source_sha256: String,
    pub warnings: Vec<OpenWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWarning {
    pub kind: &'static str,
    pub items: usize,
}

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("DT restore: {what}"))
}
fn io<T>(result: io::Result<T>) -> Result<T> {
    result.map_err(|e| Error::Inconsistent(format!("DT restore I/O: {e}")))
}

fn inflate_dbnames(
    data: &[u8],
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<Vec<u8>>> {
    zraw::inflate_budgeted(data, 16 * MIB, budget, memory_budget::Category::Metadata).map_err(
        |error| match error {
            zraw::ManagedInflateError::Memory(error) => Error::Memory(error),
            other => bad(other),
        },
    )
}

struct Tap<R> {
    inner: R,
    read: std::sync::Arc<std::sync::atomic::AtomicU64>,
    hash: std::sync::Arc<std::sync::Mutex<Sha256>>,
}
impl<R: Read> Read for Tap<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let done = buf.get(..n).ok_or_else(|| io::Error::other("read count"))?;
        self.hash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(done);
        self.read
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

fn expect(actual: Option<Event>, expected: Event) -> Result<()> {
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(bad("element order, layout or value"))
    }
}

fn check_row(head: &TableHead, row: &[ColumnValue]) -> Result<()> {
    if row.len() != head.columns.len() {
        return Err(bad("row width"));
    }
    for (c, v) in head.columns.iter().zip(row) {
        c.check(v).map_err(bad)?;
    }
    Ok(())
}

fn system_descriptors() -> Result<Vec<TableDescr>> {
    SYSTEM
        .into_iter()
        .map(|name| {
            seed()
                .tables
                .iter()
                .find(|d| d.name == name)
                .cloned()
                .ok_or_else(|| bad("seed system descriptor"))
        })
        .collect()
}

fn descriptors(
    schema: &LogicalSchema,
    users: &TableDescr,
    configured: bool,
) -> Result<Vec<TableDescr>> {
    let mut all: Vec<TableDescr> = system_descriptors()?
        .into_iter()
        .map(|d| {
            if d.name == "V8USERS" {
                users.clone()
            } else {
                d
            }
        })
        .collect();
    all.extend(descrs(schema, NameCase::Upper)?);
    if configured || ibcmd::matches(schema)? {
        all.extend(ibcmd::extra_tables()?);
    }
    Ok(all)
}

fn named_row(descriptor: &TableDescr, table: &str, values: &[(&str, Value)]) -> Result<Row> {
    descriptor
        .fields
        .iter()
        .map(|f| {
            if f.ty == FieldType::RV {
                return Ok(Value::RowVersion([0; 4]));
            }
            values
                .iter()
                .find(|(n, _)| *n == f.name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| bad(format!("unmapped {table}.{}", f.name)))
        })
        .collect()
}

fn into_onecd(v: ColumnValue, ty: ColType) -> Result<Value> {
    Ok(match (v, ty) {
        (ColumnValue::Null, _) if ty != ColType::RV => Value::Null,
        (ColumnValue::Str(s), ColType::NC(_)) => Value::Char(s),
        (ColumnValue::Str(s), ColType::NVC(_)) => Value::VarChar(s),
        (ColumnValue::Str(s), ColType::NT) => Value::NText(BlobVal::Data(
            s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        )),
        (ColumnValue::Bytes(b), ColType::B(_) | ColType::VB(_)) => Value::Binary(b),
        (ColumnValue::Bytes(b), ColType::I) => Value::Image(BlobVal::Data(b)),
        (ColumnValue::Bool(b), ColType::L) => Value::Bool(b),
        (ColumnValue::Number(n), ColType::N(..)) => Value::Numeric(n),
        (ColumnValue::DateTime(d), ColType::DT) => Value::DateTime(d),
        (v, ty) => return crate::colval::to_onecd(&v, ty),
    })
}

fn value_bytes(v: &ColumnValue) -> usize {
    match v {
        ColumnValue::Str(s) => s.len().saturating_mul(2),
        ColumnValue::Bytes(b) => b.len(),
        _ => 16,
    }
}

struct Plan {
    table: String,
    fields: Vec<Option<(usize, ColType)>>,
    indexes: usize,
}

impl Plan {
    fn new(descriptor: &TableDescr, head: &TableHead) -> Result<Plan> {
        let mut used = HashSet::new();
        let mut fields = Vec::with_capacity(descriptor.fields.len());
        for field in &descriptor.fields {
            if field.ty == FieldType::RV {
                fields.push(None);
                continue;
            }
            let (i, c) = head
                .columns
                .iter()
                .enumerate()
                .find(|(_, c)| c.name.eq_ignore_ascii_case(&field.name))
                .ok_or_else(|| bad("unmapped physical field"))?;
            if !used.insert(i) || col_type_of(field) != Some(c.ty) || field.nullable != c.nullable {
                return Err(bad("physical column mapping"));
            }
            fields.push(Some((i, c.ty)));
        }
        if used.len() != head.columns.len() {
            return Err(bad("unused transfer column"));
        }
        Ok(Plan {
            table: descriptor.name.clone(),
            fields,
            indexes: descriptor.indexes.len(),
        })
    }

    fn estimate(&self, row: &[ColumnValue]) -> usize {
        row.iter()
            .map(value_bytes)
            .fold(0usize, usize::saturating_add)
            .saturating_add(self.indexes.saturating_add(2).saturating_mul(4 * PAGE))
    }

    fn row(&self, row: Vec<ColumnValue>) -> Result<Row> {
        let mut cells: Vec<Option<ColumnValue>> = row.into_iter().map(Some).collect();
        self.fields
            .iter()
            .map(|f| match f {
                None => Ok(Value::RowVersion([0; 4])),
                Some((usize::MAX, _)) => Ok(Value::Null),
                Some((i, ty)) => into_onecd(
                    cells
                        .get_mut(*i)
                        .and_then(Option::take)
                        .ok_or_else(|| bad("row width"))?,
                    *ty,
                ),
            })
            .collect()
    }
}

impl Plan {
    fn with_absent(descriptor: &TableDescr, head: &TableHead, absent: &[&str]) -> Result<Plan> {
        let present = TableDescr {
            fields: descriptor
                .fields
                .iter()
                .filter(|f| !absent.contains(&f.name.as_str()))
                .cloned()
                .collect(),
            ..descriptor.clone()
        };
        let partial = Plan::new(&present, head)?;
        let mut mapped = partial.fields.into_iter();
        let mut fields = Vec::with_capacity(descriptor.fields.len());
        for f in &descriptor.fields {
            if absent.contains(&f.name.as_str()) {
                if !f.nullable {
                    return Err(bad("absent field must be nullable"));
                }
                fields.push(Some((usize::MAX, ColType::NVC(0))));
            } else {
                fields.push(mapped.next().ok_or_else(|| bad("field mapping"))?);
            }
        }
        Ok(Plan {
            table: descriptor.name.clone(),
            fields,
            indexes: descriptor.indexes.len(),
        })
    }
}

fn users_as_is(head: &TableHead, row: &[ColumnValue]) -> bool {
    ["EAUTH", "ADMROLE", "USSPRH"].iter().all(|name| {
        head.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .and_then(|i| row.get(i))
            .is_some_and(|v| *v != ColumnValue::Null)
    })
}

fn separated_users(seed: &TableDescr) -> TableDescr {
    let index = |name: &str, unique: bool, fields: &[(&str, u32)]| onecd::IndexDescr {
        name: name.into(),
        unique,
        fields: fields
            .iter()
            .map(|(f, n)| onecd::IndexField {
                name: (*f).into(),
                prefix_len: *n,
            })
            .collect(),
    };
    TableDescr {
        indexes: vec![
            index("IDIDX", true, &[("ID", 16)]),
            index("BYUSNAME", false, &[("USSPRH", 0), ("NAME", 64)]),
            index("BYNAME", false, &[("NAME", 64)]),
            index("BYUSDESCR", false, &[("USSPRH", 0), ("DESCR", 128)]),
            index("BYUSOSNAME", false, &[("USSPRH", 0), ("OSNAME", 128)]),
            index("BYOSNAME", false, &[("OSNAME", 128)]),
            index("BYROLESID", false, &[("ROLESID", 0)]),
            index("BYSHOW", false, &[("SHOW", 0)]),
            index("BYEAUTH", false, &[("ADMROLE", 0), ("EAUTH", 0)]),
            index("BYUSEMAIL_V8USERS", false, &[("USSPRH", 0), ("EMAIL", 128)]),
            index("BYEMAIL_V8USERS", false, &[("EMAIL", 128)]),
        ],
        ..seed.clone()
    }
}

fn users_separated(database: &Path, budget: Option<&memory_budget::MemoryBudget>) -> Result<bool> {
    let cfg = match budget {
        Some(budget) => crate::open::configuration_in_budget(database, budget)?,
        None => {
            let raw = crate::raw::RawIb::open_with(
                database,
                onecd::OpenOptions {
                    locking: Locking::None,
                    ..onecd::OpenOptions::read_only()
                },
            )?;
            crate::config::load_configuration_profile(&raw, "CONFIG", mdconf::LoadProfile::Query)?
        }
    };
    let Some(cfg) = cfg else {
        return Ok(false);
    };
    Ok(cfg
        .objects
        .iter()
        .filter(|o| o.kind == mdconf::MdKind::CommonAttribute)
        .any(|o| {
            o.props.iter().any(|p| {
                matches!(
                    p.name.as_str(),
                    "UsersSeparation" | "AuthenticationSeparation"
                ) && matches!(&p.value, mdconf::Value::Text(v) if v == "Separate")
            })
        }))
}

fn row_limit(error: Error, limits: &RestoreLimits) -> Error {
    let Error::Dt(dtfile::Error::Limit { what, .. }) = &error else {
        return error;
    };
    let limit = match *what {
        "atom bytes" if limits.atom_bytes < ROW_ATOM_CAP => limits.atom_bytes,
        "retained bytes" if limits.row_bytes < ROW_RETAINED_CAP => limits.row_bytes,
        _ => return error,
    };
    bad(format!(
        "a DT row or file is over the row share of the memory budget ({what} {} MiB); \
         increase --memory-limit",
        limit / MIB
    ))
}

fn file_estimate(len: usize) -> usize {
    len.saturating_add(
        folder::part_count(len)
            .saturating_mul(3)
            .saturating_mul(4 * PAGE),
    )
}

struct Batches<'db> {
    db: &'db Database,
    tx: Option<onecd::WriteTxn<'db>>,
    limit: usize,
    commits: u64,
    peak: usize,
}

impl<'db> Batches<'db> {
    fn new(db: &'db Database, limits: &RestoreLimits) -> Batches<'db> {
        Batches {
            db,
            tx: None,
            limit: limits.max_dirty_bytes,
            commits: 0,
            peak: 0,
        }
    }

    fn unit(&mut self, estimate: usize, what: &str) -> Result<&mut onecd::WriteTxn<'db>> {
        if estimate > self.limit / 4 * 3 {
            return Err(bad(format!(
                "{what} needs about {} MiB of the batch limit {} MiB; increase --memory-limit",
                estimate as u64 / MIB,
                self.limit as u64 / MIB
            )));
        }
        let pending = self.tx.as_ref().map_or(0, onecd::WriteTxn::pending_bytes);
        if self.tx.is_some() && pending.saturating_add(estimate) > self.limit / 2 {
            self.commit()?;
        }
        if self.tx.is_none() {
            self.tx = Some(self.db.write()?);
        }
        self.tx.as_mut().ok_or_else(|| bad("batch transaction"))
    }

    fn commit(&mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            self.peak = self.peak.max(tx.pending_bytes());
            #[cfg(test)]
            inject("batch")?;
            tx.commit()?;
            self.commits += 1;
        }
        Ok(())
    }
}

struct FolderSink<'a, 'db> {
    batches: &'a mut Batches<'db>,
    table: &'a str,
    file: &'a FolderFile,
    buf: Vec<u8>,
    part_capacity: usize,
    capacity_limit: usize,
    part: u64,
    error: Option<Error>,
}

impl FolderSink<'_, '_> {
    fn flush_part(&mut self) -> Result<()> {
        let bytes = std::mem::take(&mut self.buf);
        let tx = self.batches.unit(
            file_estimate(bytes.len()),
            &format!("file {} part", self.file.name),
        )?;
        folder::insert_new_part(tx, self.table, self.file, self.part, bytes)?;
        self.part = self
            .part
            .checked_add(1)
            .ok_or_else(|| bad("folder part overflow"))?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if !self.buf.is_empty() || self.part == 0 {
            self.flush_part()?;
        }
        Ok(())
    }
}

impl Write for FolderSink<'_, '_> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let original = input.len();
        while !input.is_empty() {
            if self.buf.capacity() == 0 {
                self.buf
                    .try_reserve_exact(self.part_capacity)
                    .map_err(|_| io::Error::other("folder part allocation failed"))?;
                if self.buf.capacity() > self.capacity_limit {
                    return Err(io::Error::other("folder part exceeds memory share"));
                }
            }
            let take = (folder::PART_SIZE - self.buf.len()).min(input.len());
            let (chunk, rest) = input
                .split_at_checked(take)
                .ok_or_else(|| io::Error::other("invalid folder part boundary"))?;
            self.buf.extend_from_slice(chunk);
            input = rest;
            if self.buf.len() == folder::PART_SIZE
                && let Err(error) = self.flush_part()
            {
                self.error = Some(error);
                return Err(io::Error::other("cannot write folder part"));
            }
        }
        Ok(original)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    static FAIL_AFTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn database_options(limits: &RestoreLimits) -> onecd::OpenOptions {
    onecd::OpenOptions {
        locking: Locking::None,
        cache_bytes: limits.cache_bytes,
        max_dirty_bytes: limits.max_dirty_bytes,
        max_retained_bytes: 0,
        sync: SyncMode::Off,
        ..onecd::OpenOptions::read_write()
    }
}

pub fn restore_dt_new_file(source: &Path, target: &Path) -> Result<RestoreReport> {
    restore_dt_new_file_with(source, target, &mut RestoreOptions::default())
}

pub fn restore_dt_new_file_with(
    source: &Path,
    target: &Path,
    opts: &mut RestoreOptions<'_>,
) -> Result<RestoreReport> {
    restore(source, target, opts, || Ok(()))
}

struct Notes<'a, 'b> {
    events: &'a mut Option<&'b mut dyn FnMut(&RestoreEvent)>,
}
impl Notes<'_, '_> {
    fn send(&mut self, event: RestoreEvent) {
        if let Some(f) = self.events.as_deref_mut() {
            f(&event);
        }
    }
}

fn sweep_staging(parent: &Path, notes: &mut Notes<'_, '_>) -> Result<()> {
    let entries = match fs::read_dir(parent) {
        Ok(e) => e,
        Err(e) => return io(Err(e)),
    };
    for entry in entries {
        let entry = io(entry)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(STAGING_PREFIX) || !io(entry.file_type())?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let lock = match File::options()
            .read(true)
            .write(true)
            .open(dir.join(LOCK_NAME))
        {
            Ok(f) => f,
            Err(_) => {
                notes.send(RestoreEvent::Note(format!(
                    "staging directory {name} has no lock file; left in place"
                )));
                continue;
            }
        };
        match lock.try_lock() {
            Ok(()) => {
                drop(lock);
                let size = dir_size(&dir);
                match fs::remove_dir_all(&dir) {
                    Ok(()) => notes.send(RestoreEvent::Note(format!(
                        "removed abandoned staging directory {name} ({} MB)",
                        size / 1_000_000
                    ))),
                    Err(e) => notes.send(RestoreEvent::Note(format!(
                        "abandoned staging directory {name} could not be removed: {e}"
                    ))),
                }
            }
            Err(fs::TryLockError::WouldBlock) => notes.send(RestoreEvent::Note(format!(
                "staging directory {name} belongs to a restore in progress; left in place"
            ))),
            Err(fs::TryLockError::Error(e)) => notes.send(RestoreEvent::Note(format!(
                "staging directory {name}: lock check failed ({e}); left in place"
            ))),
        }
    }
    Ok(())
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn transient(e: &io::Error) -> bool {
    #[cfg(test)]
    if e.kind() == io::ErrorKind::ResourceBusy {
        return true;
    }
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32))
}

#[cfg(windows)]
fn publication_leaf(path: &Path) -> Result<&std::ffi::OsStr> {
    use std::os::windows::ffi::OsStrExt;

    let name = path
        .file_name()
        .ok_or_else(|| bad("publication requires a file name"))?;
    if matches!(name.encode_wide().last(), Some(0x20 | 0x2e)) {
        return Err(bad(
            "Windows output file name must not end in a trailing dot or space",
        ));
    }
    Ok(name)
}

#[cfg(windows)]
fn publication_path(path: &Path) -> Result<PathBuf> {
    let name = publication_leaf(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(io(fs::canonicalize(parent))?.join(name))
}

fn persist(from: &Path, to: &Path) -> Result<()> {
    #[cfg(windows)]
    let (from, to) = (publication_path(from)?, publication_path(to)?);
    #[cfg(windows)]
    let (from, to) = (from.as_path(), to.as_path());
    let mut path = io(tempfile::TempPath::try_from_path(from))?;
    let mut pause = std::time::Duration::from_millis(100);
    for attempt in 0.. {
        #[cfg(test)]
        if let Some(e) = PERSIST_FAULT.with(|f| f.borrow_mut().pop()) {
            if transient(&e) && attempt < 9 {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(3200));
                continue;
            }
            let _ = path.keep();
            return Err(bad(format!("publication: {e}")));
        }
        match path.persist_noclobber(to) {
            Ok(()) => return Ok(()),
            Err(e) if transient(&e.error) && attempt < 9 => {
                path = e.path;
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(3200));
            }
            Err(e) => {
                let _ = e.path.keep();
                return Err(bad(format!("publication: {}", e.error)));
            }
        }
    }
    Err(bad("publication retries"))
}

#[cfg(test)]
thread_local! {
    static PERSIST_FAULT: std::cell::RefCell<Vec<io::Error>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn probe_publication(staging: &Path, parent: &Path) -> Result<()> {
    let probe = staging.join("probe");
    io(File::create_new(&probe))?;
    let name = format!(".yellowcode-probe-{:016x}", rand::random::<u64>());
    let published = parent.join(name);
    #[cfg(test)]
    inject("probe")?;
    persist(&probe, &published).map_err(|e| {
        bad(format!(
            "the target directory does not support publication without replacement ({e})"
        ))
    })?;
    io(fs::remove_file(&published))?;
    sync_dir(parent);
    Ok(())
}

struct Expected {
    schema: LogicalSchema,
    compat: i64,
    counts: Vec<u64>,
    files: u64,
    rows: u64,
    dbnames: memory_budget::Charged<Vec<u8>>,
    users_dropped: u64,
    matching_keys: u64,
    users_descr: TableDescr,
}

struct Run<'o, 'e> {
    opts: &'o RestoreOptions<'e>,
    started: Instant,
    source_len: u64,
    read: std::sync::Arc<std::sync::atomic::AtomicU64>,
    database: PathBuf,
    rows: u64,
    files: u64,
    table: String,
    last_progress: u64,
}

impl Run<'_, '_> {
    fn progress(&mut self, notes: &mut Notes<'_, '_>, phase: Phase) {
        let written = fs::metadata(&self.database).map_or(0, |m| m.len());
        notes.send(RestoreEvent::Progress(Progress {
            phase,
            source_read: self.read.load(std::sync::atomic::Ordering::Relaxed),
            source_len: self.source_len,
            table: self.table.clone(),
            rows: self.rows,
            files: self.files,
            written,
            elapsed: self.started.elapsed(),
        }));
    }
    fn tick(&mut self, notes: &mut Notes<'_, '_>) {
        let n = self.rows + self.files;
        if n >= self.last_progress + 65_536 {
            self.last_progress = n;
            self.progress(notes, Phase::Read);
        }
    }
}

fn restore(
    source: &Path,
    target: &Path,
    opts: &mut RestoreOptions<'_>,
    before_publish: impl FnOnce() -> Result<()>,
) -> Result<RestoreReport> {
    #[cfg(windows)]
    publication_leaf(target)?;
    match fs::symlink_metadata(target) {
        Ok(_) => return Err(bad("target already exists")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(bad(e)),
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut events = opts.events.take();
    let mut notes = Notes {
        events: &mut events,
    };
    let result = run(source, target, &parent, opts, &mut notes, before_publish);
    opts.events = events;
    result
}

fn parse_locale(bytes: &[u8], parse_share: u64) -> Result<&'static str> {
    let budget = memory_budget::MemoryBudget::new(memory_budget::Limits::new(parse_share));
    let node = brace::parse_bytes_budgeted(bytes, &budget, memory_budget::Category::Metadata)
        .map_err(|error| match error {
            brace::ManagedParseError::Memory(_) => {
                bad("locale.inf parse exceeds memory share; increase --memory-limit")
            }
            other => bad(other),
        })?;
    match node
        .as_list()
        .and_then(|v| v.first())
        .and_then(brace::Node::as_str)
    {
        Some("ru_RU") => Ok("ru_RU"),
        Some("en_US") => Ok("en_US"),
        _ => Err(bad("unsupported locale.inf locale")),
    }
}

fn probe_locale(
    source: &Path,
    limits: &RestoreLimits,
    without_users: bool,
    file_limit: u64,
) -> Result<&'static str> {
    let mut input = BufReader::with_capacity(1 << 20, io(File::open(source))?);
    let header = read_header(&mut input)?;
    if header.version != DtVersion::F3 || header.ev {
        return Err(bad("only ordinary F3"));
    }
    let row_limits = ReadLimits {
        atom_bytes: limits.atom_bytes,
        retained_bytes: limits.row_bytes,
        nodes: ROW_NODES,
    };
    let mut raw = RawReader::from_payload(inflate_reader(input)?, header);
    raw.set_read_limits(row_limits);
    let mut reader = DtReader::from_raw(raw);
    match reader.next_event()? {
        Some(Event::CompatibilityMode(mode)) if COMPAT.contains(&mode) => {}
        _ => return Err(bad("compatibility mode outside 8.3.11-8.5.1")),
    }
    for name in ["IndexCompatibility", "UseMVCC"] {
        expect(
            reader.next_event()?,
            Event::CustomFlag {
                name: name.into(),
                value: BinNode::Atom(brace::binary::Atom::int(1)),
            },
        )?;
    }
    let header_share = (limits.row_bytes / 4).min(FOLDER_HEADER_SHARE);
    let mut files = 0u64;
    for folder_name in FOLDERS.iter().take(3) {
        let head = TableHead::system(&Source::Folder((*folder_name).into()))
            .ok_or_else(|| bad("system head"))?;
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        reader.set_read_limits(ReadLimits {
            atom_bytes: row_limits.atom_bytes.min(header_share),
            retained_bytes: header_share,
            nodes: row_limits.nodes,
        });
        while let Some(metadata) = reader.folder_row_header()? {
            if metadata.len() != 5 {
                return Err(bad("folder transfer columns"));
            }
            for (column, value) in head.columns.iter().take(5).zip(&metadata) {
                column.check(value).map_err(bad)?;
            }
            let [ColumnValue::Str(name), _, _, _, ColumnValue::Number(size)] = metadata.as_slice()
            else {
                return Err(bad("folder transfer columns"));
            };
            if !(without_users
                && *folder_name == "Params"
                && name.eq_ignore_ascii_case("users.usr"))
            {
                files = files
                    .checked_add(1)
                    .ok_or_else(|| bad("folder file limit"))?;
                if files > file_limit {
                    return Err(bad("folder file limit"));
                }
            }
            let length: u64 = size
                .to_string()
                .parse()
                .map_err(|_| bad("invalid folder DATASIZE"))?;
            if *folder_name == "Params" && name.eq_ignore_ascii_case("locale.inf") {
                if length.saturating_add(header_share) > limits.row_bytes {
                    return Err(bad(
                        "folder file exceeds the row share; increase --memory-limit",
                    ));
                }
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(
                        usize::try_from(length).map_err(|_| bad("folder file size overflow"))?,
                    )
                    .map_err(|_| bad("folder file allocation failed"))?;
                if (bytes.capacity() as u64).saturating_add(header_share) > limits.row_bytes {
                    return Err(bad(
                        "folder file exceeds the row share; increase --memory-limit",
                    ));
                }
                if reader.copy_folder_binary_with_work_limit(&mut bytes, length, ROW_NODES)?
                    != length
                {
                    return Err(bad("folder DATASIZE differs from stored bytes"));
                }
                return parse_locale(
                    &bytes,
                    limits.row_bytes - header_share - bytes.capacity() as u64,
                );
            }
            if reader.copy_folder_binary_with_work_limit(&mut io::sink(), length, ROW_NODES)?
                != length
            {
                return Err(bad("folder DATASIZE differs from stored bytes"));
            }
        }
        reader.set_read_limits(row_limits);
    }
    Err(bad("missing locale.inf"))
}

fn run(
    source: &Path,
    target: &Path,
    parent: &Path,
    opts: &RestoreOptions<'_>,
    notes: &mut Notes<'_, '_>,
    before_publish: impl FnOnce() -> Result<()>,
) -> Result<RestoreReport> {
    let started = Instant::now();
    sweep_staging(parent, notes)?;
    let staging = io(tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent))?;
    let lock = io(File::create_new(staging.path().join(LOCK_NAME)))?;
    lock.try_lock()
        .map_err(|e| bad(format!("staging lock: {e:?}")))?;
    probe_publication(staging.path(), parent)?;

    let before = io(fs::metadata(source))?;
    let stamp = |m: &fs::Metadata| (m.len(), m.modified().ok());
    let locale = probe_locale(source, &opts.limits, opts.without_users, FILES_MAX)
        .map_err(|e| row_limit(e, &opts.limits))?;
    if stamp(&before) != stamp(&io(fs::metadata(source))?) {
        return Err(bad("the source changed while it was read"));
    }
    let read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let hash = std::sync::Arc::new(std::sync::Mutex::new(Sha256::new()));
    let mut input = Tap {
        inner: BufReader::with_capacity(1 << 20, io(File::open(source))?),
        read: read.clone(),
        hash: hash.clone(),
    };
    let header = read_header(&mut input)?;
    if header.version != DtVersion::F3 || header.ev {
        return Err(bad("only ordinary F3"));
    }
    let mut raw = RawReader::from_payload(inflate_reader(input)?, header);
    let row_limits = ReadLimits {
        atom_bytes: opts.limits.atom_bytes,
        retained_bytes: opts.limits.row_bytes,
        nodes: ROW_NODES,
    };
    raw.set_read_limits(row_limits);
    let mut reader = DtReader::from_raw(raw);
    let database = staging.path().join(DATABASE_NAME);
    let mut state = Run {
        opts,
        started,
        source_len: before.len(),
        read,
        database: database.clone(),
        rows: 0,
        files: 0,
        table: String::new(),
        last_progress: 0,
    };
    let (expected, commits, peak) = stream(&mut reader, &mut state, notes, row_limits, locale)
        .map_err(|e| row_limit(e, &opts.limits))?;
    drop(reader);

    let after = io(fs::metadata(source))?;
    if stamp(&before) != stamp(&after) {
        return Err(bad("the source changed while it was read"));
    }
    let sha = hash
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .finalize();
    let source_sha256: String = sha.iter().map(|b| format!("{b:02x}")).collect();

    state.progress(notes, Phase::Sync);
    #[cfg(test)]
    inject("sync")?;
    io(io(File::options().write(true).open(&database))?.sync_all())?;
    state.progress(notes, Phase::Verify);
    #[cfg(test)]
    inject("verify")?;
    let warnings = final_checks(&database, &expected, opts)?;
    before_publish()?;
    state.progress(notes, Phase::Publish);
    #[cfg(test)]
    inject("publish")?;
    persist(&database, target)?;
    sync_dir(parent);
    drop(lock);
    Ok(RestoreReport {
        source_bytes: before.len(),
        files: expected.files,
        rows: expected.rows,
        tables: expected.counts.len() - FOLDERS.len() - 3,
        users_dropped: expected.users_dropped,
        batches: commits,
        peak_pending_bytes: peak,
        source_sha256,
        warnings,
    })
}

fn stream<R: Read>(
    reader: &mut DtReader<R>,
    run: &mut Run<'_, '_>,
    notes: &mut Notes<'_, '_>,
    row_limits: ReadLimits,
    locale: &str,
) -> Result<(Expected, u64, usize)> {
    let limits = run.opts.limits;
    let compat = match reader.next_event()? {
        Some(Event::CompatibilityMode(mode)) if COMPAT.contains(&mode) => mode,
        _ => return Err(bad("compatibility mode outside 8.3.11-8.5.1")),
    };
    for name in ["IndexCompatibility", "UseMVCC"] {
        expect(
            reader.next_event()?,
            Event::CustomFlag {
                name: name.into(),
                value: BinNode::Atom(brace::binary::Atom::int(1)),
            },
        )?;
    }
    let mut db = Database::create(
        &run.database,
        onecd::CreateOptions {
            format: onecd::Format::V838,
            page_size: PAGE as u32,
            locale: locale.into(),
            open: database_options(&limits),
        },
    )?;
    let mut batches = Batches::new(&db, &limits);
    let mut commits = 0u64;
    let mut peak = 0usize;
    {
        let tx = batches.unit(0, "system tables")?;
        for d in system_descriptors()?
            .into_iter()
            .filter(|d| d.name != "V8USERS")
        {
            tx.create_table(&d)?;
        }
        let ibversion = tx.read_table("IBVERSION")?.descr().clone();
        let row = named_row(
            &ibversion,
            "IBVERSION",
            &[
                (
                    "IBVERSION",
                    Value::Numeric(Numeric::from_i64(i64::from(seed().ibversion.0))),
                ),
                (
                    "PLATFORMVERSIONREQ",
                    Value::Numeric(Numeric::from_i64(i64::from(seed().ibversion.1))),
                ),
            ],
        )?;
        tx.table("IBVERSION")?.insert(&row)?;
    }
    batches.commit()?;

    let mut counts = Vec::new();
    let mut locale_seen = false;
    let mut dbnames = None;
    let dbnames_budget =
        run.opts.memory_budget.clone().unwrap_or_else(|| {
            memory_budget::MemoryBudget::new(memory_budget::Limits::new(34 * MIB))
        });
    let mut users_dropped = 0u64;
    let mut key_bytes = 0usize;
    let collate = onecd::IcuCollate;
    for folder_name in FOLDERS {
        let head = TableHead::system(&Source::Folder(folder_name.into()))
            .ok_or_else(|| bad("system head"))?;
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        run.table = head.phys_name.clone();
        run.progress(notes, Phase::Read);
        let filename = system_descriptors()?
            .into_iter()
            .find(|d| d.name == head.phys_name)
            .and_then(|d| d.field("FILENAME").map(|(_, f)| f.clone()))
            .ok_or_else(|| bad("folder FILENAME"))?;
        let mut keys = HashSet::new();
        let mut count = 0;
        let header_share = (limits.row_bytes / 4).min(FOLDER_HEADER_SHARE);
        reader.set_read_limits(ReadLimits {
            atom_bytes: row_limits.atom_bytes.min(header_share),
            retained_bytes: header_share,
            nodes: row_limits.nodes,
        });
        while let Some(metadata) = reader.folder_row_header()? {
            if metadata.len() != 5 {
                return Err(bad("folder transfer columns"));
            }
            for (column, value) in head.columns.iter().take(5).zip(&metadata) {
                column.check(value).map_err(bad)?;
            }
            if !matches!(
                head.phys_name.as_str(),
                "CONFIG" | "PARAMS" | "FILES" | "DEPOTFILES" | "CONFIGCAS"
            ) {
                return Err(bad("nonempty pending configuration folder"));
            }
            let [
                ColumnValue::Str(name),
                ColumnValue::DateTime(creation),
                ColumnValue::DateTime(modified),
                ColumnValue::Number(attributes),
                ColumnValue::Number(size),
            ] = metadata.as_slice()
            else {
                return Err(bad("folder transfer columns"));
            };
            let length: u64 = size
                .to_string()
                .parse()
                .map_err(|_| bad("invalid folder DATASIZE"))?;
            let f = FolderFile {
                name: name.clone(),
                creation: *creation,
                modified: *modified,
                attributes: attributes.clone(),
                data_size: size.clone(),
                data: Vec::new(),
            };
            let key = onecd::collate::field_string_key(&collate, locale, &filename, &f.name)?;
            key_bytes = key_bytes.saturating_add(key.len());
            if key_bytes > FOLDER_KEYS_MAX {
                return Err(bad("folder name key limit"));
            }
            if !keys.insert(key) {
                return Err(bad("duplicate collated folder identity"));
            }
            let locale_file =
                head.phys_name == "PARAMS" && f.name.eq_ignore_ascii_case("locale.inf");
            let dbnames_file = head.phys_name == "PARAMS" && f.name.eq_ignore_ascii_case("DBNames");
            let drop_user_file = run.opts.without_users
                && head.phys_name == "PARAMS"
                && f.name.eq_ignore_ascii_case("users.usr");
            if drop_user_file {
                if reader.copy_folder_binary_with_work_limit(&mut io::sink(), length, ROW_NODES)?
                    != length
                {
                    return Err(bad("folder DATASIZE differs from stored bytes"));
                }
                continue;
            }
            run.files += 1;
            if run.files > FILES_MAX {
                return Err(bad("folder file limit"));
            }
            let special = locale_file || dbnames_file;
            let part_size = length.min(folder::PART_SIZE as u64);
            let needed = header_share.saturating_add(if special { length } else { part_size });
            let part_limit = if special {
                limits.row_bytes
            } else {
                limits.row_bytes - header_share
            };
            if needed > limits.row_bytes || part_size > part_limit {
                return Err(bad(format!(
                    "folder {} needs {needed} row bytes, limit {}; increase --memory-limit",
                    f.name, limits.row_bytes
                )));
            }
            let part_capacity =
                usize::try_from(part_size).map_err(|_| bad("folder part size overflow"))?;
            let capacity_limit = usize::try_from(part_limit).unwrap_or(usize::MAX);
            let mut sink = FolderSink {
                batches: &mut batches,
                table: &head.phys_name,
                file: &f,
                buf: Vec::new(),
                part_capacity,
                capacity_limit,
                part: 0,
                error: None,
            };
            let copied = if special {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(
                        usize::try_from(length).map_err(|_| bad("folder file size overflow"))?,
                    )
                    .map_err(|_| bad("folder file allocation failed"))?;
                if (bytes.capacity() as u64).saturating_add(header_share) > limits.row_bytes {
                    return Err(bad(
                        "folder file exceeds the row share; increase --memory-limit",
                    ));
                }
                let copied =
                    reader.copy_folder_binary_with_work_limit(&mut bytes, length, ROW_NODES)?;
                if locale_file {
                    let parse_share = limits.row_bytes - header_share - bytes.capacity() as u64;
                    if parse_locale(&bytes, parse_share)? != locale {
                        return Err(bad("locale.inf changed between passes"));
                    }
                    if locale_seen {
                        return Err(bad("duplicate locale.inf"));
                    }
                    locale_seen = true;
                }
                if dbnames_file {
                    if dbnames.is_some() {
                        return Err(bad("duplicate DBNames"));
                    }
                    dbnames = Some(inflate_dbnames(&bytes, &dbnames_budget)?);
                }
                if let Err(error) = sink.write_all(&bytes) {
                    return Err(sink.error.take().unwrap_or_else(|| bad(error)));
                }
                copied
            } else {
                let result =
                    reader.copy_folder_binary_with_work_limit(&mut sink, length, ROW_NODES);
                if let Some(error) = sink.error.take() {
                    return Err(error);
                }
                result?
            };
            if copied != length {
                return Err(bad("folder DATASIZE differs from stored bytes"));
            }
            sink.finish()?;
            count += 1;
            run.tick(notes);
        }
        reader.set_read_limits(row_limits);
        counts.push(count);
    }
    if !locale_seen {
        return Err(bad("missing locale.inf"));
    }
    let users = match reader.next_event()? {
        Some(Event::TableStart(h)) if h.source == Source::Users || h.source == Source::UsersSpr => {
            h
        }
        _ => return Err(bad("element order, layout or value")),
    };
    let expected_users = TableHead::system(&users.source).ok_or_else(|| bad("system head"))?;
    if users != expected_users {
        return Err(bad("element order, layout or value"));
    }
    run.table = "V8USERS".into();
    run.progress(notes, Phase::Read);
    let prepared = users::prepare(reader, &users, run)?;
    users_dropped += prepared.dropped;
    let seed_users = system_descriptors()?
        .into_iter()
        .find(|d| d.name == "V8USERS")
        .ok_or_else(|| bad("seed V8USERS"))?;
    let mut users_descr = seed_users.clone();
    let mut facts = users::Facts::without_configuration();
    if counts.first().is_some_and(|&n| n != 0) {
        batches.commit()?;
        commits += batches.commits;
        peak = peak.max(batches.peak);
        drop(batches);
        drop(db);
        let separated = if prepared.rebuild {
            facts = users::facts(&run.database, run.opts.memory_budget.as_ref())?;
            facts.separated
        } else {
            users_separated(&run.database, run.opts.memory_budget.as_ref())?
        };
        db = Database::open(&run.database, database_options(&limits))?;
        batches = Batches::new(&db, &limits);
        if separated {
            users_descr = separated_users(&seed_users);
        }
    }
    batches
        .unit(4 * PAGE, "V8USERS")?
        .create_table(&users_descr)?;
    let (stored_users, matching_keys) =
        prepared.write(&users, &users_descr, &mut batches, &facts, run.opts)?;
    drop(facts);
    counts.push(stored_users);
    for (source, table) in [
        (Source::PasswordPolicies, "V8USERPWDPLCS"),
        (Source::CompromisedPasswords, "V8CMSDPWDS"),
    ] {
        let head = TableHead::system(&source).ok_or_else(|| bad("system head"))?;
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        run.table = table.into();
        run.progress(notes, Phase::Read);
        let descriptor = system_descriptors()?
            .into_iter()
            .find(|d| d.name == table)
            .ok_or_else(|| bad("seed policy table"))?;
        let plan = Plan::new(&descriptor, &head)?;
        let mut count = 0u64;
        loop {
            match reader.next_event()? {
                Some(Event::TableEnd) => break,
                Some(Event::Row(row)) => {
                    check_row(&head, &row)?;
                    let tx = batches.unit(plan.estimate(&row), table)?;
                    let row = plan.row(row)?;
                    tx.table(table)?.insert(&row)?;
                    count += 1;
                }
                _ => return Err(bad("missing system table end")),
            }
        }
        counts.push(count);
    }

    reader.set_read_limits(ReadLimits {
        atom_bytes: SCHEMA_TEXT_MAX as u64,
        retained_bytes: SCHEMA_TEXT_MAX as u64,
        nodes: limits.schema_nodes,
    });
    let schema = match reader.next_event() {
        Ok(Some(Event::DataBaseStart(schema))) => schema,
        Ok(_) => return Err(bad("expected one DataBase")),
        Err(dtfile::Error::Limit { what: "nodes", .. }) => {
            return Err(bad(if limits.schema_nodes < SCHEMA_NODES_CAP {
                format!(
                    "schema node limit {} (a share of the memory budget); increase --memory-limit",
                    limits.schema_nodes
                )
            } else {
                format!("schema node limit {}", limits.schema_nodes)
            }));
        }
        Err(dtfile::Error::Limit { .. }) => return Err(bad("schema byte limit")),
        Err(e) => return Err(e.into()),
    };
    reader.set_read_limits(row_limits);
    let text = schema.to_text();
    if text.len() > SCHEMA_TEXT_MAX {
        return Err(bad("schema byte limit"));
    }
    if schema.walk().len() > TABLES_MAX {
        return Err(bad("schema table limit"));
    }
    let heads = TableHead::database(&schema)?;
    let mut logical = descrs(&schema, NameCase::Upper)?;
    let mut names: HashSet<String> = SYSTEM.into_iter().map(str::to_owned).collect();
    for d in &logical {
        if d.name.to_ascii_uppercase().contains("BINARYDATA")
            || !names.insert(d.name.to_uppercase())
        {
            return Err(bad("unsupported or duplicate physical table"));
        }
    }
    if counts.first().is_some_and(|&n| n != 0) || ibcmd::matches(&schema)? {
        for descriptor in ibcmd::extra_tables()? {
            if !names.insert(descriptor.name.to_uppercase()) {
                return Err(bad("unsupported or duplicate physical table"));
            }
            logical.push(descriptor);
        }
    }
    batches.commit()?;
    for d in &logical {
        let tx = batches.unit(4 * PAGE, "table description")?;
        tx.create_table(d)?;
    }
    {
        let tx = batches.unit(text.len().saturating_mul(3), "schema")?;
        let dbschema = tx.read_table("DBSCHEMA")?.descr().clone();
        let row = named_row(
            &dbschema,
            "DBSCHEMA",
            &[("SERIALIZEDDATA", Value::Image(BlobVal::Data(text.clone())))],
        )?;
        tx.table("DBSCHEMA")?.insert(&row)?;
        let storage = tx.read_table("SCHEMASTORAGE")?.descr().clone();
        let row = named_row(
            &storage,
            "SCHEMASTORAGE",
            &[
                (
                    "SCHEMAID",
                    Value::Numeric(Numeric::from_i64(i64::from(seed().schemastorage.0))),
                ),
                (
                    "STATUS",
                    Value::Numeric(Numeric::from_i64(i64::from(seed().schemastorage.1))),
                ),
                ("CURRENTSCHEMA", Value::Image(BlobVal::Data(text))),
                (
                    "NEWGENCREATED",
                    Value::Image(BlobVal::Data(seed().newgen.clone())),
                ),
                (
                    "NEWGENDROPPED",
                    Value::Image(BlobVal::Data(seed().newgen.clone())),
                ),
            ],
        )?;
        tx.table("SCHEMASTORAGE")?.insert(&row)?;
    }
    batches.commit()?;
    commits += batches.commits;
    peak = peak.max(batches.peak);
    drop(batches);
    drop(db);
    let dbnames = dbnames.ok_or_else(|| bad("missing DBNames"))?;
    let partial = Expected {
        schema,
        compat,
        counts,
        files: run.files,
        rows: 0,
        dbnames,
        users_dropped,
        matching_keys,
        users_descr,
    };
    run.table.clear();
    run.progress(notes, Phase::Metadata);
    #[cfg(test)]
    inject("metadata")?;
    check_metadata(&run.database, &partial, run.opts)?;

    let db = Database::open(&run.database, database_options(&limits))?;
    let mut batches = Batches::new(&db, &limits);
    let mut counts = partial.counts;
    for head in heads {
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        run.table = head.phys_name.clone();
        run.progress(notes, Phase::Read);
        let plan = {
            let tx = batches.unit(0, "table")?;
            let descriptor = tx.read_table(&head.phys_name)?.descr().clone();
            Plan::new(&descriptor, &head)?
        };
        let mut count = 0u64;
        loop {
            match reader.next_event()? {
                Some(Event::TableEnd) => break,
                Some(Event::Row(row)) => {
                    check_row(&head, &row)?;
                    let tx =
                        batches.unit(plan.estimate(&row), &format!("a row of {}", plan.table))?;
                    let row = plan.row(row)?;
                    tx.table(&plan.table)?.insert(&row)?;
                    count += 1;
                    run.rows += 1;
                    run.tick(notes);
                }
                _ => return Err(bad("missing logical table end")),
            }
        }
        counts.push(count);
    }
    expect(reader.next_event()?, Event::DataBaseEnd)?;
    if reader.next_event()?.is_some() {
        return Err(bad("element after DataBase"));
    }
    batches.commit()?;
    commits += batches.commits;
    peak = peak.max(batches.peak);
    drop(batches);
    drop(db);
    Ok((
        Expected {
            counts,
            rows: run.rows,
            ..partial
        },
        commits,
        peak,
    ))
}

fn open_check(path: &Path, opts: &RestoreOptions<'_>) -> Result<Infobase> {
    let ib = Infobase::open(
        path,
        OpenOptions {
            locking: Locking::None,
            schema_check: SchemaCheck::Warn,
            memory_budget: opts.memory_budget.clone(),
            load_profile: mdconf::LoadProfile::Query,
            ..OpenOptions::mode(OpenMode::ReadOnly)
        },
    )?;
    for w in ib.warnings() {
        match w {
            Warning::SchemaMismatch(_) | Warning::UnsupportedObjects(_) | Warning::Extensions => {}
            other => return Err(bad(format!("open warning: {other:?}"))),
        }
    }
    Ok(ib)
}

fn open_warnings(ib: &Infobase) -> Vec<OpenWarning> {
    ib.warnings()
        .iter()
        .filter_map(|w| match w {
            Warning::SchemaMismatch(v) => Some(OpenWarning {
                kind: "SchemaMismatch",
                items: v.len(),
            }),
            Warning::UnsupportedObjects(v) => Some(OpenWarning {
                kind: "UnsupportedObjects",
                items: v.len(),
            }),
            Warning::Extensions => Some(OpenWarning {
                kind: "Extensions",
                items: 1,
            }),
            _ => None,
        })
        .collect()
}

fn check_metadata(
    path: &Path,
    expected: &Expected,
    opts: &RestoreOptions<'_>,
) -> Result<(bool, Vec<OpenWarning>)> {
    let ib = open_check(path, opts)?;
    if ib.raw().dbschema_text()? != expected.schema.to_text()
        || ib.raw().dbnames_text()?.as_slice() != expected.dbnames.as_slice()
    {
        return Err(bad("schema or DBNames"));
    }
    match ib.configuration() {
        Some(cfg) => {
            if i64::from(cfg.compatibility_mode().0) != expected.compat {
                return Err(bad(format!(
                    "DT compatibility mode {} differs from CONFIG {}",
                    expected.compat,
                    cfg.compatibility_mode().0
                )));
            }
        }
        None => {
            if expected.compat != 80501 {
                return Err(bad("unqualified no-config schema/data"));
            }
            if expected.schema != seed().schema {
                if !ibcmd::matches(&expected.schema)? {
                    return Err(bad("unqualified no-config schema/data"));
                }
                ibcmd::check_metadata(&ib)?;
            }
        }
    }
    Ok((ib.configuration().is_some(), open_warnings(&ib)))
}

fn final_checks(
    path: &Path,
    expected: &Expected,
    opts: &RestoreOptions<'_>,
) -> Result<Vec<OpenWarning>> {
    let (configured, warnings) = check_metadata(path, expected, opts)?;
    if !configured && expected.rows != 0 {
        return Err(bad("unqualified no-config schema/data"));
    }
    let db = Database::open(
        path,
        onecd::OpenOptions {
            locking: Locking::None,
            cache_bytes: opts.limits.cache_bytes,
            ..onecd::OpenOptions::read_only()
        },
    )?;
    let report = db.verify()?;
    if !report.violations.is_empty() {
        return Err(bad("engine verification"));
    }
    let expected_descrs = descriptors(&expected.schema, &expected.users_descr, configured)?;
    let mut names: Vec<_> = expected_descrs.iter().map(|d| d.name.clone()).collect();
    names.sort();
    let mut actual = db.info().tables;
    actual.sort();
    if actual != names {
        return Err(bad("final catalog"));
    }
    let rows_of = |name: &str| {
        report
            .table_rows
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, c)| *c)
            .ok_or_else(|| bad(format!("final count of {name}")))
    };
    if configured || ibcmd::matches(&expected.schema)? {
        for table in ibcmd::extra_tables()? {
            if rows_of(&table.name)? != 0 {
                return Err(bad("unexpected binary-data storage rows"));
            }
        }
    }
    let read = db.read();
    let mut actual_counts = Vec::new();
    for name in FOLDERS {
        let table = read.table(name)?;
        let part = table
            .descr()
            .field("PARTNO")
            .ok_or_else(|| bad("folder PARTNO"))?
            .0;
        let mut count = 0;
        for row in table.scan() {
            let (_, row) = row?;
            if matches!(row.get(part), Some(Value::Numeric(n)) if n.eq_value(&Numeric::from_i64(0)))
            {
                count += 1;
            }
        }
        actual_counts.push(count);
    }
    for name in ["V8USERS", "V8USERPWDPLCS", "V8CMSDPWDS"] {
        actual_counts.push(rows_of(name)?);
    }
    for head in TableHead::database(&expected.schema)? {
        actual_counts.push(rows_of(&head.phys_name)?);
    }
    if actual_counts != expected.counts {
        return Err(bad("final row/file counts"));
    }
    for d in expected_descrs {
        let table = read.table(&d.name)?;
        let mut actual = table.descr().clone();
        actual.files = [0; 3];
        if actual != d {
            return Err(bad(format!("final descriptor {}", d.name)));
        }
    }
    let t = read.table("IBVERSION")?;
    let rows = t.scan().collect::<onecd::Result<Vec<_>>>()?;
    let [(_, row)] = rows.as_slice() else {
        return Err(bad("IBVERSION cardinality"));
    };
    for (field, n) in [
        ("IBVERSION", seed().ibversion.0),
        ("PLATFORMVERSIONREQ", seed().ibversion.1),
    ] {
        let index = t
            .descr()
            .field(field)
            .ok_or_else(|| bad("IBVERSION field"))?
            .0;
        if !matches!(row.get(index), Some(Value::Numeric(value)) if value.eq_value(&Numeric::from_i64(i64::from(n))))
        {
            return Err(bad("IBVERSION policy"));
        }
    }
    let t = read.table("SCHEMASTORAGE")?;
    let rows = t.scan().collect::<onecd::Result<Vec<_>>>()?;
    let [(_, row)] = rows.as_slice() else {
        return Err(bad("SCHEMASTORAGE cardinality"));
    };
    let field = |name: &str| -> Result<&Value> {
        let index = t
            .descr()
            .field(name)
            .ok_or_else(|| bad("SCHEMASTORAGE field"))?
            .0;
        row.get(index).ok_or_else(|| bad("SCHEMASTORAGE row"))
    };
    let bytes = |v: &Value| -> Result<Vec<u8>> {
        match v {
            Value::Image(BlobVal::Ref(r)) => Ok(t.blob_bytes(*r)?),
            Value::Image(BlobVal::Data(b)) => Ok(b.clone()),
            _ => Err(bad("SCHEMASTORAGE blob")),
        }
    };
    let number = |v: &Value, n: u32| matches!(v, Value::Numeric(x) if x.eq_value(&Numeric::from_i64(i64::from(n))));
    if !number(field("SCHEMAID")?, seed().schemastorage.0)
        || !number(field("STATUS")?, seed().schemastorage.1)
        || bytes(field("CURRENTSCHEMA")?)? != expected.schema.to_text()
        || bytes(field("NEWGENCREATED")?)? != seed().newgen
        || bytes(field("NEWGENDROPPED")?)? != seed().newgen
    {
        return Err(bad("SCHEMASTORAGE policy"));
    }
    if rows_of("V8USERSMATKEYS")? != expected.matching_keys {
        return Err(bad("V8USERSMATKEYS row count"));
    }
    Ok(warnings)
}
