use crate::TestBase;
use ibschema::columns::{ColRole, ColType};
use ibschema::fields::{FieldValue, single_ref_table};
use ibschema::names::StdField;
use ibschema::storage::IndexId;
use ibschema::storage::{FieldId, TableId, TableInfo};
use ibschema::store::{
    Direction, IndexAccess, IndexPosition, ReadTxn, Row, RowHandle, Scanned, StoreError,
};
use query_ir::schema::{FieldRole, Only, QField, StdRole, TableKind};
use query_ir::{QFieldId, QTableId, QValue};
use std::ops::Bound;

pub type StoredRows<'a> = Box<dyn Iterator<Item = Result<(u64, Vec<QValue>), String>> + 'a>;
pub type StoredTypedRows<'a> =
    Box<dyn Iterator<Item = Result<(u64, Vec<QValue>), StoreError>> + 'a>;

pub struct StoredTable<'a> {
    read: Box<dyn ReadTxn + Send>,
    id: TableId,
    info: &'a TableInfo,
    positions: Vec<usize>,
    reads: Vec<ibschema::storage::FieldId>,
    pub fields: Vec<&'a QField>,
}
impl TestBase {
    pub fn stored_table(&self, table: QTableId) -> Result<Option<StoredTable<'_>>, String> {
        let schema = self.schema.table(table);
        if !matches!(schema.kind, TableKind::Object(_) | TableKind::Tabular) {
            return Ok(None);
        }
        let Some(id) = self.storage.table(table) else {
            return Ok(None);
        };
        let info = self
            .map
            .try_table(id)
            .ok_or("table has no storage information")?;
        let mut fields = Vec::new();
        let mut positions = Vec::new();
        for (index, field) in schema.fields.iter().enumerate() {
            if matches!(
                field.role,
                FieldRole::Std(
                    StdRole::Predefined | StdRole::PredefinedDataName | StdRole::RecordKind
                )
            ) {
                continue;
            }
            let index = u16::try_from(index).map_err(|_| "too many fields")?;
            if let Some(stored) = self.storage.field(table, QFieldId(index)) {
                fields.push(field);
                positions.push(usize::from(stored.0));
            }
        }
        let read = self.store.read().map_err(|e| e.to_string())?;
        let mut reads: Vec<ibschema::storage::FieldId> = positions
            .iter()
            .filter_map(|&p| u16::try_from(p).ok().map(ibschema::storage::FieldId))
            .collect();
        reads.extend(info.field_by_std(StdField::Folder));
        Ok(Some(StoredTable {
            read,
            id,
            info,
            positions,
            reads,
            fields,
        }))
    }
}
impl StoredTable<'_> {
    pub fn scan_after(&self, after: u64) -> Result<StoredRows<'_>, String> {
        let rows = self.scan_after_typed(after).map_err(|e| e.to_string())?;
        Ok(Box::new(rows.map(|row| row.map_err(|e| e.to_string()))))
    }

    pub fn scan_after_typed(&self, after: u64) -> Result<StoredTypedRows<'_>, StoreError> {
        let rows = self
            .read
            .scan_fields(self.id, &self.reads, Some(RowHandle(after)))?;
        Ok(Box::new(rows.map(|row| {
            let (handle, row) = row?;
            let projected = self.project(&row).map_err(value_error)?;
            Ok((handle.0, projected))
        })))
    }

    pub fn set_blob_prefix(&self, max_bytes: Option<usize>) {
        self.read.set_blob_prefix(max_bytes);
    }

    pub fn take_blob_cuts(&self) -> Vec<(usize, u64)> {
        self.read
            .take_blob_cuts()
            .into_iter()
            .filter_map(|(id, len)| {
                let at = self
                    .positions
                    .iter()
                    .position(|p| *p == usize::from(id.0))?;
                Some((at, len))
            })
            .collect()
    }

    pub fn scan_after_filtered(
        &self,
        after: u64,
        filter: &[(FieldId, bool)],
        end: u64,
    ) -> Result<StoredScanned<'_, u64>, StoreError> {
        let rows = self.read.scan_fields_filtered(
            self.id,
            &self.reads,
            RowHandle(after),
            filter,
            RowHandle(end),
        )?;
        Ok(Box::new(rows.map(|item| {
            Ok(match item? {
                Scanned::Row(h, row) => Scanned::Row(h.0, self.project(&row).map_err(value_error)?),
                Scanned::Skipped(h) => Scanned::Skipped(h.0),
            })
        })))
    }

    pub fn scan_end(&self) -> Result<u64, StoreError> {
        Ok(self.read.scan_end(self.id)?.0)
    }

    pub fn record_filter_field(&self, role: StdRole) -> RecordField {
        let Some(at) = self
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
        else {
            return RecordField::Absent;
        };
        let (Some(field), Some(&position)) = (self.fields.get(at), self.positions.get(at)) else {
            return RecordField::Absent;
        };
        let Ok(id) = u16::try_from(position).map(FieldId) else {
            return RecordField::Absent;
        };
        let columns = self
            .info
            .field(id)
            .and_then(|f| self.info.columns.get(f.columns.clone()));
        record_field_kind(id, field.only, columns)
    }

    pub fn next_used_after(&self, after: u64) -> Result<Option<u64>, StoreError> {
        Ok(self
            .read
            .next_used_after(self.id, RowHandle(after))?
            .map(|h| h.0))
    }
}

fn value_error(e: String) -> StoreError {
    StoreError::Backend(e.into())
}

pub type StoredScanned<'a, P> =
    Box<dyn Iterator<Item = Result<Scanned<P, Vec<QValue>>, StoreError>> + 'a>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordField {
    Field(FieldId),
    Absent,
    NotForEveryRow,
    NotBoolean,
}

pub fn record_field_kind(
    id: FieldId,
    only: Only,
    columns: Option<&[ibschema::columns::Column]>,
) -> RecordField {
    if only != Only::Any {
        return RecordField::NotForEveryRow;
    }
    match columns {
        Some([c]) if c.ty == ColType::L && !c.nullable => RecordField::Field(id),
        _ => RecordField::NotBoolean,
    }
}

pub type StoredTypedIndexRows<'a> =
    Box<dyn Iterator<Item = Result<(IndexPosition, Vec<QValue>), StoreError>> + 'a>;

pub type StoredIndexRows<'a> =
    Box<dyn Iterator<Item = Result<(IndexPosition, Vec<QValue>), String>> + 'a>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionKind {
    Number,
    Date,
    String,
    Ref { table: u32 },
}
impl StoredTable<'_> {
    pub fn indexes(&self) -> Result<Vec<IndexAccess>, String> {
        let mut indexes = self.read.indexes(self.id).map_err(|e| e.to_string())?;
        for ix in &mut indexes {
            for (id, reason) in &mut ix.fields {
                if let Some(p) = self.positions.iter().position(|p| *p == usize::from(id.0))
                    && self
                        .fields
                        .get(p)
                        .is_some_and(|f| matches!(f.only, Only::Folders | Only::Items))
                {
                    *reason = Some("field is applicable only to folders or items; logical NULLs require filtering".into());
                }
            }
        }
        Ok(indexes)
    }
    pub fn index_field_name(&self, id: ibschema::storage::FieldId) -> Option<&str> {
        let mapped = self
            .positions
            .iter()
            .position(|p| *p == usize::from(id.0))
            .and_then(|p| self.fields.get(p))
            .map(|f| f.name_ru.as_str());
        mapped.or_else(|| {
            if self.info.owner.is_none() || !self.info.separators.iter().any(|s| s.field == id) {
                return None;
            }
            self.info
                .field(id)?
                .md_name
                .strip_prefix("CommonAttribute.")
                .filter(|n| !n.is_empty() && !n.contains('.'))
        })
    }
    pub fn is_separator(&self, id: ibschema::storage::FieldId) -> bool {
        self.info.separators.iter().any(|s| s.field == id)
    }
    pub fn condition_kind(&self, id: ibschema::storage::FieldId) -> Option<ConditionKind> {
        let field = self.info.field(id)?;
        match self.info.columns.get(field.columns.clone())? {
            [c] => match (c.ty, c.role) {
                (ColType::N(_, _), _) => Some(ConditionKind::Number),
                (ColType::DT, _) => Some(ConditionKind::Date),
                (ColType::NC(_) | ColType::NVC(_), ColRole::Str) => Some(ConditionKind::String),
                (ColType::B(16), ColRole::Ref) => {
                    single_ref_table(field).map(|table| ConditionKind::Ref { table })
                }
                _ => None,
            },
            _ => None,
        }
    }
    pub fn index_rows(
        &self,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
    ) -> Result<StoredIndexRows<'_>, ibschema::store::StoreError> {
        let rows = self.index_rows_typed(index, lo, hi, dir, after)?;
        Ok(Box::new(rows.map(|r| r.map_err(|e| e.to_string()))))
    }
    pub fn index_rows_typed(
        &self,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
    ) -> Result<StoredTypedIndexRows<'_>, StoreError> {
        let rows = self
            .read
            .range_after_fields(self.id, index, lo, hi, dir, after, &self.reads)?;
        Ok(Box::new(rows.map(|r| {
            let (pos, row) = r?;
            Ok((pos, self.project(&row).map_err(value_error)?))
        })))
    }
    pub fn index_rows_filtered(
        &self,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
        filter: &[(FieldId, bool)],
    ) -> Result<StoredScanned<'_, IndexPosition>, StoreError> {
        let rows = self.read.range_after_fields_filtered(
            self.id,
            index,
            lo,
            hi,
            dir,
            after,
            &self.reads,
            filter,
        )?;
        Ok(Box::new(rows.map(|item| {
            Ok(match item? {
                Scanned::Row(p, row) => Scanned::Row(p, self.project(&row).map_err(value_error)?),
                Scanned::Skipped(p) => Scanned::Skipped(p),
            })
        })))
    }
    pub fn count_index_rows(
        &self,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
    ) -> Result<u64, StoreError> {
        let rows =
            self.read
                .range_after_fields(self.id, index, lo, hi, Direction::Forward, None, &[])?;
        let mut n = 0u64;
        for row in rows {
            row?;
            n += 1;
        }
        Ok(n)
    }
    pub fn next_index_position(
        &self,
        index: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
    ) -> Result<Option<IndexPosition>, StoreError> {
        self.read
            .next_position_after(self.id, index, lo, hi, dir, after)
    }
    fn project(&self, row: &Row) -> Result<Vec<QValue>, String> {
        let values = query_ir::convert::row_to_q(self.info, row);
        let folder = self
            .info
            .field_by_std(StdField::Folder)
            .and_then(|f| values.get(usize::from(f.0)))
            .is_some_and(|v| matches!(v, QValue::Bool(false)));
        let projected = self
            .positions
            .iter()
            .zip(&self.fields)
            .map(|(&index, field)| {
                if (field.only == Only::Folders && !folder) || (field.only == Only::Items && folder)
                {
                    return Ok(QValue::Null);
                }
                let value = values
                    .get(index)
                    .cloned()
                    .ok_or_else(|| "stored field is absent from row".to_string())?;
                if field.role == FieldRole::Std(StdRole::IsFolder)
                    && self
                        .info
                        .fields
                        .get(index)
                        .is_some_and(|f| f.std == Some(StdField::Folder))
                {
                    match value {
                        QValue::Bool(value) => Ok(QValue::Bool(!value)),
                        _ => Err("stored Folder field is not boolean".to_string()),
                    }
                } else {
                    Ok(value)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(projected)
    }
}
