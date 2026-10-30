use crate::{AppError, Result};
use ibschema::fields::{FieldValue, encode};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use ibschema::store::{
    Direction, ReadTxn, Row, RowHandle, RowIter, Store, StoreError, StoreResult, WriteTxn,
};
use ibschema::values::Uuid16;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct ReadOnlyScope {
    values: Vec<(Uuid16, FieldValue)>,
}

fn invalid() -> AppError {
    AppError::Unsupported("P1ro: incomplete or invalid read-only separator scope".into())
}

impl ReadOnlyScope {
    pub fn new(values: Vec<(Uuid16, FieldValue)>) -> Result<Self> {
        for (i, (id, value)) in values.iter().enumerate() {
            if *id == Uuid16::NIL
                || values.iter().take(i).any(|(other, _)| other == id)
                || !matches!(
                    value,
                    FieldValue::Bool(_)
                        | FieldValue::Number(_)
                        | FieldValue::String(_)
                        | FieldValue::Date(_)
                        | FieldValue::Uuid(_)
                        | FieldValue::Ref { .. }
                )
            {
                return Err(invalid());
            }
        }
        Ok(Self { values })
    }

    pub(crate) fn area(&self, table: &TableInfo) -> Result<Vec<FieldValue>> {
        table
            .separators
            .iter()
            .map(|s| {
                let value = self
                    .values
                    .iter()
                    .find(|(id, _)| *id == s.common_attribute)
                    .map(|(_, value)| value)
                    .ok_or_else(invalid)?;
                encode(table, s.field, value).map_err(|_| invalid())?;
                Ok(value.clone())
            })
            .collect()
    }

    pub(crate) fn bind(&self, inner: Arc<dyn Store>) -> Result<Arc<dyn Store>> {
        let mut tables = HashMap::new();
        for (i, table) in inner.storage().tables().iter().enumerate() {
            let values = self.area(table)?;
            if !values.is_empty() {
                let id = TableId(u32::try_from(i).map_err(|_| invalid())?);
                tables.insert(
                    id,
                    table
                        .separators
                        .iter()
                        .map(|s| s.field)
                        .zip(values)
                        .collect(),
                );
            }
        }
        if self.values.iter().any(|(id, _)| {
            !inner
                .storage()
                .tables()
                .iter()
                .any(|t| t.separators.iter().any(|s| s.common_attribute == *id))
        }) {
            return Err(invalid());
        }
        Ok(Arc::new(ScopedStore {
            inner,
            tables: Arc::new(tables),
        }))
    }
}

type TableScopes = HashMap<TableId, Vec<(FieldId, FieldValue)>>;

struct ScopedStore {
    inner: Arc<dyn Store>,
    tables: Arc<TableScopes>,
}

impl Store for ScopedStore {
    fn storage(&self) -> &StorageMap {
        self.inner.storage()
    }
    fn read(&self) -> StoreResult<Box<dyn ReadTxn + Send>> {
        Ok(Box::new(ScopedTxn {
            inner: self.inner.read()?,
            tables: self.tables.clone(),
        }))
    }
    fn write(&self, _: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        Err(StoreError::Unsupported(
            "P1ro: read-only application".into(),
        ))
    }
}

struct ScopedTxn {
    inner: Box<dyn ReadTxn + Send>,
    tables: Arc<TableScopes>,
}

impl ScopedTxn {
    fn contains(&self, table: TableId, row: &Row) -> bool {
        self.tables.get(&table).is_none_or(|fields| {
            fields.iter().all(
                |(field, value)| match (row.get(usize::from(field.0)), value) {
                    (Some(FieldValue::Number(stored)), FieldValue::Number(area)) => {
                        stored.eq_value(area)
                    }
                    (Some(stored), area) => stored == area,
                    _ => false,
                },
            )
        })
    }

    fn filter<'a>(&'a self, table: TableId, rows: RowIter<'a>) -> RowIter<'a> {
        Box::new(rows.filter(move |item| match item {
            Ok((_, row)) => self.contains(table, row),
            Err(_) => true,
        }))
    }
}

impl ReadTxn for ScopedTxn {
    fn read_memory(&mut self, limit: u64) -> StoreResult<memory_budget::MemoryBudget> {
        self.inner.read_memory(limit)
    }
    fn scan(&self, table: TableId) -> StoreResult<RowIter<'_>> {
        Ok(self.filter(table, self.inner.scan(table)?))
    }
    fn range(
        &self,
        table: TableId,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        direction: Direction,
    ) -> StoreResult<RowIter<'_>> {
        Ok(self.filter(table, self.inner.range(table, index, lo, hi, direction)?))
    }
    fn get(&self, table: TableId, handle: RowHandle) -> StoreResult<Row> {
        let row = self.inner.get(table, handle)?;
        if self.contains(table, &row) {
            Ok(row)
        } else {
            Err(StoreError::Unsupported(
                "P1ro: row outside read-only scope".into(),
            ))
        }
    }
    fn open_blob(
        &self,
        table: TableId,
        handle: RowHandle,
        field: FieldId,
    ) -> StoreResult<Box<dyn ibschema::store::ReadSeek + '_>> {
        self.get(table, handle)?;
        self.inner.open_blob(table, handle, field)
    }
    fn row_count_estimate(&self, table: TableId) -> StoreResult<u64> {
        if !self.tables.contains_key(&table) {
            return self.inner.row_count_estimate(table);
        }
        self.scan(table)?
            .try_fold(0u64, |n, row| row.map(|_| n.saturating_add(1)))
    }
}
