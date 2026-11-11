use crate::{AppError, Result};
use ibschema::Numeric;
use ibschema::fields::{FieldValue, encode};
use ibschema::logical::{LType, TypeCode};
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

fn no_zero(why: String) -> AppError {
    AppError::Unsupported(format!("P1ro: no zero read-only scope: {why}"))
}

fn type_text(types: &[LType]) -> String {
    if types.is_empty() {
        return "no type".into();
    }
    types
        .iter()
        .map(|t| format!("{:?}({},{})", t.code, t.length, t.precision))
        .collect::<Vec<_>>()
        .join(" | ")
}

struct Found<'a> {
    id: Uuid16,
    table: &'a str,
    field: &'a str,
    types: &'a [LType],
}

fn zero_area<'a>(found: impl IntoIterator<Item = Found<'a>>) -> Result<Vec<(Uuid16, FieldValue)>> {
    let mut values: Vec<(Uuid16, FieldValue)> = Vec::new();
    for f in found {
        if f.id == Uuid16::NIL {
            return Err(no_zero(format!(
                "the separator field {} of table {} has no common attribute",
                f.field, f.table
            )));
        }
        if !matches!(f.types, [t] if t.code == TypeCode::N) {
            return Err(no_zero(format!(
                "separator {} (field {} of table {}) has type {}; only a single number type is supported",
                f.id,
                f.field,
                f.table,
                type_text(f.types)
            )));
        }
        if !values.iter().any(|(id, _)| *id == f.id) {
            values.push((f.id, FieldValue::Number(Numeric::from_i64(0))));
        }
    }
    values.sort_by_key(|(id, _)| *id);
    Ok(values)
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

    pub fn zero_from_schema(map: &StorageMap) -> Result<Self> {
        let mut found = Vec::new();
        for table in map.tables() {
            for separator in &table.separators {
                let field = table.field(separator.field).ok_or_else(|| {
                    no_zero(format!(
                        "table {} has a separator on the missing field {}",
                        table.logical, separator.field.0
                    ))
                })?;
                found.push(Found {
                    id: separator.common_attribute,
                    table: &table.logical,
                    field: &field.name,
                    types: &field.types,
                });
            }
        }
        Self::new(zero_area(found)?)
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
        self.bind_with(inner, false)
    }

    fn bind_with(&self, inner: Arc<dyn Store>, writable: bool) -> Result<Arc<dyn Store>> {
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
            writable,
        }))
    }
}

#[derive(Clone)]
pub struct DataArea {
    scope: ReadOnlyScope,
}

impl DataArea {
    pub fn new(values: Vec<(Uuid16, FieldValue)>) -> Result<Self> {
        Ok(Self {
            scope: ReadOnlyScope::new(values)?,
        })
    }

    pub fn zero_from_schema(map: &StorageMap) -> Result<Self> {
        Ok(Self {
            scope: ReadOnlyScope::zero_from_schema(map)?,
        })
    }

    pub(crate) fn scope(&self) -> &ReadOnlyScope {
        &self.scope
    }

    pub(crate) fn bind(&self, inner: Arc<dyn Store>) -> Result<Arc<dyn Store>> {
        self.scope.bind_with(inner, true)
    }
}

type TableScopes = HashMap<TableId, Vec<(FieldId, FieldValue)>>;

struct ScopedStore {
    inner: Arc<dyn Store>,
    tables: Arc<TableScopes>,
    writable: bool,
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
    fn write(&self, wait: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        if !self.writable {
            return Err(StoreError::Unsupported(
                "P1ro: read-only application".into(),
            ));
        }
        Ok(Box::new(ScopedTxn {
            inner: self.inner.write(wait)?,
            tables: self.tables.clone(),
        }))
    }
}

struct ScopedTxn<T: ?Sized> {
    inner: Box<T>,
    tables: Arc<TableScopes>,
}

impl<T: ReadTxn + ?Sized> ScopedTxn<T> {
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

impl<T: ReadTxn + ?Sized> ReadTxn for ScopedTxn<T> {
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

impl ScopedTxn<dyn WriteTxn> {
    fn stamped(&self, table: TableId, row: &Row) -> Row {
        let mut row = row.clone();
        for (field, value) in self.tables.get(&table).into_iter().flatten() {
            if let Some(slot) = row.get_mut(usize::from(field.0)) {
                *slot = value.clone();
            }
        }
        row
    }

    fn own(&self, table: TableId, handle: RowHandle) -> StoreResult<()> {
        let row = self.inner.get(table, handle)?;
        if self.contains(table, &row) {
            Ok(())
        } else {
            Err(StoreError::Unsupported(
                "P1ro: row outside the data area".into(),
            ))
        }
    }
}

impl WriteTxn for ScopedTxn<dyn WriteTxn> {
    fn insert(&mut self, table: TableId, row: &Row) -> StoreResult<RowHandle> {
        let row = self.stamped(table, row);
        self.inner.insert(table, &row)
    }
    fn update(&mut self, table: TableId, handle: RowHandle, row: &Row) -> StoreResult<()> {
        self.own(table, handle)?;
        let row = self.stamped(table, row);
        self.inner.update(table, handle, &row)
    }
    fn delete(&mut self, table: TableId, handle: RowHandle) -> StoreResult<()> {
        self.own(table, handle)?;
        self.inner.delete(table, handle)
    }
    fn commit(self: Box<Self>) -> StoreResult<()> {
        self.inner.commit()
    }
}
