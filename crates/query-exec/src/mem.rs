use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use bsl_prim::Collation;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId};
use query_ir::{ExecError, QValue};

use crate::key::{Key, key_of};
use crate::source::{DataSource, Row, RowIter};

type Entries = BTreeMap<Vec<Key>, Vec<usize>>;

#[derive(Default)]
struct TableData {
    rows: Vec<Row>,
    indexes: Vec<Entries>,
    by_id: HashMap<Key, usize>,
}

pub struct MemSource {
    map: Arc<StorageMap>,
    coll: Arc<dyn Collation>,
    tables: Vec<TableData>,
}

fn fatal(s: impl Into<String>) -> ExecError {
    ExecError::Fatal(s.into())
}

impl MemSource {
    pub fn new(map: Arc<StorageMap>, coll: Arc<dyn Collation>) -> MemSource {
        let tables = map
            .tables()
            .iter()
            .map(|t| TableData {
                indexes: (0..=t.indexes.len()).map(|_| Entries::new()).collect(),
                ..TableData::default()
            })
            .collect();
        MemSource { map, coll, tables }
    }

    fn index_fields(&self, t: TableId, i: IndexId) -> Result<Vec<FieldId>, ExecError> {
        let info = self
            .map
            .try_table(t)
            .ok_or_else(|| fatal("unknown table"))?;
        match i {
            IndexId::Id => match &info.key_index {
                Some(ix) => Ok(ix.fields.clone()),
                None => info
                    .field_by_std(StdField::Id)
                    .or_else(|| info.field_by_name("ID"))
                    .map(|f| vec![f])
                    .ok_or_else(|| fatal("table without ID")),
            },
            IndexId::Named(k) => info
                .indexes
                .get(usize::from(k))
                .map(|ix| ix.fields.clone())
                .ok_or_else(|| fatal("unknown index")),
        }
    }

    fn slot(i: IndexId) -> usize {
        match i {
            IndexId::Id => 0,
            IndexId::Named(k) => usize::from(k) + 1,
        }
    }

    fn row_key(&self, fields: &[FieldId], row: &Row) -> Vec<Key> {
        fields
            .iter()
            .map(|f| {
                let v = row.get(usize::from(f.0)).unwrap_or(&QValue::Null);
                key_of(v, self.coll.as_ref())
            })
            .collect()
    }

    pub fn insert(&mut self, t: TableId, row: Row) -> Result<(), ExecError> {
        let info = self
            .map
            .try_table(t)
            .ok_or_else(|| fatal("unknown table"))?;
        if row.len() != info.fields.len() {
            return Err(fatal(format!(
                "row of {} values for table {} of {} fields",
                row.len(),
                info.logical,
                info.fields.len()
            )));
        }
        let n_named = info.indexes.len();
        let id_field = info
            .field_by_std(StdField::Id)
            .or_else(|| info.field_by_name("ID"));
        let mut keys = Vec::with_capacity(n_named + 1);
        keys.push(self.row_key(&self.index_fields(t, IndexId::Id).unwrap_or_default(), &row));
        for k in 0..n_named {
            let k = u16::try_from(k).map_err(|_| fatal("too many indexes"))?;
            keys.push(self.row_key(&self.index_fields(t, IndexId::Named(k))?, &row));
        }
        let id_key = id_field
            .and_then(|f| row.get(usize::from(f.0)))
            .map(|v| key_of(v, self.coll.as_ref()));
        let data = self
            .tables
            .get_mut(t.0 as usize)
            .ok_or_else(|| fatal("unknown table"))?;
        let pos = data.rows.len();
        data.rows.push(row);
        for (entries, key) in data.indexes.iter_mut().zip(keys) {
            entries.entry(key).or_default().push(pos);
        }
        if let Some(k) = id_key {
            data.by_id.entry(k).or_insert(pos);
        }
        Ok(())
    }

    fn data(&self, t: TableId) -> Result<&TableData, ExecError> {
        self.tables
            .get(t.0 as usize)
            .ok_or_else(|| fatal("unknown table"))
    }
}

fn prefix_cmp(k: &[Key], p: &[Key]) -> std::cmp::Ordering {
    k.iter().take(p.len()).cmp(p.iter())
}

impl DataSource for MemSource {
    fn map(&self) -> &StorageMap {
        &self.map
    }

    fn scan(&self, t: TableId) -> Result<RowIter<'_>, ExecError> {
        Ok(Box::new(self.data(t)?.rows.iter().map(|r| Ok(r.clone()))))
    }

    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[QValue]>,
        hi: Bound<&[QValue]>,
        desc: bool,
    ) -> Result<RowIter<'_>, ExecError> {
        use std::cmp::Ordering::*;
        let data = self.data(t)?;
        let entries = data
            .indexes
            .get(Self::slot(i))
            .ok_or_else(|| fatal("unknown index"))?;
        self.index_fields(t, i)?;
        let keys = |vs: &[QValue]| -> Vec<Key> {
            vs.iter().map(|v| key_of(v, self.coll.as_ref())).collect()
        };
        let lo = match lo {
            Bound::Included(v) => Bound::Included(keys(v)),
            Bound::Excluded(v) => Bound::Excluded(keys(v)),
            Bound::Unbounded => Bound::Unbounded,
        };
        let hi = match hi {
            Bound::Included(v) => Bound::Included(keys(v)),
            Bound::Excluded(v) => Bound::Excluded(keys(v)),
            Bound::Unbounded => Bound::Unbounded,
        };
        let start = match &lo {
            Bound::Included(p) | Bound::Excluded(p) => Bound::Included(p.clone()),
            Bound::Unbounded => Bound::Unbounded,
        };
        let mut positions: Vec<usize> = Vec::new();
        for (k, rows) in entries.range((start, Bound::Unbounded)) {
            match &lo {
                Bound::Excluded(p) if prefix_cmp(k, p) == Equal => continue,
                _ => {}
            }
            let past = match &hi {
                Bound::Included(p) => prefix_cmp(k, p) == Greater,
                Bound::Excluded(p) => prefix_cmp(k, p) != Less,
                Bound::Unbounded => false,
            };
            if past {
                break;
            }
            positions.extend(rows.iter().copied());
        }
        if desc {
            positions.reverse();
        }
        Ok(Box::new(
            positions
                .into_iter()
                .filter_map(move |p| data.rows.get(p).cloned())
                .map(Ok),
        ))
    }

    fn get_by_id(&self, t: TableId, id: &QValue) -> Result<Option<Row>, ExecError> {
        let data = self.data(t)?;
        let k = key_of(id, self.coll.as_ref());
        Ok(data.by_id.get(&k).and_then(|&p| data.rows.get(p)).cloned())
    }

    fn row_count_estimate(&self, t: TableId) -> u64 {
        self.data(t).map_or(0, |d| d.rows.len() as u64)
    }
}
