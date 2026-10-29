use crate::error::{Error, Result};
use crate::raw::{FolderEntry, FolderIndex, FolderTxnIndex, RawIb};
use ibschema::dbnames::DbNames;
use ibschema::logical::{LogicalSchema, Table};
use ibschema::rules::{self, Built, RulesOptions};
use ibschema::storage::StorageMap;
use mdconf::ConfigurationExt;
use mdconf::{Configuration, EntryMeta, EntrySource, LoadOptions, LoadProfile};
use onecd::{DateTime, Numeric, ReadTxn};
use std::collections::HashMap;
use std::sync::Arc;
use v8cont::V8Time;

mod managed;
pub mod metered;
pub use managed::ManagedConfigTxnSource;
pub use metered::{MeteredConfigSource, OpenLease};

pub struct ConfigSource {
    index: FolderIndex,
    metas: Vec<EntryMeta>,
    by_name: HashMap<String, usize>,
}

pub fn v8time_of(d: &DateTime) -> V8Time {
    V8Time::from_civil(
        i32::from(d.year),
        u32::from(d.month),
        u32::from(d.day),
        u32::from(d.hour),
        u32::from(d.minute),
        u32::from(d.second),
    )
    .unwrap_or(V8Time(0))
}

fn attrs_of(n: &Numeric, table: &str, name: &str) -> Result<u32> {
    let bad = || Error::Inconsistent(format!("{table}: {name}: bad ATTRIBUTES"));
    if n.scale() != 0 || (n.is_negative() && n.digits().iter().any(|&d| d != 0)) {
        return Err(bad());
    }
    n.digits().iter().try_fold(0u32, |acc, &d| {
        acc.checked_mul(10)
            .and_then(|a| a.checked_add(u32::from(d)))
            .ok_or_else(bad)
    })
}

fn entry_metas(
    entries: &[FolderEntry],
    table: &str,
) -> Result<(Vec<EntryMeta>, HashMap<String, usize>)> {
    let mut metas = Vec::with_capacity(entries.len());
    let mut by_name = HashMap::new();
    for f in entries {
        let meta = EntryMeta {
            attrs: attrs_of(&f.attributes, table, &f.name)?,
            ctime: v8time_of(&f.creation),
            mtime: v8time_of(&f.modified),
            name: f.name.clone(),
        };
        by_name.entry(meta.name.clone()).or_insert(metas.len());
        metas.push(meta);
    }
    Ok((metas, by_name))
}

pub struct ConfigTxnSource<'a> {
    index: FolderTxnIndex<'a>,
    metas: Vec<EntryMeta>,
    by_name: HashMap<String, usize>,
}

impl<'a> ConfigTxnSource<'a> {
    pub fn read(txn: &'a ReadTxn, table: &str) -> Result<Self> {
        let index = FolderTxnIndex::read(txn, table)?;
        let (metas, by_name) = entry_metas(&index.entries, table)?;
        Ok(Self {
            index,
            metas,
            by_name,
        })
    }

    pub fn read_bounded(
        txn: &'a ReadTxn,
        table: &str,
        max_rows: usize,
        max_name: usize,
    ) -> Result<Self> {
        let index = FolderTxnIndex::read_bounded(txn, table, max_rows, max_name)?;
        let (metas, by_name) = entry_metas(&index.entries, table)?;
        Ok(Self {
            index,
            metas,
            by_name,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }
}

impl EntrySource for ConfigTxnSource<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Ok(self.metas.clone())
    }

    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        let Some(&i) = self.by_name.get(name) else {
            return Ok(None);
        };
        self.index
            .data(i)
            .map(Some)
            .map_err(|e| mdconf::Error::Io(std::io::Error::other(format!("{name}: {e}"))))
    }

    fn stored_len(&self, name: &str) -> mdconf::Result<Option<u64>> {
        let Some(&i) = self.by_name.get(name) else {
            return Ok(None);
        };
        self.index.data_len(i).map(Some).map_err(|_| {
            mdconf::Error::Io(std::io::Error::other("CONFIG stored length unavailable"))
        })
    }

    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        let Some(&i) = self.by_name.get(name) else {
            return Ok(None);
        };
        self.index
            .data_bounded(i, limit)
            .map(Some)
            .map_err(|e| mdconf::Error::Io(std::io::Error::other(format!("{name}: {e}"))))
    }
}

pub fn load_configuration_in(txn: &ReadTxn, table: &str) -> Result<Option<Configuration>> {
    let src = ConfigTxnSource::read(txn, table)?;
    load_source(&src, src.is_empty(), table)
}

impl ConfigSource {
    pub fn read(ib: &RawIb, table: &str) -> Result<ConfigSource> {
        let index = ib.folder_index(table)?;
        let (metas, by_name) = entry_metas(&index.entries, table)?;
        Ok(ConfigSource {
            index,
            metas,
            by_name,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }
}

impl EntrySource for ConfigSource {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Ok(self.metas.clone())
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        let Some(&i) = self.by_name.get(name) else {
            return Ok(None);
        };
        self.index
            .data(i)
            .map(Some)
            .map_err(|e| mdconf::Error::Io(std::io::Error::other(format!("{name}: {e}"))))
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        let Some(&i) = self.by_name.get(name) else {
            return Ok(None);
        };
        self.index
            .data_bounded(i, limit)
            .map(Some)
            .map_err(|e| mdconf::Error::Io(std::io::Error::other(format!("{name}: {e}"))))
    }
}

pub fn load_configuration(ib: &RawIb, table: &str) -> Result<Option<Configuration>> {
    load_configuration_profile(ib, table, LoadProfile::Full)
}

pub fn load_configuration_profile(
    ib: &RawIb,
    table: &str,
    profile: LoadProfile,
) -> Result<Option<Configuration>> {
    let src = ConfigSource::read(ib, table)?;
    load_source_with(&src, src.is_empty(), table, profile)
}

fn load_source(src: &dyn EntrySource, empty: bool, table: &str) -> Result<Option<Configuration>> {
    load_source_with(src, empty, table, LoadProfile::Full)
}

fn load_source_with(
    src: &dyn EntrySource,
    empty: bool,
    table: &str,
    profile: LoadProfile,
) -> Result<Option<Configuration>> {
    if empty {
        return Ok(None);
    }
    let opts = LoadOptions {
        keep_source: false,
        profile,
        ..LoadOptions::default()
    };
    let cfg = Configuration::load(src, &opts).map_err(|source| Error::ConfigTable {
        table: table.to_string(),
        source,
    })?;
    Ok(Some(cfg))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDiff {
    Missing(String),
    Extra(String),
    Differs(String, Vec<u8>, Vec<u8>),
}

impl TableDiff {
    pub fn table(&self) -> &str {
        match self {
            TableDiff::Missing(n) | TableDiff::Extra(n) | TableDiff::Differs(n, ..) => n,
        }
    }
}

fn one_table(t: &Table) -> Vec<u8> {
    LogicalSchema {
        tables: vec![t.clone()],
    }
    .to_text()
}

pub fn schema_diff(ours: &LogicalSchema, actual: &LogicalSchema) -> Vec<TableDiff> {
    let mut out = Vec::new();
    for t in &actual.tables {
        match ours.table(&t.name) {
            None => out.push(TableDiff::Missing(t.name.clone())),
            Some(o) => {
                let (a, b) = (one_table(o), one_table(t));
                if a != b {
                    out.push(TableDiff::Differs(t.name.clone(), a, b));
                }
            }
        }
    }
    for o in &ours.tables {
        if actual.table(&o.name).is_none() {
            out.push(TableDiff::Extra(o.name.clone()));
        }
    }
    out
}

pub fn verify_built(built: &Built, actual: &LogicalSchema) -> Result<()> {
    if !built.allocated.is_empty() {
        const SHOWN: usize = 8;
        let mut list: Vec<String> = built
            .allocated
            .iter()
            .take(SHOWN)
            .map(|n| format!("({}, {})", n.uuid, n.purpose))
            .collect();
        if built.allocated.len() > SHOWN {
            list.push("...".into());
        }
        return Err(Error::Inconsistent(format!(
            "DBNames has no number for {} (uuid, purpose) entries the configuration needs: {}",
            built.allocated.len(),
            list.join(", ")
        )));
    }
    let list: Vec<String> = schema_diff(&built.schema, actual)
        .iter()
        .map(|d| {
            let what = match d {
                TableDiff::Missing(_) => "missing",
                TableDiff::Extra(_) => "extra",
                TableDiff::Differs(..) => "differs",
            };
            format!("{} {what}", d.table())
        })
        .collect();
    if list.is_empty() {
        Ok(())
    } else {
        Err(Error::SchemaMismatch(list))
    }
}

pub fn storage_of(ib: &RawIb, base: &LogicalSchema) -> Result<StorageMap> {
    Ok(storage_and_config(ib, base)?.0)
}

pub fn storage_and_config(
    ib: &RawIb,
    base: &LogicalSchema,
) -> Result<(StorageMap, Option<Configuration>)> {
    let dbschema = Arc::new(LogicalSchema::from_text(&ib.dbschema_text()?)?);
    let Some(cfg) = load_configuration(ib, "CONFIG")? else {
        return Ok((StorageMap::from_schema(dbschema)?, None));
    };
    let mut names = DbNames::parse(&ib.dbnames_text()?)?;
    let built = rules::build(&cfg, &mut names, base, &RulesOptions::default())?;
    verify_built(&built, &dbschema)?;
    Ok((StorageMap::new(dbschema, &built)?, Some(cfg)))
}

pub fn present_service_tables(base: &LogicalSchema, names: &DbNames) -> LogicalSchema {
    LogicalSchema {
        tables: base
            .tables
            .iter()
            .filter(|t| names.number(brace::Guid::NIL, &t.name).is_some())
            .cloned()
            .collect(),
    }
}

pub fn storage_and_config_lenient(
    ib: &RawIb,
    base: &LogicalSchema,
) -> Result<(StorageMap, Option<Configuration>, Vec<String>)> {
    let dbschema = Arc::new(LogicalSchema::from_text(&ib.dbschema_text()?)?);
    let Some(cfg) = load_configuration(ib, "CONFIG")? else {
        return Ok((StorageMap::from_schema(dbschema)?, None, Vec::new()));
    };
    let mut names = DbNames::parse(&ib.dbnames_text()?)?;
    let opts = RulesOptions {
        skip_unsupported: true,
    };
    let base = present_service_tables(base, &names);
    let built = rules::build(&cfg, &mut names, &base, &opts)?;
    let mut notes = check_schema(&built, &dbschema);
    let (map, left_out) = StorageMap::new_lenient(dbschema, &built)?;
    notes.extend(left_out);
    Ok((map, Some(cfg), notes))
}

pub fn check_schema(built: &Built, actual: &LogicalSchema) -> Vec<String> {
    let names: std::collections::HashSet<&str> = built
        .objects
        .iter()
        .flat_map(|o| o.tables.iter())
        .map(|t| t.logical.as_str())
        .filter(|n| !n.contains('.'))
        .collect();
    let pick = |s: &LogicalSchema| LogicalSchema {
        tables: s
            .tables
            .iter()
            .filter(|t| names.contains(t.name.as_str()))
            .cloned()
            .collect(),
    };
    let mut out: Vec<String> = schema_diff(&pick(&built.schema), &pick(actual))
        .iter()
        .map(|d| match d {
            TableDiff::Differs(n, ours, theirs) => {
                format!("{n} differs: {}", first_difference(ours, theirs))
            }
            TableDiff::Extra(n) => format!("{n}: not in DBSCHEMA"),
            TableDiff::Missing(n) => format!("{n}: not in the built schema"),
        })
        .collect();
    out.extend(
        built
            .allocated
            .iter()
            .map(|n| format!("DBNames has no {} for {}", n.purpose, n.uuid)),
    );
    out
}

fn first_difference(ours: &[u8], actual: &[u8]) -> String {
    let at = ours
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .unwrap_or(ours.len().min(actual.len()));
    let from = at.saturating_sub(16);
    let cut = |t: &[u8]| {
        let end = (at + 24).min(t.len());
        String::from_utf8_lossy(t.get(from..end).unwrap_or_default()).replace('\n', " ")
    };
    format!(
        "at byte {at}: built `{}`, actual `{}`",
        cut(ours),
        cut(actual)
    )
}
