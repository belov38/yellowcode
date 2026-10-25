use std::ops::Bound;
use std::time::Duration;

use crate::fields::FieldValue;
use crate::storage::{FieldId, IndexId, StorageMap, TableId};

pub mod gate;
#[cfg(feature = "testing")]
pub mod mem;
pub use gate::{WriterGate, WriterTicket};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowHandle(pub u64);

pub type Row = Vec<FieldValue>;
pub type RowIter<'a> = Box<dyn Iterator<Item = StoreResult<(RowHandle, Row)>> + 'a>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPosition {
    pub key: Vec<u8>,
    pub row: RowHandle,
}
pub type PositionedRows<'a> = Box<dyn Iterator<Item = StoreResult<(IndexPosition, Row)>> + 'a>;

#[derive(Debug, Clone, PartialEq)]
pub enum Scanned<P, R = Row> {
    Row(P, R),
    Skipped(P),
}

impl<P, R> Scanned<P, R> {
    pub fn position(&self) -> &P {
        match self {
            Self::Row(p, _) | Self::Skipped(p) => p,
        }
    }
}

pub type ScannedRows<'a> = Box<dyn Iterator<Item = StoreResult<Scanned<RowHandle>>> + 'a>;
pub type ScannedPositioned<'a> = Box<dyn Iterator<Item = StoreResult<Scanned<IndexPosition>>> + 'a>;

pub type RecordFilter<'f> = &'f [(FieldId, bool)];

fn record_passes(
    map: Option<&StorageMap>,
    t: TableId,
    filter: RecordFilter<'_>,
    row: &Row,
) -> StoreResult<bool> {
    for (f, want) in filter {
        match row.get(usize::from(f.0)) {
            Some(FieldValue::Bool(b)) => {
                if b != want {
                    return Ok(false);
                }
            }
            _ => {
                let ti = map.and_then(|m| m.try_table(t));
                return Err(StoreError::BadValue {
                    table: ti.map(|t| t.logical.clone()).unwrap_or_default(),
                    field: ti
                        .and_then(|t| t.field(*f))
                        .map(|f| f.name.clone())
                        .unwrap_or_default(),
                    row: 0,
                    why: "the record filter value is not a boolean".into(),
                });
            }
        }
    }
    Ok(true)
}

fn with_filter_fields(fields: &[FieldId], filter: RecordFilter<'_>) -> Vec<FieldId> {
    let mut all = fields.to_vec();
    all.extend(filter.iter().map(|(f, _)| *f));
    all
}

fn bad_value_at(e: StoreError, at: u64) -> StoreError {
    match e {
        StoreError::BadValue {
            table,
            field,
            row: 0,
            why,
        } => StoreError::BadValue {
            table,
            field,
            row: at,
            why,
        },
        other => other,
    }
}

#[derive(Debug, Clone)]
pub struct IndexAccess {
    pub id: IndexId,
    pub name: String,
    pub key_len: usize,
    pub fields: Vec<(FieldId, Option<String>)>,
    pub ref_pairs: Vec<FieldId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

pub trait ReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

pub trait Store: Send + Sync {
    fn storage(&self) -> &StorageMap;
    fn read(&self) -> StoreResult<Box<dyn ReadTxn + Send>>;
    fn write(&self, wait: Duration) -> StoreResult<Box<dyn WriteTxn>>;
}

pub trait ReadTxn {
    fn scan(&self, t: TableId) -> StoreResult<RowIter<'_>>;
    fn scan_after(&self, t: TableId, after: RowHandle) -> StoreResult<RowIter<'_>> {
        Ok(Box::new(self.scan(t)?.filter(move |row| match row {
            Ok((handle, _)) => *handle > after,
            Err(_) => true,
        })))
    }
    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
    ) -> StoreResult<RowIter<'_>>;
    fn indexes(&self, _t: TableId) -> StoreResult<Vec<IndexAccess>> {
        Err(StoreError::Unsupported(
            "positioned indexes unavailable".into(),
        ))
    }
    fn range_after(
        &self,
        _t: TableId,
        _i: IndexId,
        _lo: Bound<&[FieldValue]>,
        _hi: Bound<&[FieldValue]>,
        _dir: Direction,
        _after: Option<&IndexPosition>,
    ) -> StoreResult<PositionedRows<'_>> {
        Err(StoreError::Unsupported(
            "positioned ranges unavailable".into(),
        ))
    }
    fn get(&self, t: TableId, h: RowHandle) -> StoreResult<Row>;
    fn set_blob_prefix(&self, max_bytes: Option<usize>) {
        let _ = max_bytes;
    }
    fn take_blob_cuts(&self) -> Vec<(FieldId, u64)> {
        Vec::new()
    }
    fn scan_fields(
        &self,
        t: TableId,
        fields: &[FieldId],
        after: Option<RowHandle>,
    ) -> StoreResult<RowIter<'_>> {
        let _ = fields;
        match after {
            None => self.scan(t),
            Some(h) => self.scan_after(t, h),
        }
    }
    fn range_fields(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        fields: &[FieldId],
    ) -> StoreResult<RowIter<'_>> {
        let _ = fields;
        self.range(t, i, lo, hi, dir)
    }
    fn get_fields(&self, t: TableId, h: RowHandle, fields: &[FieldId]) -> StoreResult<Row> {
        let _ = fields;
        self.get(t, h)
    }
    #[allow(clippy::too_many_arguments, reason = "range_after and its projection")]
    fn range_after_fields(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
        fields: &[FieldId],
    ) -> StoreResult<PositionedRows<'_>> {
        let _ = fields;
        self.range_after(t, i, lo, hi, dir, after)
    }
    fn next_position_after(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
    ) -> StoreResult<Option<IndexPosition>> {
        match self.range_after(t, i, lo, hi, dir, after)?.next() {
            None => Ok(None),
            Some(row) => row.map(|(p, _)| Some(p)),
        }
    }
    fn next_used_after(&self, t: TableId, after: RowHandle) -> StoreResult<Option<RowHandle>> {
        match self.scan_after(t, after)?.next() {
            None => Ok(None),
            Some(row) => row.map(|(h, _)| Some(h)),
        }
    }
    fn open_blob(
        &self,
        t: TableId,
        h: RowHandle,
        f: FieldId,
    ) -> StoreResult<Box<dyn ReadSeek + '_>>;
    fn row_count_estimate(&self, t: TableId) -> StoreResult<u64>;
    fn storage_map(&self) -> Option<&StorageMap> {
        None
    }
    fn scan_fields_filtered(
        &self,
        t: TableId,
        fields: &[FieldId],
        after: RowHandle,
        filter: RecordFilter<'_>,
        end: RowHandle,
    ) -> StoreResult<ScannedRows<'_>> {
        let all = with_filter_fields(fields, filter);
        let rows = self.scan_fields(t, &all, Some(after))?;
        let filter = filter.to_vec();
        let map = self.storage_map();
        Ok(Box::new(
            rows.take_while(move |r| !matches!(r, Ok((h, _)) if *h >= end))
                .map(move |r| {
                    let (h, row) = r?;
                    record_passes(map, t, &filter, &row)
                        .map_err(|e| bad_value_at(e, h.0))
                        .map(|pass| {
                            if pass {
                                Scanned::Row(h, row)
                            } else {
                                Scanned::Skipped(h)
                            }
                        })
                }),
        ))
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "range_after_fields and its filter"
    )]
    fn range_after_fields_filtered(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
        fields: &[FieldId],
        filter: RecordFilter<'_>,
    ) -> StoreResult<ScannedPositioned<'_>> {
        let all = with_filter_fields(fields, filter);
        let rows = self.range_after_fields(t, i, lo, hi, dir, after, &all)?;
        let filter = filter.to_vec();
        let map = self.storage_map();
        Ok(Box::new(rows.map(move |r| {
            let (p, row) = r?;
            record_passes(map, t, &filter, &row)
                .map_err(|e| bad_value_at(e, p.row.0))
                .map(|pass| {
                    if pass {
                        Scanned::Row(p, row)
                    } else {
                        Scanned::Skipped(p)
                    }
                })
        })))
    }
    fn scan_end(&self, t: TableId) -> StoreResult<RowHandle> {
        let mut end = RowHandle(1);
        for r in self.scan(t)? {
            end = RowHandle(r?.0.0.saturating_add(1).max(end.0));
        }
        Ok(end)
    }
}

pub trait WriteTxn: ReadTxn {
    fn insert(&mut self, t: TableId, row: &Row) -> StoreResult<RowHandle>;
    fn update(&mut self, t: TableId, h: RowHandle, row: &Row) -> StoreResult<()>;
    fn delete(&mut self, t: TableId, h: RowHandle) -> StoreResult<()>;
    fn commit(self: Box<Self>) -> StoreResult<()>;
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("unique violation in {table_name}.{index_name}")]
    UniqueViolation {
        table: TableId,
        index: IndexId,
        table_name: String,
        index_name: String,
    },
    #[error("writer busy")]
    WriterBusy,
    #[error("null not allowed in {table}.{field}")]
    NullNotAllowed { table: String, field: String },
    #[error("value does not fit {table}.{field}: {why}")]
    ValueDoesNotFit {
        table: String,
        field: String,
        why: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("snapshot too old")]
    SnapshotTooOld,
    #[error("transaction too large")]
    TxnTooLarge,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("bad value in {table}.{field} (row {row}): {why}")]
    BadValue {
        table: String,
        field: String,
        row: u64,
        why: String,
    },
    #[error("storage: {0}")]
    Backend(Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Memory(memory_budget::MemoryLimit),
}

impl StoreError {
    pub fn from_schema(e: crate::Error) -> StoreError {
        match e {
            crate::Error::ValueDoesNotFit { table, column, why } => {
                if why == crate::fields::NULL_NOT_ALLOWED {
                    StoreError::NullNotAllowed {
                        table,
                        field: column,
                    }
                } else {
                    StoreError::ValueDoesNotFit {
                        table,
                        field: column,
                        why,
                    }
                }
            }
            crate::Error::BadValue { table, field, why } => StoreError::BadValue {
                table,
                field,
                row: 0,
                why,
            },
            crate::Error::Unsupported(s) => StoreError::Unsupported(s),
            other => StoreError::Backend(Box::new(other)),
        }
    }
}
