use mdconf::{CfFileExt, ConfigurationExt};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use infobase::raw::RawIb;
use mdconf::{CfFile, Configuration, ConfigurationInspection, LoadOptions};

use onecd::{Locking, OpenOptions};

use crate::Result;

mod dt;
mod dt_spill;
mod managed_dt;
pub use dt::ManagedSpoolError as ManagedDtSourceError;
pub use managed_dt::{ManagedDtExtractError, ManagedDtSource};

#[derive(Clone)]
pub struct ConfigEntries {
    spool: std::sync::Arc<dt::Spool>,
    fingerprint: Fingerprint,
}
impl ConfigEntries {
    pub fn read_range(
        &self,
        name: &str,
        offset: u64,
        len: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        use mdconf::EntrySource;
        self.spool.stored_range_owned(name, offset, len)
    }
    pub fn summary(&self) -> ConfigEntrySummary {
        self.spool.summary()
    }
    pub fn largest_range(
        &self,
        offset: u64,
        len: usize,
    ) -> mdconf::Result<Option<mdconf::StoredBytes>> {
        self.spool.largest_range(offset, len)
    }
    pub(crate) fn load(
        &self,
        path: &Path,
        opts: &LoadOptions,
        expect: Option<&Fingerprint>,
    ) -> std::result::Result<(Configuration, Fingerprint), OpenError> {
        let fp = file_fingerprint(path)?;
        if fp != self.fingerprint || expect.is_some_and(|e| e != &fp) {
            return Err(OpenError::Changed);
        }
        let cfg = Configuration::load(&*self.spool, opts).map_err(|e| classify(path, &e))?;
        if file_fingerprint(path)? != fp {
            return Err(OpenError::Changed);
        }
        Ok((cfg, fp))
    }
}
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ConfigEntrySummary {
    pub entries: usize,
    pub stored_bytes: u64,
    pub largest_entry: u64,
}

pub(crate) fn open_retained_dt(
    path: &Path,
    opts: &LoadOptions,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<(Configuration, Fingerprint, ConfigEntries)> {
    progress("container")?;
    let fp = file_fingerprint(path).map_err(|e| e.to_string())?;
    let spool = std::sync::Arc::new(dt::extract_with_memory(path, memory, progress)?);
    progress("metadata")?;
    let cfg = {
        let _phase = memory_budget::profile::phase(memory_budget::profile::Phase::Metadata);
        Configuration::load(&*spool, opts).map_err(|e| dt::memory_hint(e.into()))?
    };
    if file_fingerprint(path).map_err(|e| e.to_string())? != fp {
        return Err("DT input changed during metadata load".into());
    }
    Ok((
        cfg,
        fp.clone(),
        ConfigEntries {
            spool,
            fingerprint: fp,
        },
    ))
}

#[derive(Clone)]
pub struct OpenMemory {
    pub(super) budget: memory_budget::MemoryBudget,
    pub(super) startup: u64,
    _control:
        Option<std::sync::Arc<std::sync::Mutex<memory_budget::control::storage::ControlReserve>>>,
}
impl OpenMemory {
    pub fn from_context(context: &crate::service::memory::Context) -> Self {
        Self {
            budget: context.ledger().clone(),
            startup: context.restore_budget().0,
            _control: None,
        }
    }
    pub fn resolve(limit: Option<String>) -> Result<Self> {
        let (context, reserve) = crate::service::memory::Settings::memory_only(limit).resolve()?;
        let mut memory = Self::from_context(&context);
        memory._control = Some(std::sync::Arc::new(std::sync::Mutex::new(reserve)));
        Ok(memory)
    }
    pub fn ledger(&self) -> &memory_budget::MemoryBudget {
        &self.budget
    }
    pub fn budget(&self) -> u64 {
        self.startup
    }
    pub(crate) fn from_budget(budget: &memory_budget::MemoryBudget) -> Self {
        Self {
            budget: budget.clone(),
            startup: budget.snapshot().admission,
            _control: None,
        }
    }
}
impl std::fmt::Debug for OpenMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenMemory")
            .field("startup", &self.startup)
            .finish_non_exhaustive()
    }
}

const SIG_1CD: &[u8; 8] = b"1CDBMSV8";

const INFOBASE_FILE: &str = "1Cv8.1CD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Cf,
    Dt,
    Infobase,
    XmlDir,
}

pub fn resolve(path: &Path) -> PathBuf {
    infobase_file(path).unwrap_or_else(|| path.to_path_buf())
}

fn infobase_file(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let exact = dir.join(INFOBASE_FILE);
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(INFOBASE_FILE))
                && e.path().is_file()
        })
        .map(|e| e.path())
}

pub fn detect(path: &Path) -> Result<SourceKind> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        if let Some(file) = infobase_file(path) {
            return detect(&file);
        }
        return if path.join("Configuration.xml").is_file() {
            Ok(SourceKind::XmlDir)
        } else {
            Err(format!(
                "{}: a directory with neither Configuration.xml nor {INFOBASE_FILE}",
                path.display()
            )
            .into())
        };
    }
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut head = [0u8; 8];
    let n = f
        .read(&mut head)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if head.get(..n) == Some(SIG_1CD.as_slice()) {
        return Ok(SourceKind::Infobase);
    }
    if head.get(..n) == Some(dtfile::container::SIGNATURE.as_slice())
        || head.get(..n) == Some(b"EVIBDmpF".as_slice())
    {
        return Ok(SourceKind::Dt);
    }
    match v8cont::detect(&f) {
        Ok(Some(_)) => Ok(SourceKind::Cf),
        _ => Err(format!(
            "{}: neither a v8 container (.cf, .cfe) nor a DT dump nor an infobase (1Cv8.1CD) nor a directory with Configuration.xml",
            path.display()
        )
        .into()),
    }
}

pub fn open(path: &Path, opts: &LoadOptions) -> Result<Configuration> {
    open_with_progress(path, opts, &mut |_| Ok(()))
}

pub fn open_with_progress(
    path: &Path,
    opts: &LoadOptions,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    open_inner(path, opts, None, progress)
}

pub fn open_with_memory(
    path: &Path,
    opts: &LoadOptions,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    open_inner(path, opts, Some(memory), progress)
}
fn open_inner(
    path: &Path,
    opts: &LoadOptions,
    memory: Option<&OpenMemory>,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> Result<Configuration> {
    let path = &resolve(path);
    progress("container")?;
    match detect(path)? {
        SourceKind::Cf => {
            let cf = CfFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            progress("metadata")?;
            Configuration::load(&cf, opts).map_err(|e| format!("{}: {e}", path.display()).into())
        }
        SourceKind::Dt => match memory {
            Some(memory) => dt::open_with_memory(path, opts, memory, progress),
            None => dt::open(path, opts, progress),
        },
        SourceKind::Infobase => {
            progress("metadata")?;
            open_infobase(path, opts)
        }
        SourceKind::XmlDir => Err("XML sources need stage C8 (load_xml)".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fingerprint {
    File {
        len: u64,
        modified: Option<std::time::SystemTime>,
    },
    Config([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    Changed,
    Transient(String),
    Decode(String),
    Pressure(String),
    Cancelled(String),
}

impl std::error::Error for OpenError {}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Changed => f.write_str("the source changed since it was opened"),
            OpenError::Transient(e)
            | OpenError::Decode(e)
            | OpenError::Pressure(e)
            | OpenError::Cancelled(e) => f.write_str(e),
        }
    }
}

pub(crate) fn is_io(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if c.downcast_ref::<std::io::Error>().is_some()
            || matches!(
                c.downcast_ref::<mdconf::Error>(),
                Some(mdconf::Error::Io(_))
            )
        {
            return true;
        }
        cur = c.source();
    }
    false
}

pub(crate) fn is_resource(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(e);
    while let Some(error) = current {
        if error.downcast_ref::<memory_budget::MemoryLimit>().is_some()
            || error.downcast_ref::<memory_budget::BudgetError>().is_some()
            || matches!(
                error.downcast_ref::<mdconf::Error>(),
                Some(mdconf::Error::Memory(_))
            )
            || error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::OutOfMemory)
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn classify(path: &Path, e: &(dyn std::error::Error + 'static)) -> OpenError {
    let mut current = Some(e);
    while let Some(error) = current {
        if let Some(opening) = error.downcast_ref::<OpenError>() {
            return opening.clone();
        }
        current = error.source();
    }
    let text = format!("{}: {e}", path.display());
    if is_resource(e) {
        OpenError::Pressure(text)
    } else if is_io(e) {
        OpenError::Transient(text)
    } else {
        OpenError::Decode(text)
    }
}

fn file_fingerprint(path: &Path) -> std::result::Result<Fingerprint, OpenError> {
    let meta = std::fs::metadata(path).map_err(|e| classify(path, &e))?;
    Ok(Fingerprint::File {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn config_fingerprint(src: &dyn mdconf::EntrySource) -> mdconf::Result<Fingerprint> {
    config_fingerprint_checked(src, &|| Ok(()))
}
fn config_fingerprint_checked(
    src: &dyn mdconf::EntrySource,
    check: &dyn Fn() -> mdconf::Result<()>,
) -> mdconf::Result<Fingerprint> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let list = src.list_owned_checked(check)?;
    for m in list.iter() {
        check()?;
        h.update((m.name.len() as u64).to_le_bytes());
        h.update(m.name.as_bytes());
        h.update(m.ctime.0.to_le_bytes());
        h.update(m.mtime.0.to_le_bytes());
        h.update(m.attrs.to_le_bytes());
    }
    Ok(Fingerprint::Config(h.finalize().into()))
}

fn read_infobase(path: &Path) -> std::result::Result<infobase::config::ConfigSource, OpenError> {
    let mut o = OpenOptions::read_only();
    o.locking = Locking::None;
    let ib = RawIb::open_with(path, o).map_err(|e| classify(path, &e))?;
    let src =
        infobase::config::ConfigSource::read(&ib, "CONFIG").map_err(|e| classify(path, &e))?;
    Ok(src)
}

pub fn fingerprint(path: &Path) -> std::result::Result<Fingerprint, OpenError> {
    let path = &resolve(path);
    match detect(path).map_err(|e| match std::fs::metadata(path) {
        Err(io) => classify(path, &io),
        Ok(_) => OpenError::Decode(e.to_string()),
    })? {
        SourceKind::Infobase => {
            let src = read_infobase(path)?;
            config_fingerprint(&src).map_err(|e| classify(path, &e))
        }
        SourceKind::XmlDir => Err(OpenError::Decode(
            "XML sources need stage C8 (load_xml)".into(),
        )),
        SourceKind::Cf | SourceKind::Dt => file_fingerprint(path),
    }
}

pub fn open_snapshot(
    path: &Path,
    opts: &LoadOptions,
    expect: Option<&Fingerprint>,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> std::result::Result<(Configuration, Fingerprint), OpenError> {
    open_snapshot_inner(path, opts, expect, None, progress)
}

pub fn open_snapshot_with_memory(
    path: &Path,
    opts: &LoadOptions,
    expect: Option<&Fingerprint>,
    memory: &OpenMemory,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> std::result::Result<(Configuration, Fingerprint), OpenError> {
    open_snapshot_inner(path, opts, expect, Some(memory), progress)
}
fn open_snapshot_inner(
    path: &Path,
    opts: &LoadOptions,
    expect: Option<&Fingerprint>,
    memory: Option<&OpenMemory>,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> std::result::Result<(Configuration, Fingerprint), OpenError> {
    let path = &resolve(path);
    let stop = |e: crate::Error| OpenError::Decode(e.to_string());
    progress("container").map_err(stop)?;
    let same = |f: &Fingerprint| expect.is_none_or(|x| x == f);
    let kind = detect(path).map_err(|e| match std::fs::metadata(path) {
        Err(io) => classify(path, &io),
        Ok(_) => OpenError::Decode(e.to_string()),
    })?;
    match kind {
        SourceKind::Cf => {
            let fp = file_fingerprint(path)?;
            if !same(&fp) {
                return Err(OpenError::Changed);
            }
            let cf = CfFile::open(path).map_err(|e| classify(path, &e))?;
            progress("metadata").map_err(stop)?;
            let cfg = Configuration::load(&cf, opts).map_err(|e| classify(path, &e))?;
            Ok((cfg, fp))
        }
        SourceKind::Dt => {
            let fp = file_fingerprint(path)?;
            if !same(&fp) {
                return Err(OpenError::Changed);
            }
            let cfg = match memory {
                Some(memory) => dt::open_with_memory(path, opts, memory, progress),
                None => dt::open(path, opts, progress),
            }
            .map_err(|e| classify(path, &*e))?;
            Ok((cfg, fp))
        }
        SourceKind::Infobase => {
            let src = read_infobase(path)?;
            let fp = config_fingerprint(&src).map_err(|e| classify(path, &e))?;
            if !same(&fp) {
                return Err(OpenError::Changed);
            }
            progress("metadata").map_err(stop)?;
            let cfg = if src.is_empty() {
                Configuration::default()
            } else {
                Configuration::load(&src, &infobase_options(opts))
                    .map_err(|e| classify(path, &e))?
            };
            Ok((cfg, fp))
        }
        SourceKind::XmlDir => Err(OpenError::Decode(
            "XML sources need stage C8 (load_xml)".into(),
        )),
    }
}

pub(crate) struct MemorySnapshot {
    pub cfg: Configuration,
    pub fingerprint: Fingerprint,
    pub lease: std::sync::Arc<super::memory::GuiLease>,
}

fn managed_config<T>(
    path: &Path,
    session: &super::memory::Session,
    consume: impl FnOnce(
        infobase::config::ManagedConfigTxnSource<'_>,
    ) -> std::result::Result<T, OpenError>,
) -> std::result::Result<T, OpenError> {
    session
        .check()
        .map_err(|e| OpenError::Cancelled(e.to_string()))?;
    let mut options = OpenOptions::read_only();
    options.locking = Locking::None;
    options.cache_bytes = session.cache_bytes();
    options.memory_budget = Some(session.budget().clone());
    let db = onecd::Database::open(path, options).map_err(|e| classify(path, &e))?;
    let txn = db.read();
    let check = || {
        session.check().map_err(|_| {
            infobase::Error::Config(mdconf::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "opening cancelled",
            )))
        })
    };
    let src = infobase::config::ManagedConfigTxnSource::read_in_checked(
        &txn,
        "CONFIG",
        session.budget(),
        &check,
    )
    .map_err(|e| {
        if session.is_cancelled() {
            OpenError::Cancelled(e.to_string())
        } else {
            classify(path, &e)
        }
    })?;
    consume(src)
}

pub(crate) fn fingerprint_with_memory(
    path: &Path,
    session: &super::memory::Session,
) -> std::result::Result<Fingerprint, OpenError> {
    session
        .check()
        .map_err(|e| OpenError::Cancelled(e.to_string()))?;
    if detect(path).map_err(|e| classify(path, &*e))? != SourceKind::Infobase {
        return file_fingerprint(path);
    }
    managed_config(path, session, |src| {
        let bytes = (src.list().len() as u64)
            .saturating_mul(128)
            .saturating_add(src.list().iter().map(|m| m.name.len() as u64).sum::<u64>());
        let _list = session
            .budget()
            .try_reserve(memory_budget::Category::Metadata, bytes)
            .map_err(|e| classify(path, &e))?;
        let src = infobase::config::MeteredConfigSource::new(src, session.budget().clone());
        config_fingerprint_checked(&src, &|| {
            session.check().map_err(|_| {
                mdconf::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "opening cancelled",
                ))
            })
        })
        .map_err(|e| {
            if session.is_cancelled() {
                OpenError::Cancelled(e.to_string())
            } else {
                classify(path, &e)
            }
        })
    })
}

pub(crate) fn open_snapshot_with_session(
    path: &Path,
    opts: &LoadOptions,
    expect: Option<&Fingerprint>,
    session: &super::memory::Session,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> std::result::Result<MemorySnapshot, OpenError> {
    session
        .check()
        .map_err(|e| OpenError::Cancelled(e.to_string()))?;
    let lease = super::memory::GuiLease::new(session).map_err(|e| classify(path, &e))?;
    open_snapshot_with_memory_lease(path, opts, expect, session, lease, progress)
}

pub(crate) fn open_snapshot_with_memory_lease(
    path: &Path,
    opts: &LoadOptions,
    expect: Option<&Fingerprint>,
    session: &super::memory::Session,
    lease: std::sync::Arc<super::memory::GuiLease>,
    progress: &mut impl FnMut(&'static str) -> Result<()>,
) -> std::result::Result<MemorySnapshot, OpenError> {
    use mdconf::EntrySource;
    use std::sync::Arc;
    session
        .check()
        .map_err(|e| OpenError::Cancelled(e.to_string()))?;
    let opts = LoadOptions {
        keep_source: false,
        meter: Some(lease.clone() as Arc<dyn mdconf::LoadMeter>),
        ..opts.clone()
    };
    progress("container").map_err(|e| OpenError::Cancelled(e.to_string()))?;
    let same = |fp: &Fingerprint| -> std::result::Result<(), OpenError> {
        if expect.is_none_or(|old| old == fp) {
            Ok(())
        } else {
            Err(OpenError::Changed)
        }
    };
    let classify_load = |e: &mdconf::Error| {
        if session.is_cancelled() {
            OpenError::Cancelled("opening cancelled".into())
        } else {
            classify(path, e)
        }
    };
    let (cfg, fingerprint) = match detect(path).map_err(|e| classify(path, &*e))? {
        SourceKind::Cf => {
            let fp = file_fingerprint(path)?;
            same(&fp)?;
            let cf = mdconf::ManagedCfFile::open(path, session.budget())
                .map_err(|e| classify(path, &e))?;
            let list = cf.list().map_err(|e| classify(path, &e))?;
            lease
                .entries(
                    list.len() as u64,
                    list.iter().map(|m| m.name.len() as u64).sum(),
                )
                .map_err(|e| classify(path, &e))?;
            drop(list);
            progress("metadata").map_err(|e| OpenError::Cancelled(e.to_string()))?;
            let cfg = Configuration::load(&cf, &opts).map_err(|e| classify_load(&e))?;
            (cfg, fp)
        }
        SourceKind::Infobase => managed_config(path, session, |src| {
            lease
                .entries(
                    src.list().len() as u64,
                    src.list().iter().map(|m| m.name.len() as u64).sum(),
                )
                .map_err(|e| classify(path, &e))?;
            let entries = infobase::config::MeteredConfigSource::new(src, session.budget().clone());
            let fp = config_fingerprint_checked(&entries, &|| {
                mdconf::LoadMeter::checkpoint(lease.as_ref())
            })
            .map_err(|e| classify_load(&e))?;
            same(&fp)?;
            progress("metadata").map_err(|e| OpenError::Cancelled(e.to_string()))?;
            let cfg = if entries.is_empty() {
                Configuration::default()
            } else {
                Configuration::load(&entries, &opts).map_err(|e| classify_load(&e))?
            };
            Ok((cfg, fp))
        })?,
        SourceKind::Dt => {
            let fp = file_fingerprint(path)?;
            same(&fp)?;
            let mut checked = |phase| {
                session.check()?;
                progress(phase)
            };
            let memory = OpenMemory::from_budget(session.budget());
            let spool = dt::extract_with_memory(path, &memory, &mut checked).map_err(|e| {
                if session.is_cancelled() {
                    OpenError::Cancelled(e.to_string())
                } else {
                    classify(path, &*e)
                }
            })?;
            let list = mdconf::EntrySource::list_owned(&spool).map_err(|e| classify(path, &e))?;
            lease
                .entries(
                    list.len() as u64,
                    list.iter().map(|m| m.name.len() as u64).sum(),
                )
                .map_err(|e| classify(path, &e))?;
            drop(list);
            progress("metadata").map_err(|e| OpenError::Cancelled(e.to_string()))?;
            let cfg = Configuration::load(&spool, &opts).map_err(|e| classify_load(&e))?;
            (cfg, fp)
        }
        SourceKind::XmlDir => {
            return Err(OpenError::Decode(
                "XML sources need stage C8 (load_xml)".into(),
            ));
        }
    };
    session
        .check()
        .map_err(|e| OpenError::Cancelled(e.to_string()))?;
    Ok(MemorySnapshot {
        cfg,
        fingerprint,
        lease,
    })
}

fn infobase_options(opts: &LoadOptions) -> LoadOptions {
    LoadOptions {
        keep_source: false,
        ..opts.clone()
    }
}

pub fn inspect(path: &Path, opts: &LoadOptions) -> Result<ConfigurationInspection> {
    if detect(path)? == SourceKind::Cf {
        let cf = CfFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        cf.inspect_configuration(opts)
            .map_err(|e| format!("{}: {e}", path.display()).into())
    } else {
        open(path, opts).map(|cfg| ConfigurationInspection::Configuration(Box::new(cfg)))
    }
}

fn open_infobase(path: &Path, opts: &LoadOptions) -> Result<Configuration> {
    let mut o = OpenOptions::read_only();
    o.locking = Locking::None;
    let ib = RawIb::open_with(path, o).map_err(|e| format!("{}: {e}", path.display()))?;
    let src = infobase::config::ConfigSource::read(&ib, "CONFIG")
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if src.is_empty() {
        return Ok(Configuration::default());
    }
    Configuration::load(&src, &infobase_options(opts)).map_err(|e| {
        let e = infobase::Error::ConfigTable {
            table: "CONFIG".to_string(),
            source: e,
        };
        format!("{}: {e}", path.display()).into()
    })
}

pub fn with_config_entries<T>(
    path: &Path,
    consume: impl FnOnce(&dyn mdconf::EntrySource) -> Result<T>,
) -> Result<T> {
    with_config_entries_inner(path, None, consume)
}

pub fn with_config_entries_with_memory<T>(
    path: &Path,
    memory: &OpenMemory,
    consume: impl FnOnce(&dyn mdconf::EntrySource) -> Result<T>,
) -> Result<T> {
    with_config_entries_inner(path, Some(memory), consume)
}
fn with_config_entries_inner<T>(
    path: &Path,
    memory: Option<&OpenMemory>,
    consume: impl FnOnce(&dyn mdconf::EntrySource) -> Result<T>,
) -> Result<T> {
    let path = &resolve(path);
    match detect(path)? {
        SourceKind::Cf => consume(&CfFile::open(path)?),
        SourceKind::Dt => {
            let spool = match memory {
                Some(memory) => dt::extract_with_memory(path, memory, &mut |_| Ok(()))?,
                None => dt::extract(path, &mut |_| Ok(()))?,
            };
            consume(&spool).map_err(dt::memory_hint)
        }
        SourceKind::Infobase => {
            let mut opts = OpenOptions::read_only();
            opts.locking = Locking::None;
            let raw = RawIb::open_with(path, opts)?;
            consume(&infobase::config::ConfigSource::read(&raw, "CONFIG")?)
        }
        SourceKind::XmlDir => Err("XML sources need stages C7/C8 (load_xml)".into()),
    }
}
