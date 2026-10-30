use super::{EntryBacking, EntryMeta, EntrySource, Inflated, StoredBytes, bad, memory_error};
use crate::{Error, Result};
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};
use std::fs::File;
use std::path::Path;
use v8cont::{EntryInfo, ManagedContainer, ManagedReadError};

const CATEGORY: Category = Category::Metadata;

pub struct ManagedCfFile<'a> {
    container: ManagedContainer<'a, File>,
    order: Charged<Vec<u32>>,
    budget: &'a MemoryBudget,
}

fn name_at(entries: &[EntryInfo], index: u32) -> &str {
    entries
        .get(index as usize)
        .map_or("", |entry| entry.name.as_str())
}

fn read_error(entry: &str, error: ManagedReadError) -> Error {
    match error {
        ManagedReadError::Memory(error) => memory_error(entry, error),
        ManagedReadError::Io(error) => Error::Io(error),
        other => bad(entry, &other.to_string()),
    }
}

impl<'a> ManagedCfFile<'a> {
    pub fn open(path: &Path, budget: &'a MemoryBudget) -> Result<Self> {
        let container = ManagedContainer::open(File::open(path)?, budget, CATEGORY)
            .map_err(|error| read_error("<container>", error))?;
        let entries = container.entries();
        let count =
            u32::try_from(entries.len()).map_err(|_| bad("<container>", "too many entries"))?;
        let mut order = BudgetedVec::try_with_capacity(budget, CATEGORY, entries.len())
            .map_err(|error| memory_error("<container>", error))?;
        for index in 0..count {
            order
                .try_push(index)
                .map_err(|error| memory_error("<container>", error))?;
        }
        order.sort_unstable_by(|a, b| {
            name_at(entries, *a)
                .cmp(name_at(entries, *b))
                .then(a.cmp(b))
        });
        Ok(Self {
            order: order.into_charged_vec(),
            container,
            budget,
        })
    }

    fn find(&self, name: &str) -> Option<&EntryInfo> {
        let entries = self.container.entries();
        let at = self.order.partition_point(|&i| name_at(entries, i) < name);
        let index = *self.order.get(at)? as usize;
        entries.get(index).filter(|entry| entry.name == name)
    }

    fn read(&self, entry: &EntryInfo, limit: u64) -> Result<Charged<Vec<u8>>> {
        self.container
            .read_data(entry, limit)
            .map_err(|error| read_error(&entry.name, error))
    }
}

impl EntrySource for ManagedCfFile<'_> {
    fn list(&self) -> Result<Vec<EntryMeta>> {
        Ok(self
            .container
            .entries()
            .iter()
            .map(|e| EntryMeta {
                name: e.name.clone(),
                ctime: e.ctime,
                mtime: e.mtime,
                attrs: e.attrs,
            })
            .collect())
    }
    fn stored(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Err(bad(name, "a managed .cf reads charged entries only"))
    }
    fn stored_owned(&self, name: &str) -> Result<Option<StoredBytes>> {
        self.stored_owned_bounded(name, usize::MAX)
    }
    fn stored_owned_bounded(&self, name: &str, limit: usize) -> Result<Option<StoredBytes>> {
        let Some(entry) = self.find(name) else {
            return Ok(None);
        };
        if entry.data_len > limit as u64 {
            return Err(bad(name, "stored size exceeds product limit"));
        }
        let stored = self.read(entry, limit as u64)?;
        Ok(Some(EntryBacking::Native(stored.to_vec())))
    }
    fn stored_len(&self, name: &str) -> Result<Option<u64>> {
        Ok(self.find(name).map(|entry| entry.data_len))
    }
    fn stored_inflated_bounded(&self, name: &str, limit: u64) -> Result<Option<Inflated>> {
        self.stored_inflated_checked(name, limit, &|| Ok(()))
    }
    fn stored_inflated_checked(
        &self,
        name: &str,
        limit: u64,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Inflated>> {
        check()?;
        let limit = limit.min(zraw::DEFAULT_LIMIT);
        let Some(entry) = self.find(name) else {
            return Ok(None);
        };
        if entry.data_len > limit {
            return Err(bad(name, "stored size exceeds product limit"));
        }
        let stored = self.read(entry, limit)?;
        check()?;
        let inflated =
            match zraw::inflate_budgeted_checked(&stored, limit, self.budget, CATEGORY, || {
                check().is_ok()
            }) {
                Ok(None) => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "opening cancelled",
                    )));
                }
                Ok(Some(bytes)) => Inflated {
                    bytes,
                    was_deflated: true,
                },
                Err(zraw::ManagedInflateError::Memory(error)) => {
                    return Err(memory_error(name, error));
                }
                Err(_) => Inflated {
                    bytes: stored,
                    was_deflated: false,
                },
            };
        Ok(Some(inflated))
    }
}
