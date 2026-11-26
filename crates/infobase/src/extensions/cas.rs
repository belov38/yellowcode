use crate::config::ManagedConfigTxnSource;
use mdconf::configinfo::ConfigInfo;
use mdconf::{EntryBacking, EntryMeta, EntrySource, StoredBytes};
use memory_budget::MemoryBudget;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use v8cont::V8Time;

pub(super) struct CasSource<'a> {
    folder: ManagedConfigTxnSource<'a>,
    rows: HashMap<String, (String, V8Time, V8Time, u32)>,
    manifest: String,
    entries: RefCell<HashMap<String, String>>,
}

fn hex(bytes: &[u8; 20]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn invalid(what: impl std::fmt::Display) -> mdconf::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string()).into()
}

fn to_mdconf(error: crate::Error) -> mdconf::Error {
    match error {
        crate::Error::Config(error) => error,
        crate::Error::Memory(error)
        | crate::Error::Inflate(zraw::ManagedInflateError::Memory(error))
        | crate::Error::Engine(onecd::Error::Memory(error)) => charged_error(error),
        other => invalid(other),
    }
}

fn charged_error(error: memory_budget::BudgetError) -> mdconf::Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => mdconf::Error::Memory(limit),
        other => mdconf::Error::Io(io::Error::new(io::ErrorKind::OutOfMemory, other)),
    }
}

impl<'a> CasSource<'a> {
    pub(super) fn open(
        txn: &'a onecd::ReadTxn,
        budget: &'a MemoryBudget,
        generation: [u8; 20],
    ) -> crate::Result<Self> {
        let folder = ManagedConfigTxnSource::read_in(txn, "CONFIGCAS", budget)?;
        let rows = folder
            .list()
            .iter()
            .map(|e| {
                (
                    e.name.to_ascii_lowercase(),
                    (e.name.clone(), e.ctime, e.mtime, e.attrs),
                )
            })
            .collect();
        Ok(Self {
            folder,
            rows,
            manifest: hex(&generation),
            entries: RefCell::new(HashMap::new()),
        })
    }

    pub(super) fn prepare(&self, manifest: &ConfigInfo) {
        *self.entries.borrow_mut() = manifest
            .entries
            .iter()
            .map(|e| (e.name.to_ascii_lowercase(), hex(&e.stored_sha1)))
            .collect();
    }

    fn row(&self, name: &str) -> Option<(String, V8Time, V8Time, u32)> {
        let key = if name == "configinfo" {
            self.manifest.clone()
        } else {
            self.entries
                .borrow()
                .get(&name.to_ascii_lowercase())?
                .clone()
        };
        self.rows.get(&key).cloned()
    }

    pub(super) fn meta(&self, name: &str) -> Option<(V8Time, V8Time, u32)> {
        self.row(name)
            .map(|(_, ctime, mtime, attrs)| (ctime, mtime, attrs))
    }
}

impl EntrySource for CasSource<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Err(io::Error::from(io::ErrorKind::Unsupported).into())
    }
    fn stored(&self, _: &str) -> mdconf::Result<Option<Vec<u8>>> {
        Err(io::Error::from(io::ErrorKind::Unsupported).into())
    }
    fn stored_len(&self, name: &str) -> mdconf::Result<Option<u64>> {
        let Some((row, ..)) = self.row(name) else {
            return Ok(None);
        };
        self.folder
            .indexed_len(&row)
            .map(|len| len.map(|len| len as u64))
            .map_err(invalid)
    }
    fn stored_owned_bounded(
        &self,
        name: &str,
        limit: usize,
    ) -> mdconf::Result<Option<StoredBytes>> {
        let Some((row, ..)) = self.row(name) else {
            return Ok(None);
        };
        self.folder
            .stored_bounded(&row, limit)
            .map_err(to_mdconf)?
            .map(EntryBacking::from_charged)
            .transpose()
            .map_err(charged_error)
    }
}
