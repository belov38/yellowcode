use crate::config::{
    MeteredConfigSource, OpenLease, load_configuration_profile, present_service_tables,
};
use crate::create::{CreateOptions, create_empty};
use crate::error::{Error, Result};
use crate::maybe_charged::MaybeCharged;
use crate::raw::RawIb;
use crate::seed::seed;
use crate::store1cd::Store1cd;
use ibschema::dbnames::DbNames;
use ibschema::logical::LogicalSchema;
use ibschema::rules::{self, RulesOptions};
use ibschema::storage::StorageMap;
use ibschema::store::{Store, WriterGate};
use mdconf::ConfigurationExt;
use mdconf::{Configuration, LoadMeter, LoadOptions, LoadProfile};
use onecd::{Database, Format, Locking};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::config::check_schema;

const SI_FILES: [&str; 3] = [
    "1a621f0f-5568-4183-bd9f-f6ef670e7090.si",
    "2203278d-ef4f-4f68-98f1-feb257d53ecc.si",
    "a07b62f0-1f01-484a-93d9-d42764cedac0.si",
];

const MIN_WRITABLE_COMPAT: u32 = 80311;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    ReadOnly,
    ReadWrite,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaCheck {
    #[default]
    Strict,
    Warn,
}

#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub memory_budget: Option<memory_budget::MemoryBudget>,
    pub mode: OpenMode,
    pub max_dirty_bytes: usize,
    pub cache_bytes: usize,
    pub locking: Locking,
    pub schema_check: SchemaCheck,
    pub load_profile: LoadProfile,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions::mode(OpenMode::ReadOnly)
    }
}

impl OpenOptions {
    pub fn mode(mode: OpenMode) -> OpenOptions {
        let e = onecd::OpenOptions::read_only();
        OpenOptions {
            memory_budget: None,
            mode,
            max_dirty_bytes: e.max_dirty_bytes,
            cache_bytes: e.cache_bytes,
            locking: Locking::Exclusive,
            schema_check: SchemaCheck::Strict,
            load_profile: LoadProfile::Full,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    NoSchemaStorage,
    PendingConfiguration,
    MissingSi(Vec<String>),
    Extensions,
    ReadOnlyFormat(String),
    SchemaMismatch(Vec<String>),
    UnsupportedObjects(Vec<String>),
}

pub struct Infobase {
    path: PathBuf,
    mode: OpenMode,
    raw: RawIb,
    map: Arc<StorageMap>,
    config: Option<Arc<Configuration>>,
    gate: Arc<WriterGate>,
    warnings: Vec<Warning>,
    write_refusal: Option<String>,
    store: Arc<Store1cd>,
    dbnames: MaybeCharged<DbNames>,
    schema: MaybeCharged<Arc<LogicalSchema>>,
    metadata: Option<OpenLease>,
}

#[cfg(feature = "calibration")]
#[doc(hidden)]
pub static OPEN_PROBE: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

#[doc(hidden)]
#[inline]
pub fn probe(stage: &str) {
    #[cfg(feature = "calibration")]
    if let Some(f) = OPEN_PROBE.get() {
        f(stage);
    }
    #[cfg(not(feature = "calibration"))]
    let _ = stage;
}

#[cfg(feature = "calibration")]
#[doc(hidden)]
pub static OPEN_INPUT_PROBE: std::sync::OnceLock<fn(&str, u64)> = std::sync::OnceLock::new();

#[doc(hidden)]
#[inline]
pub fn probe_input(name: &str, value: u64) {
    #[cfg(feature = "calibration")]
    if let Some(f) = OPEN_INPUT_PROBE.get() {
        f(name, value);
    }
    #[cfg(not(feature = "calibration"))]
    let _ = (name, value);
}

const METADATA_VALUE_LIMIT: u64 = zraw::DEFAULT_LIMIT;

fn memory(error: memory_budget::MemoryLimit) -> Error {
    Error::Memory(memory_budget::BudgetError::Limit(error))
}

fn memory_first(error: Error) -> Error {
    match error {
        Error::Engine(onecd::Error::Memory(e))
        | Error::Inflate(zraw::ManagedInflateError::Memory(e))
        | Error::Schema(ibschema::Error::Memory(e)) => Error::Memory(e),
        Error::ConfigTable {
            source: mdconf::Error::Memory(m),
            ..
        }
        | Error::Config(mdconf::Error::Memory(m)) => memory(m),
        other => {
            let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&other);
            while let Some(e) = cause {
                if let Some(b) = e.downcast_ref::<memory_budget::BudgetError>() {
                    return Error::Memory(*b);
                }
                if let Some(m) = e.downcast_ref::<memory_budget::MemoryLimit>() {
                    return memory(*m);
                }
                cause = e.source();
            }
            other
        }
    }
}

fn schema_error(error: ibschema::Error) -> Error {
    match error {
        ibschema::Error::Memory(e) => Error::Memory(e),
        other => Error::Schema(other),
    }
}

fn load_config_metered(
    txn: &onecd::ReadTxn,
    budget: &memory_budget::MemoryBudget,
    lease: &Arc<OpenLease>,
) -> Result<Option<(Configuration, u64)>> {
    use crate::config::metered::model;
    probe("open:config_index");
    let src = crate::config::ManagedConfigTxnSource::read_in(txn, "CONFIG", budget)?;
    if src.is_empty() {
        return Ok(None);
    }
    let entries = u64::try_from(src.list().len()).unwrap_or(u64::MAX);
    probe("open:config_load");
    lease
        .grow(
            model::PER_ENTRY
                .saturating_mul(entries)
                .saturating_add(model::CONFIG_FIXED),
            false,
        )
        .map_err(memory)?;
    let src = MeteredConfigSource::new(src, budget.clone());
    let load = LoadOptions {
        keep_source: false,
        profile: LoadProfile::Query,
        meter: Some(Arc::clone(lease) as Arc<dyn LoadMeter>),
        ..LoadOptions::default()
    };
    let cfg = Configuration::load(&src, &load).map_err(|source| match source {
        mdconf::Error::Memory(m) => memory(m),
        source => Error::ConfigTable {
            table: "CONFIG".to_string(),
            source,
        },
    })?;
    Ok(Some((cfg, entries)))
}

pub(crate) fn configuration_in_budget(
    file: &Path,
    budget: &memory_budget::MemoryBudget,
) -> Result<Option<Configuration>> {
    let fixed = OpenLease::reserve_fixed(budget)?;
    let db = Database::open(
        file,
        onecd::OpenOptions {
            memory_budget: Some(budget.clone()),
            locking: Locking::None,
            ..onecd::OpenOptions::read_only()
        },
    )
    .map_err(|e| memory_first(e.into()))?;
    let db = Arc::new(db);
    let evictor: std::sync::Weak<dyn memory_budget::Evictor + Send> =
        Arc::<Database>::downgrade(&db);
    let budget = budget.try_with_evictor(evictor)?;
    let lease = Arc::new(OpenLease::new(
        fixed,
        Arc::clone(&db) as Arc<dyn memory_budget::Evictor + Send>,
    ));
    let txn = db.read();
    let cfg = load_config_metered(&txn, &budget, &lease).map_err(memory_first)?;
    Ok(cfg.map(|(cfg, _)| cfg))
}

impl Infobase {
    pub fn open(path: &Path, opts: OpenOptions) -> Result<Infobase> {
        let file = if path.is_dir() {
            path.join("1Cv8.1CD")
        } else {
            path.to_path_buf()
        };
        let mut eo = onecd::OpenOptions {
            memory_budget: opts.memory_budget.clone(),
            locking: opts.locking,
            cache_bytes: opts.cache_bytes,
            max_dirty_bytes: opts.max_dirty_bytes,
            ..onecd::OpenOptions::read_only()
        };
        if opts.mode == OpenMode::ReadWrite {
            eo.mode = onecd::Mode::ReadWrite;
        }
        let fixed = match &opts.memory_budget {
            Some(budget)
                if opts.load_profile == LoadProfile::Query && opts.mode != OpenMode::ReadWrite =>
            {
                Some(OpenLease::reserve_fixed(budget)?)
            }
            _ => None,
        };
        let db = match Database::open(&file, eo) {
            Ok(db) => Arc::new(db),
            Err(onecd::Error::Unsupported(onecd::Unsupported::WriteLegacyFormat)) => {
                return Err(Error::Unsupported("8.2.14 format: read only".into()));
            }
            Err(e) if fixed.is_some() => return Err(memory_first(e.into())),
            Err(e) => return Err(e.into()),
        };
        let legacy = db.info().format == Format::V8214;
        if let (Some(fixed), Some(budget)) = (fixed, &opts.memory_budget) {
            return Self::open_metered(file, &opts, budget.clone(), fixed, db, legacy)
                .map_err(memory_first);
        }
        let raw = RawIb::from_db(db);

        let dbnames = DbNames::parse(&raw.dbnames_text()?)?;
        let schema_text = raw.dbschema_text()?;
        let schema = Arc::new(LogicalSchema::from_text(&schema_text)?);
        let mut warnings = Vec::new();
        match raw.schema_storage()? {
            None => warnings.push(Warning::NoSchemaStorage),
            Some(s) if s.current != schema_text => {
                return Err(Error::Inconsistent(
                    "SCHEMASTORAGE.CURRENTSCHEMA differs from DBSCHEMA".into(),
                ));
            }
            Some(_) => {}
        }

        let mut write_refusal = None;
        let kept_schema = MaybeCharged::Legacy(Arc::clone(&schema));
        let (map, config) = if opts.mode == OpenMode::Raw {
            write_refusal = Some("raw mode".to_string());
            (StorageMap::from_schema(schema)?, None)
        } else {
            match load_configuration_profile(&raw, "CONFIG", opts.load_profile)? {
                None => (StorageMap::from_schema(schema)?, None),
                Some(cfg) => {
                    let warn = opts.schema_check == SchemaCheck::Warn;
                    let base = present_service_tables(&seed().schema, &dbnames);
                    let built = rules::build(
                        &cfg,
                        &mut dbnames.clone(),
                        &base,
                        &RulesOptions {
                            skip_unsupported: warn,
                        },
                    )?;
                    let mut diff = check_schema(&built, &schema);
                    let map = if diff.is_empty() {
                        StorageMap::new(schema, &built)?
                    } else if warn {
                        let (map, notes) = StorageMap::new_lenient(schema, &built)?;
                        diff.extend(notes);
                        map
                    } else {
                        return Err(Error::SchemaMismatch(diff));
                    };
                    if !diff.is_empty() {
                        write_refusal = Some("schema differs from the rules".to_string());
                        warnings.push(Warning::SchemaMismatch(diff));
                    }
                    if !built.skipped.is_empty() {
                        write_refusal.get_or_insert_with(|| "kinds without rules".to_string());
                        warnings.push(Warning::UnsupportedObjects(built.skipped.clone()));
                    }
                    (map, Some(Arc::new(cfg)))
                }
            }
        };

        if raw.has_rows("CONFIGSAVE")? {
            warnings.push(Warning::PendingConfiguration);
        }
        if raw.has_rows("CONFIG")? {
            let mut missing = Vec::new();
            for n in SI_FILES {
                if raw.folder_file("PARAMS", n)?.is_none() {
                    missing.push(n.to_string());
                }
            }
            if !missing.is_empty() {
                warnings.push(Warning::MissingSi(missing));
            }
        }
        if let Some(cfg) = &config {
            let c = cfg.compatibility_mode().0;
            if c < MIN_WRITABLE_COMPAT {
                warnings.push(Warning::ReadOnlyFormat(format!(
                    "compatibility mode {c} is below {MIN_WRITABLE_COMPAT}"
                )));
            }
        }
        if let Some(t) = map
            .table_by_name("ExtensionsInfo")
            .and_then(|id| map.tables().get(id.0 as usize))
        {
            if !raw.has_table(&t.phys) {
                return Err(Error::Inconsistent(format!(
                    "ExtensionsInfo is in DBSCHEMA but the 1CD has no table {}",
                    t.phys
                )));
            }
            if raw.has_rows(&t.phys)? {
                warnings.push(Warning::Extensions);
                write_refusal.get_or_insert_with(|| "extensions".to_string());
            }
        }
        if legacy {
            warnings.push(Warning::ReadOnlyFormat("8.2.14 format".into()));
        }
        if opts.mode == OpenMode::ReadWrite
            && let Some(Warning::ReadOnlyFormat(why)) = warnings
                .iter()
                .find(|w| matches!(w, Warning::ReadOnlyFormat(_)))
        {
            return Err(Error::Unsupported(format!("{why}: read only")));
        }
        if opts.mode == OpenMode::ReadWrite
            && let Some(why) = &write_refusal
            && warnings.iter().any(|w| {
                matches!(
                    w,
                    Warning::SchemaMismatch(_) | Warning::UnsupportedObjects(_)
                )
            })
        {
            return Err(Error::Unsupported(format!("{why}: read only")));
        }
        let map = Arc::new(map);
        let gate = WriterGate::new();
        let store = Arc::new(Store1cd::new(
            Arc::clone(raw.db_arc()),
            Arc::clone(&map),
            Arc::clone(&gate),
            write_refusal.clone(),
        )?);
        Ok(Infobase {
            path: file,
            mode: opts.mode,
            raw,
            map,
            config,
            gate,
            warnings,
            write_refusal,
            store,
            dbnames: MaybeCharged::Legacy(dbnames),
            schema: kept_schema,
            metadata: None,
        })
    }

    fn open_metered(
        file: PathBuf,
        opts: &OpenOptions,
        budget: memory_budget::MemoryBudget,
        fixed: memory_budget::Reservation,
        db: Arc<Database>,
        legacy: bool,
    ) -> Result<Infobase> {
        use crate::config::metered::model;
        use crate::raw::managed::{
            dbnames_text_in_budgeted, dbschema_text_in_budgeted, folder_file_data_in_budgeted,
            has_rows_in_budgeted, schema_storage_current_in_budgeted,
        };
        let evictor: std::sync::Weak<dyn memory_budget::Evictor + Send> =
            Arc::<Database>::downgrade(&db);
        let budget = budget.try_with_evictor(evictor)?;
        let lease = Arc::new(OpenLease::new(
            fixed,
            Arc::clone(&db) as Arc<dyn memory_budget::Evictor + Send>,
        ));
        let grow = |bytes: u64, retained: bool| lease.grow(bytes, retained).map_err(memory);
        let raw = RawIb::from_db(Arc::clone(&db));
        let txn = db.read();
        probe("open:dbnames");
        let names_text = dbnames_text_in_budgeted(&txn, &budget, METADATA_VALUE_LIMIT)?;
        let names_len = u64::try_from(names_text.len()).unwrap_or(u64::MAX);
        let dbnames = DbNames::parse_budgeted(&names_text, &budget).map_err(schema_error)?;
        drop(names_text);
        probe("open:dbschema");
        let schema_text = dbschema_text_in_budgeted(&txn, &budget, METADATA_VALUE_LIMIT)?;
        let schema_len = u64::try_from(schema_text.len()).unwrap_or(u64::MAX);
        let schema = LogicalSchema::from_text_budgeted(&schema_text, &budget)
            .map_err(schema_error)?
            .map(Arc::new);
        probe("open:schema_storage");
        let mut warnings = Vec::new();
        match schema_storage_current_in_budgeted(&txn, &budget, METADATA_VALUE_LIMIT)? {
            None => warnings.push(Warning::NoSchemaStorage),
            Some(current) if current.as_slice() != schema_text.as_slice() => {
                return Err(Error::Inconsistent(
                    "SCHEMASTORAGE.CURRENTSCHEMA differs from DBSCHEMA".into(),
                ));
            }
            Some(_) => {}
        }
        drop(schema_text);

        let mut write_refusal = None;
        let plain_map = |grow: &dyn Fn(u64, bool) -> Result<()>| -> Result<StorageMap> {
            grow(model::PER_MAP_BYTE.saturating_mul(schema_len), true)?;
            Ok(StorageMap::from_schema(Arc::clone(&schema))?)
        };
        let (map, config) = if opts.mode == OpenMode::Raw {
            write_refusal = Some("raw mode".to_string());
            (plain_map(&grow)?, None)
        } else {
            match load_config_metered(&txn, &budget, &lease)? {
                None => (plain_map(&grow)?, None),
                Some((cfg, entries)) => {
                    let (decoded, inflated) = lease.admitted();
                    probe_input("entries", entries);
                    probe_input("decoded_entries", decoded);
                    probe_input("inflated_bytes", inflated);
                    let (held, copied) = lease.admitted_shapes();
                    probe_input("held_max", held);
                    probe_input("copied_bytes", copied);
                    probe_input("dbnames_bytes", names_len);
                    probe_input("dbschema_bytes", schema_len);
                    let rules_bytes = model::PER_DBNAMES_BYTE
                        .saturating_mul(names_len)
                        .saturating_add(model::PER_RULES_BYTE.saturating_mul(schema_len));
                    probe("open:rules");
                    grow(rules_bytes, false)?;
                    let warn = opts.schema_check == SchemaCheck::Warn;
                    let base = present_service_tables(&seed().schema, &dbnames);
                    let built = rules::build(
                        &cfg,
                        &mut (*dbnames).clone(),
                        &base,
                        &RulesOptions {
                            skip_unsupported: warn,
                        },
                    )?;
                    drop(base);
                    let mut diff = check_schema(&built, &schema);
                    probe("open:map");
                    grow(model::PER_MAP_BYTE.saturating_mul(schema_len), true)?;
                    let map = if diff.is_empty() {
                        StorageMap::new(Arc::clone(&schema), &built)?
                    } else if warn {
                        let (map, notes) = StorageMap::new_lenient(Arc::clone(&schema), &built)?;
                        diff.extend(notes);
                        map
                    } else {
                        return Err(Error::SchemaMismatch(diff));
                    };
                    if !diff.is_empty() {
                        write_refusal = Some("schema differs from the rules".to_string());
                        warnings.push(Warning::SchemaMismatch(diff));
                    }
                    if !built.skipped.is_empty() {
                        write_refusal.get_or_insert_with(|| "kinds without rules".to_string());
                        warnings.push(Warning::UnsupportedObjects(built.skipped.clone()));
                    }
                    drop(built);
                    lease.shrink(rules_bytes).map_err(memory)?;
                    (map, Some(Arc::new(cfg)))
                }
            }
        };

        probe("open:service");
        if has_rows_in_budgeted(&txn, "CONFIGSAVE", &budget)? {
            warnings.push(Warning::PendingConfiguration);
        }
        if has_rows_in_budgeted(&txn, "CONFIG", &budget)? {
            let mut missing = Vec::new();
            for n in SI_FILES {
                let found =
                    folder_file_data_in_budgeted(&txn, "PARAMS", n, &budget, METADATA_VALUE_LIMIT)?;
                if found.is_none() {
                    missing.push(n.to_string());
                }
            }
            if !missing.is_empty() {
                warnings.push(Warning::MissingSi(missing));
            }
        }
        if let Some(cfg) = &config {
            let c = cfg.compatibility_mode().0;
            if c < MIN_WRITABLE_COMPAT {
                warnings.push(Warning::ReadOnlyFormat(format!(
                    "compatibility mode {c} is below {MIN_WRITABLE_COMPAT}"
                )));
            }
        }
        if let Some(t) = map
            .table_by_name("ExtensionsInfo")
            .and_then(|id| map.tables().get(id.0 as usize))
        {
            if txn.find_table(&t.phys)?.is_none() {
                return Err(Error::Inconsistent(format!(
                    "ExtensionsInfo is in DBSCHEMA but the 1CD has no table {}",
                    t.phys
                )));
            }
            if has_rows_in_budgeted(&txn, &t.phys, &budget)? {
                warnings.push(Warning::Extensions);
                write_refusal.get_or_insert_with(|| "extensions".to_string());
            }
        }
        if legacy {
            warnings.push(Warning::ReadOnlyFormat("8.2.14 format".into()));
        }
        drop(txn);
        probe("open:store");
        grow(model::PER_PHYS_BYTE.saturating_mul(schema_len), true)?;
        let readers = model::PER_STORE_TABLE.saturating_mul(map.tables().len() as u64);
        grow(readers, false)?;
        let map = Arc::new(map);
        let gate = WriterGate::new();
        let store = Arc::new(Store1cd::new(
            Arc::clone(&db),
            Arc::clone(&map),
            Arc::clone(&gate),
            write_refusal.clone(),
        )?);
        lease.shrink(readers).map_err(memory)?;
        probe("open:done");
        let lease = Arc::try_unwrap(lease).map_err(|_| {
            Error::InconsistentStatic("metadata lease still shared after the configuration load")
        })?;
        db.keep_opened_files();
        Ok(Infobase {
            path: file,
            mode: opts.mode,
            raw,
            map,
            config,
            gate,
            warnings,
            write_refusal,
            store,
            dbnames: MaybeCharged::Charged(dbnames),
            schema: MaybeCharged::Charged(schema),
            metadata: Some(lease),
        })
    }

    pub fn create(dir: &Path, opts: &CreateOptions) -> Result<Infobase> {
        create_empty(dir, opts)?;
        Infobase::open(dir, OpenOptions::mode(OpenMode::ReadWrite))
    }

    pub fn mode(&self) -> OpenMode {
        self.mode
    }
    pub fn file(&self) -> &Path {
        &self.path
    }
    pub fn raw(&self) -> &RawIb {
        &self.raw
    }
    pub fn dbnames(&self) -> &DbNames {
        &self.dbnames
    }
    pub fn storage(&self) -> &Arc<StorageMap> {
        &self.map
    }
    pub fn configuration(&self) -> Option<Arc<Configuration>> {
        self.config.clone()
    }
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
    pub fn gate(&self) -> &Arc<WriterGate> {
        &self.gate
    }
    pub fn store(&self) -> Arc<dyn Store> {
        Arc::clone(&self.store) as Arc<dyn Store>
    }
    pub fn write_refusal(&self) -> Option<&str> {
        self.write_refusal.as_deref()
    }
    pub fn into_startup_owners(self) -> StartupOwners {
        let db = Arc::clone(self.raw.db_arc());
        let (metadata, transient) = match self.metadata {
            Some(lease) => {
                let transient = lease.bytes().saturating_sub(lease.retained());
                (Some(lease.into_reservation()), transient)
            }
            None => (None, 0),
        };
        StartupOwners {
            store: self.store as Arc<dyn Store>,
            map: self.map,
            config: self.config,
            warnings: self.warnings,
            db,
            dbnames: self.dbnames,
            schema: self.schema,
            metadata,
            transient,
        }
    }

    pub fn open_lease(&self) -> Option<&OpenLease> {
        self.metadata.as_ref()
    }
}

pub struct StartupOwners {
    pub store: Arc<dyn Store>,
    pub map: Arc<StorageMap>,
    pub config: Option<Arc<Configuration>>,
    pub warnings: Vec<Warning>,
    pub db: Arc<Database>,
    pub dbnames: MaybeCharged<DbNames>,
    pub schema: MaybeCharged<Arc<LogicalSchema>>,
    pub metadata: Option<memory_budget::Reservation>,
    pub transient: u64,
}
