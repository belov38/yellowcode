use std::ops::Bound;
use std::sync::atomic::AtomicBool;

use std::sync::Arc;

use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use query_ir::{ExecError, QValue};

pub const SKIP_MAX_AREAS: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct IndexPrefixes {
    pub tuples: Arc<Vec<Vec<QValue>>>,
    pub fresh: bool,
}

pub(crate) fn index_fields(info: &TableInfo, i: IndexId) -> Option<&[FieldId]> {
    match i {
        IndexId::Id => info.key_index.as_ref(),
        IndexId::Named(k) => info.indexes.get(usize::from(k)),
    }
    .map(|ix| ix.fields.as_slice())
}

pub(crate) fn area_id_key(info: &TableInfo) -> Option<(&[FieldId], FieldId)> {
    let key = info.key_index.as_ref()?;
    let id = info.field_by_std(ibschema::names::StdField::Id)?;
    let (area, rest) = key.fields.split_at_checked(key.prefix)?;
    (key.prefix > 0 && rest == [id]).then_some((area, id))
}

pub type Row = Vec<QValue>;
pub type RowIter<'a> = Box<dyn Iterator<Item = Result<Row, ExecError>> + 'a>;

pub trait DataSource {
    fn map(&self) -> &StorageMap;
    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError>;
    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
    ) -> Result<RowIter<'_>, ExecError>;
    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError>;
    fn row_count_estimate(&self, t: TableId) -> u64;
    fn scan_fields(&self, t: TableId, fields: &[FieldId]) -> Result<RowIter<'_>, ExecError> {
        let _ = fields;
        self.scan(t)
    }
    fn range_fields(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
        fields: &[FieldId],
    ) -> Result<RowIter<'_>, ExecError> {
        let _ = fields;
        self.range(t, i, lo, hi, desc)
    }
    fn get_by_id_fields(
        &self,
        t: TableId,
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        let _ = fields;
        self.get_by_id(t, id)
    }
    fn index_prefixes(
        &self,
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    ) -> Result<Option<IndexPrefixes>, ExecError> {
        let fields: Vec<FieldId> = self
            .map()
            .try_table(t)
            .and_then(|info| index_fields(info, i))
            .and_then(|f| f.get(..n))
            .ok_or_else(|| ExecError::Fatal("index_prefixes: no such index prefix".into()))?
            .to_vec();
        let mut tuples: Vec<Vec<QValue>> = Vec::new();
        for row in self.range_fields(t, i, Bound::Unbounded, Bound::Unbounded, false, &fields)? {
            let row = row?;
            let tuple: Vec<QValue> = fields
                .iter()
                .map(|f| row.get(usize::from(f.0)).cloned().unwrap_or(QValue::Null))
                .collect();
            if tuples.last() != Some(&tuple) {
                if tuples.len() == limit {
                    return Ok(None);
                }
                tuples.push(tuple);
            }
        }
        Ok(Some(IndexPrefixes {
            tuples: Arc::new(tuples),
            fresh: true,
        }))
    }
    fn get_by_id_in(
        &self,
        t: TableId,
        area: &[QValue],
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        let info = self
            .map()
            .try_table(t)
            .ok_or_else(|| ExecError::Fatal("get_by_id_in: unknown table".into()))?;
        let Some((area_fields, id_field)) = area_id_key(info) else {
            return self.get_by_id_fields(t, id, fields);
        };
        let area_fields = area_fields.to_vec();
        let mut wanted = fields.to_vec();
        wanted.extend(area_fields.iter().copied());
        wanted.push(id_field);
        for row in self.scan_fields(t, &wanted)? {
            let row = row?;
            let at = |f: FieldId| row.get(usize::from(f.0));
            if at(id_field) == Some(id)
                && area_fields.len() == area.len()
                && area_fields.iter().zip(area).all(|(f, v)| at(*f) == Some(v))
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }
}

pub struct ExecEnv<'a> {
    pub collation: &'a dyn bsl_prim::Collation,
    pub native: Option<crate::coll::NativeCollation<'a>>,
    pub cancel: &'a AtomicBool,
    pub stop: Option<&'a AtomicBool>,
    pub mem_limit: u64,
    pub max_number_limbs: u64,
    pub memory: Option<&'a memory_budget::RequestMemory>,
    pub lease: Option<&'a memory_budget::Reservation>,
    pub evictor: Option<&'a dyn memory_budget::Evictor>,
}
