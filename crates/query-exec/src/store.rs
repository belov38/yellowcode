use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

use ibschema::fields::{FieldValue, encode_bound};
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, StoreError};
use query_ir::convert::{from_q, row_to_q};
use query_ir::{ExecError, QValue, RuntimeError};
use query_syntax::{MsgId, QueryError};

use crate::exec::execute;
use crate::result::QueryResult;
use crate::source::{
    DataSource, ExecEnv, IndexPrefixes, Row, RowIter, SKIP_MAX_AREAS, area_id_key, index_fields,
};
use query_ir::StmtL;

pub struct StoreSource<'a> {
    txn: &'a dyn ReadTxn,
    map: &'a StorageMap,
    areas: RefCell<HashMap<TableId, Option<Areas>>>,
}

type Areas = Arc<Vec<Vec<QValue>>>;

impl<'a> StoreSource<'a> {
    pub fn new(txn: &'a dyn ReadTxn, map: &'a StorageMap) -> StoreSource<'a> {
        StoreSource {
            txn,
            map,
            areas: RefCell::new(HashMap::new()),
        }
    }
}

pub(crate) fn store_error(e: StoreError) -> ExecError {
    match e {
        StoreError::SnapshotTooOld => ExecError::Runtime(RuntimeError {
            chain: vec![QueryError::new(MsgId::SELECT_ERROR_RUNTIMEERROR, None)],
        }),
        StoreError::Memory(m) => ExecError::Memory(m),
        other => ExecError::Fatal(format!("store: {other}")),
    }
}

fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("store source: {what}"))
}

fn rows<'s>(info: &'s TableInfo, it: ibschema::store::RowIter<'s>) -> RowIter<'s> {
    Box::new(it.map(|r| r.map(|(_, row)| row_to_q(info, &row)).map_err(store_error)))
}

impl<'a> StoreSource<'a> {
    fn table(&self, t: TableId) -> Result<&'a TableInfo, ExecError> {
        self.map.try_table(t).ok_or_else(|| fatal("unknown table"))
    }

    fn unheld_point(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
    ) -> bool {
        let (Bound::Included(a), Bound::Included(b)) = (lo, hi) else {
            return false;
        };
        if a != b {
            return false;
        }
        let Some(info) = self.map.try_table(t) else {
            return false;
        };
        let Some(fields) = index_fields(info, i) else {
            return false;
        };
        a.len() <= fields.len()
            && a.iter().zip(fields).any(|(v, f)| {
                !from_q(v, info, *f).is_ok_and(|fv| encode_bound(info, *f, &fv).is_ok())
            })
    }

    fn bound(&self, t: TableId, i: IndexId, b: Bound<&[QValue]>) -> Bound<Vec<FieldValue>> {
        let (vals, inclusive) = match b {
            Bound::Unbounded => return Bound::Unbounded,
            Bound::Included(v) => (v, true),
            Bound::Excluded(v) => (v, false),
        };
        let Some(info) = self.map.try_table(t) else {
            return Bound::Unbounded;
        };
        let Some(fields) = index_fields(info, i) else {
            return Bound::Unbounded;
        };
        if vals.len() > fields.len() {
            return Bound::Unbounded;
        }
        let mut out = Vec::with_capacity(vals.len());
        for (v, f) in vals.iter().zip(fields) {
            let Ok(fv) = from_q(v, info, *f) else {
                return Bound::Unbounded;
            };
            if encode_bound(info, *f, &fv).is_err() {
                return Bound::Unbounded;
            }
            out.push(fv);
        }
        if inclusive || out.iter().any(|v| matches!(v, FieldValue::String(_))) {
            Bound::Included(out)
        } else {
            Bound::Excluded(out)
        }
    }
}

impl DataSource for StoreSource<'_> {
    fn map(&self) -> &StorageMap {
        self.map
    }

    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError> {
        let info = self.table(t)?;
        Ok(rows(info, self.txn.scan(t).map_err(store_error)?))
    }

    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
    ) -> Result<RowIter<'_>, ExecError> {
        if self.unheld_point(t, i, lo, hi) {
            return Ok(Box::new(std::iter::empty()));
        }
        let lo = self.bound(t, i, lo);
        let hi = self.bound(t, i, hi);
        let dir = if desc {
            Direction::Backward
        } else {
            Direction::Forward
        };
        let info = self.table(t)?;
        let it = self
            .txn
            .range(
                t,
                i,
                lo.as_ref().map(Vec::as_slice),
                hi.as_ref().map(Vec::as_slice),
                dir,
            )
            .map_err(store_error)?;
        Ok(rows(info, it))
    }

    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError> {
        self.get_by_id_projected(t, id, None)
    }

    fn row_count_estimate(&self, t: TableId) -> u64 {
        self.txn.row_count_estimate(t).unwrap_or(0)
    }

    fn scan_fields(&self, t: TableId, fields: &[FieldId]) -> Result<RowIter<'_>, ExecError> {
        let info = self.table(t)?;
        Ok(rows(
            info,
            self.txn.scan_fields(t, fields, None).map_err(store_error)?,
        ))
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
        if self.unheld_point(t, i, lo, hi) {
            return Ok(Box::new(std::iter::empty()));
        }
        let lo = self.bound(t, i, lo);
        let hi = self.bound(t, i, hi);
        let dir = if desc {
            Direction::Backward
        } else {
            Direction::Forward
        };
        let info = self.table(t)?;
        let it = self
            .txn
            .range_fields(
                t,
                i,
                lo.as_ref().map(Vec::as_slice),
                hi.as_ref().map(Vec::as_slice),
                dir,
                fields,
            )
            .map_err(store_error)?;
        Ok(rows(info, it))
    }

    fn get_by_id_fields(
        &self,
        t: TableId,
        id: &QValue,
        fields: &[FieldId],
    ) -> Result<Option<Row>, ExecError> {
        self.get_by_id_projected(t, id, Some(fields))
    }

    fn index_prefixes(
        &self,
        t: TableId,
        i: IndexId,
        n: usize,
        limit: usize,
    ) -> Result<Option<IndexPrefixes>, ExecError> {
        if let Some(known) = self.areas.borrow().get(&t) {
            debug_assert!(
                known
                    .as_ref()
                    .is_none_or(|tuples| tuples.first().is_none_or(|first| first.len() == n)),
                "index_prefixes of {t:?}: {n} fields, the cached tuples have another length"
            );
            return Ok(known.clone().map(|tuples| IndexPrefixes {
                tuples,
                fresh: false,
            }));
        }
        let info = self.table(t)?;
        let fields: Vec<FieldId> = index_fields(info, i)
            .and_then(|f| f.get(..n))
            .ok_or_else(|| fatal("no such index prefix"))?
            .to_vec();
        let mut tuples: Vec<Vec<QValue>> = Vec::new();
        let mut after: Option<Vec<FieldValue>> = None;
        let found = loop {
            let lo = match &after {
                Some(key) => Bound::Excluded(key.as_slice()),
                None => Bound::Unbounded,
            };
            let first = self
                .txn
                .range_fields(t, i, lo, Bound::Unbounded, Direction::Forward, &fields)
                .map_err(store_error)?
                .next();
            let Some(first) = first else {
                break true;
            };
            let (_, row) = first.map_err(store_error)?;
            let key: Vec<FieldValue> = fields
                .iter()
                .map(|f| {
                    row.get(usize::from(f.0))
                        .cloned()
                        .unwrap_or(FieldValue::Null)
                })
                .collect();
            if after.as_ref() == Some(&key) {
                return Err(fatal("the jump over the data areas does not advance"));
            }
            if tuples.len() == limit {
                break false;
            }
            let q = row_to_q(info, &row);
            tuples.push(
                fields
                    .iter()
                    .map(|f| q.get(usize::from(f.0)).cloned().unwrap_or(QValue::Null))
                    .collect(),
            );
            after = Some(key);
        };
        let kept = found.then(|| Arc::new(tuples));
        self.areas.borrow_mut().insert(t, kept.clone());
        Ok(kept.map(|tuples| IndexPrefixes {
            tuples,
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
        let info = self.table(t)?;
        let Some((area_fields, _)) = area_id_key(info) else {
            return self.get_by_id_fields(t, id, fields);
        };
        if area.len() != area_fields.len() {
            return Err(fatal(
                "get_by_id_in: the area does not match the separators",
            ));
        }
        let mut key = Vec::with_capacity(area.len() + 1);
        for (v, f) in area.iter().zip(area_fields) {
            match from_q(v, info, *f) {
                Ok(fv) if encode_bound(info, *f, &fv).is_ok() => key.push(fv),
                _ => return Ok(None),
            }
        }
        self.point_in_area(t, key, id, Some(fields))
    }
}

impl StoreSource<'_> {
    fn point_in_area(
        &self,
        t: TableId,
        mut key: Vec<FieldValue>,
        id: &QValue,
        fields: Option<&[FieldId]>,
    ) -> Result<Option<Row>, ExecError> {
        let info = self.table(t)?;
        let Some((_, id_field)) = area_id_key(info) else {
            return Err(fatal("point_in_area: not a separated key"));
        };
        let Ok(idv) = from_q(id, info, id_field) else {
            return Ok(None);
        };
        if encode_bound(info, id_field, &idv).is_err() {
            return Ok(None);
        }
        key.push(idv);
        let wanted: Option<Vec<FieldId>> = fields.map(|fs| {
            let mut v = fs.to_vec();
            if !v.contains(&id_field) {
                v.push(id_field);
            }
            v
        });
        let it = match &wanted {
            Some(w) => self.txn.range_fields(
                t,
                IndexId::Id,
                Bound::Included(&key[..]),
                Bound::Included(&key[..]),
                Direction::Forward,
                w,
            ),
            None => self.txn.range(
                t,
                IndexId::Id,
                Bound::Included(&key[..]),
                Bound::Included(&key[..]),
                Direction::Forward,
            ),
        }
        .map_err(store_error)?;
        let pos = usize::from(id_field.0);
        for r in rows(info, it) {
            let r = r?;
            if r.get(pos) == Some(id) {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    fn get_by_id_projected(
        &self,
        t: TableId,
        id: &QValue,
        fields: Option<&[FieldId]>,
    ) -> Result<Option<Row>, ExecError> {
        let info = self.table(t)?;
        let id_field = info
            .field_by_std(StdField::Id)
            .or_else(|| info.field_by_name("ID"))
            .ok_or_else(|| fatal("a table without ID"))?;
        let pos = usize::from(id_field.0);
        let is_row = |r: &Row| r.get(pos) == Some(id);
        let wanted: Option<Vec<FieldId>> = fields.map(|fs| {
            let mut v = fs.to_vec();
            if !v.contains(&id_field) {
                v.push(id_field);
            }
            v
        });
        let only_id = |fields: &[FieldId]| fields == [id_field];
        let ix = if info
            .key_index
            .as_ref()
            .is_some_and(|ix| only_id(&ix.fields))
        {
            Some(IndexId::Id)
        } else {
            info.indexes
                .iter()
                .position(|ix| only_id(&ix.fields))
                .and_then(|k| u16::try_from(k).ok())
                .map(IndexId::Named)
        };
        if ix.is_none()
            && let Some((area_fields, _)) = area_id_key(info)
            && let Some(areas) =
                self.index_prefixes(t, IndexId::Id, area_fields.len(), SKIP_MAX_AREAS)?
        {
            for tuple in areas.tuples.iter() {
                let mut key = Vec::with_capacity(tuple.len() + 1);
                for (v, f) in tuple.iter().zip(area_fields) {
                    key.push(from_q(v, info, *f).map_err(|e| fatal(&e.to_string()))?);
                }
                if let Some(row) = self.point_in_area(t, key, id, fields)? {
                    return Ok(Some(row));
                }
            }
            return Ok(None);
        }
        let Some(ix) = ix else {
            let it = match &wanted {
                Some(w) => self.scan_fields(t, w)?,
                None => self.scan(t)?,
            };
            for r in it {
                let r = r?;
                if is_row(&r) {
                    return Ok(Some(r));
                }
            }
            return Ok(None);
        };
        let Ok(key) = from_q(id, info, id_field) else {
            return Ok(None);
        };
        if encode_bound(info, id_field, &key).is_err() {
            return Ok(None);
        }
        let key = [key];
        let it = match &wanted {
            Some(w) => self.txn.range_fields(
                t,
                ix,
                Bound::Included(&key[..]),
                Bound::Included(&key[..]),
                Direction::Forward,
                w,
            ),
            None => self.txn.range(
                t,
                ix,
                Bound::Included(&key[..]),
                Bound::Included(&key[..]),
                Direction::Forward,
            ),
        }
        .map_err(store_error)?;
        for r in rows(info, it) {
            let r = r?;
            if is_row(&r) {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }
}

pub fn execute_store(
    stmt: &StmtL,
    txn: &dyn ReadTxn,
    map: &StorageMap,
    env: &ExecEnv<'_>,
) -> Result<QueryResult, ExecError> {
    execute(stmt, &StoreSource::new(txn, map), env)
}
