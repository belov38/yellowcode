use super::v8time_of;
use crate::raw::managed::{append, column, folder_data_len, partno};
use crate::{Error, Result};
use mdconf::EntryMeta;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use onecd::{ReadTxn, RowId, TableRead, Value};
use std::cmp::Ordering;

fn bad(what: &'static str) -> Error {
    Error::InconsistentStatic(what)
}
fn lower_cmp(first: &str, second: &str) -> Ordering {
    first
        .chars()
        .flat_map(char::to_lowercase)
        .cmp(second.chars().flat_map(char::to_lowercase))
}

#[derive(Clone, Copy)]
struct Part {
    slot: RowId,
    bytes: usize,
}
struct PartRow {
    name: String,
    number: u64,
    part: Part,
    metadata: RowMetadata,
}
struct RowMetadata {
    ctime: Option<v8cont::V8Time>,
    mtime: Option<v8cont::V8Time>,
    attrs: Option<u32>,
    declared: Option<u64>,
    numeric_attributes: bool,
    numeric_size: bool,
}
struct StoredEntry {
    parts: Vec<Part>,
    declared: Option<u64>,
}
struct Staged {
    metadata: EntryMeta,
    stored: StoredEntry,
    first_slot: RowId,
}
struct Index {
    metadata: Vec<EntryMeta>,
    stored: Vec<StoredEntry>,
    by_name: Vec<usize>,
}

pub struct ManagedConfigTxnSource<'a> {
    table: TableRead<'a>,
    index: Charged<Index>,
    budget: &'a MemoryBudget,
    data_column: usize,
}
impl<'a> ManagedConfigTxnSource<'a> {
    pub fn read(txn: &'a ReadTxn, name: &str) -> Result<Self> {
        let budget = txn.memory_budget().ok_or(Error::UnsupportedStatic(
            "managed CONFIG source requires a managed transaction",
        ))?;
        Self::read_in(txn, name, budget)
    }
    pub fn read_in(txn: &'a ReadTxn, name: &str, budget: &'a MemoryBudget) -> Result<Self> {
        if !txn
            .memory_budget()
            .is_some_and(|ledger| ledger.same_instance(budget))
        {
            return Err(Error::UnsupportedStatic(
                "managed CONFIG source requires a transaction of the same ledger",
            ));
        }
        let table = txn
            .find_table(name)?
            .ok_or_else(|| bad("missing CONFIG table"))?;
        let data_column = column(&table, "BINARYDATA")?;
        let rows = collect_rows(&table, budget)?;
        let index = build_index(rows, budget)?;
        Ok(Self {
            table,
            index,
            budget,
            data_column,
        })
    }
    pub fn list(&self) -> &[EntryMeta] {
        &self.index.metadata
    }
    pub fn is_empty(&self) -> bool {
        self.index.metadata.is_empty()
    }
    pub fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>> {
        self.read_data(name, None)
    }
    pub fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Charged<Vec<u8>>>> {
        self.read_data(name, Some(limit))
    }
    fn read_data(&self, name: &str, cap: Option<usize>) -> Result<Option<Charged<Vec<u8>>>> {
        let Ok(at) = self.index.by_name.binary_search_by(|&index| {
            self.index
                .metadata
                .get(index)
                .map(|metadata| metadata.name.as_str())
                .unwrap_or_default()
                .cmp(name)
        }) else {
            return Ok(None);
        };
        let entry = self
            .index
            .by_name
            .get(at)
            .and_then(|&index| self.index.stored.get(index))
            .ok_or_else(|| bad("invalid CONFIG entry index"))?;
        if cap.is_some_and(|limit| entry.declared.is_none_or(|size| size > limit as u64)) {
            return Err(bad("CONFIG entry size exceeds product limit"));
        }
        let total = entry.parts.iter().try_fold(0usize, |total, part| {
            total.checked_add(part.bytes).ok_or(BudgetError::Overflow)
        })?;
        if cap.is_some_and(|limit| total > limit) {
            return Err(bad("CONFIG entry size exceeds product limit"));
        }
        let mut output = BudgetedVec::try_with_capacity(self.budget, Category::Metadata, total)?;
        for part in &entry.parts {
            let row = self
                .table
                .get_managed(part.slot, self.budget, Category::Metadata, u64::MAX)?
                .ok_or_else(|| bad("CONFIG part vanished"))?;
            let value = row.get(self.data_column);
            if folder_data_len(value)? != part.bytes {
                return Err(bad("CONFIG part length changed"));
            }
            append(&self.table, value, &mut output, self.budget, u64::MAX)?;
        }
        Ok(Some(output.into_charged_vec()))
    }
}

fn collect_rows(
    table: &TableRead<'_>,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Charged<PartRow>>> {
    let name_column = column(table, "FILENAME")?;
    let data_column = column(table, "BINARYDATA")?;
    let creation = column(table, "CREATION")?;
    let modified = column(table, "MODIFIED")?;
    let attributes = column(table, "ATTRIBUTES")?;
    let data_size = column(table, "DATASIZE")?;
    let part_column = table.descr().field("PARTNO").map(|(index, _)| index);
    let mut rows = BudgetedVec::new(budget, Category::Metadata)?;
    for slot in 1..table.num_slots() {
        let Some(row) = table.get_managed(slot, budget, Category::Metadata, u64::MAX)? else {
            continue;
        };
        let Some(Value::Char(name) | Value::VarChar(name)) = row.get(name_column) else {
            return Err(bad("CONFIG FILENAME is not text"));
        };
        let number = part_column.map_or(Ok(0), |index| partno(row.get(index)))?;
        let bytes = folder_data_len(row.get(data_column))?;
        let date = |index| match row.get(index) {
            Some(Value::DateTime(date)) => Some(v8time_of(date)),
            _ => None,
        };
        let metadata = RowMetadata {
            ctime: date(creation),
            mtime: date(modified),
            attrs: partno(row.get(attributes))
                .ok()
                .and_then(|number| u32::try_from(number).ok()),
            declared: partno(row.get(data_size)).ok(),
            numeric_attributes: matches!(row.get(attributes), Some(Value::Numeric(_))),
            numeric_size: matches!(row.get(data_size), Some(Value::Numeric(_))),
        };
        let mut copy = BudgetedVec::try_with_capacity(budget, Category::Metadata, name.len())?;
        copy.try_extend_from_slice(name.as_bytes())?;
        let copy = copy
            .into_charged_vec()
            .try_map(String::from_utf8)
            .map_err(|_| bad("CONFIG FILENAME is not UTF-8"))?;
        rows.try_push(copy.map(|name| PartRow {
            name,
            number,
            part: Part { slot, bytes },
            metadata,
        }))?;
    }
    rows.sort_unstable_by(|first, second| {
        lower_cmp(&first.name, &second.name).then_with(|| first.number.cmp(&second.number))
    });
    for pair in rows.windows(2) {
        let [first, second] = pair else { continue };
        if lower_cmp(&first.name, &second.name) == Ordering::Equal {
            if first.name != second.name {
                return Err(bad("CONFIG file names differ only in case"));
            }
            if first.number == second.number {
                return Err(bad("CONFIG repeats PARTNO"));
            }
        }
    }
    let mut previous_name: Option<&str> = None;
    let mut earliest = RowId::MAX;
    for row in rows.iter() {
        if previous_name != Some(row.name.as_str()) {
            previous_name = Some(&row.name);
            earliest = RowId::MAX;
        }
        if row.part.slot < earliest {
            let metadata = &row.metadata;
            if metadata.ctime.is_none()
                || metadata.mtime.is_none()
                || !metadata.numeric_attributes
                || !metadata.numeric_size
            {
                return Err(bad("CONFIG metadata has invalid value types"));
            }
        }
        earliest = earliest.min(row.part.slot);
    }
    Ok(rows)
}

fn build_index(
    mut rows: BudgetedVec<Charged<PartRow>>,
    budget: &MemoryBudget,
) -> Result<Charged<Index>> {
    let groups = usize::from(!rows.is_empty())
        + rows
            .windows(2)
            .filter(|pair| matches!(pair, [first, second] if first.name != second.name))
            .count();
    let mut staged = BudgetedVec::try_with_capacity(budget, Category::Metadata, groups)?;
    while let Some(last) = rows.last() {
        let count = rows
            .iter()
            .rev()
            .take_while(|row| row.name == last.name)
            .count();
        let mut parts = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
        let mut selected = None;
        let mut first_slot = RowId::MAX;
        for _ in 0..count {
            let part = rows.pop().ok_or_else(|| bad("missing CONFIG part index"))?;
            first_slot = first_slot.min(part.part.slot);
            parts.try_push(part.part)?;
            selected = Some(part);
        }
        parts.reverse();
        let selected = selected.ok_or_else(|| bad("empty CONFIG entry group"))?;
        let ctime = selected
            .metadata
            .ctime
            .ok_or_else(|| bad("invalid CONFIG CREATION"))?;
        let mtime = selected
            .metadata
            .mtime
            .ok_or_else(|| bad("invalid CONFIG MODIFIED"))?;
        let attrs = selected
            .metadata
            .attrs
            .ok_or_else(|| bad("invalid CONFIG ATTRIBUTES"))?;
        let declared = selected.metadata.declared;
        let entry = selected
            .try_zip(parts.into_charged_vec())?
            .map(|(selected, parts)| Staged {
                metadata: EntryMeta {
                    name: selected.name,
                    ctime,
                    mtime,
                    attrs,
                },
                stored: StoredEntry { parts, declared },
                first_slot,
            });
        staged.try_push(entry)?;
    }
    staged.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.first_slot));
    let metadata =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, groups)?.into_charged_vec();
    let stored =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, groups)?.into_charged_vec();
    let mut output = metadata.try_zip(stored)?;
    while let Some(entry) = staged.pop() {
        output = output
            .try_zip(entry)?
            .map(|((mut metadata, mut stored), entry)| {
                metadata.push(entry.metadata);
                stored.push(entry.stored);
                (metadata, stored)
            });
    }
    let mut by_name = BudgetedVec::try_with_capacity(budget, Category::Metadata, groups)?;
    for index in 0..groups {
        by_name.try_push(index)?;
    }
    Ok(output
        .try_zip(by_name.into_charged_vec())?
        .map(|((metadata, stored), mut by_name)| {
            by_name.sort_unstable_by(|&first, &second| {
                metadata
                    .get(first)
                    .map(|meta| meta.name.as_str())
                    .cmp(&metadata.get(second).map(|meta| meta.name.as_str()))
            });
            Index {
                metadata,
                stored,
                by_name,
            }
        }))
}

fn recoverable_content_error(error: &Error) -> bool {
    match error {
        Error::Engine(error) => match error {
            onecd::Error::Corrupt { .. } | onecd::Error::CorruptStatic { .. } => true,
            onecd::Error::Io(error) => matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
            ),
            onecd::Error::Memory(_)
            | onecd::Error::Locked { .. }
            | onecd::Error::Unsupported(_)
            | onecd::Error::TxnTooLarge
            | onecd::Error::SnapshotTooOld
            | onecd::Error::Constraint(_)
            | onecd::Error::NotFound(_) => false,
        },
        Error::Inflate(error) => match error {
            zraw::ManagedInflateError::Corrupt
            | zraw::ManagedInflateError::Truncated
            | zraw::ManagedInflateError::Trailing => true,
            zraw::ManagedInflateError::Memory(_)
            | zraw::ManagedInflateError::TooLarge(_)
            | zraw::ManagedInflateError::Zlib(_) => false,
        },
        Error::Memory(_)
        | Error::InconsistentStatic(_)
        | Error::Dt(_)
        | Error::Schema(_)
        | Error::Config(_)
        | Error::ConfigTable { .. }
        | Error::Inconsistent(_)
        | Error::SchemaMismatch(_)
        | Error::Store(_)
        | Error::Unsupported(_)
        | Error::UnsupportedStatic(_) => false,
    }
}

impl mdconf::ManagedEntrySource for ManagedConfigTxnSource<'_> {
    type Error = Error;
    fn recoverable_content_error(&self, error: &Error) -> bool {
        recoverable_content_error(error)
    }
    fn list(&self) -> &[EntryMeta] {
        Self::list(self)
    }
    fn memory_budget(&self) -> &MemoryBudget {
        self.budget
    }
    fn stored(&self, name: &str) -> Result<Option<Charged<Vec<u8>>>> {
        Self::stored(self, name)
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> Result<Option<Charged<Vec<u8>>>> {
        Self::stored_bounded(self, name, limit)
    }
}
