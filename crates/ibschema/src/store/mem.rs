use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::ops::Bound;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use onecd::{Collate, DateTime, Numeric, Strength};

use super::{
    Direction, ReadSeek, ReadTxn, Row, RowHandle, RowIter, Store, StoreError, StoreResult,
    WriteTxn, WriterGate, WriterTicket,
};
use crate::columns::ColType;
use crate::fields::{self, FieldValue};
use crate::storage::{FieldId, IndexId, IndexInfo, StorageMap, TableId, TableInfo};
use crate::values::ColumnValue;

enum Order {
    Utf16,
    Icu {
        collate: Arc<dyn Collate>,
        locale: String,
    },
}

struct Inner {
    map: Arc<StorageMap>,
    order: Order,
    state: Mutex<Arc<State>>,
    gate: Arc<WriterGate>,
}

#[derive(Clone)]
struct State {
    tables: Vec<Arc<BTreeMap<u64, Arc<Stored>>>>,
    next_handle: u64,
    next_version: u32,
}

struct Stored {
    row: Row,
    id_key: Vec<KeyPart>,
    keys: Vec<Vec<KeyPart>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyPart {
    Null,
    Bool(bool),
    Num(Numeric),
    Date(DateTime),
    Str(Vec<u8>),
    Bytes(Vec<u8>),
}

fn trim_zeros_front(s: &[u8]) -> &[u8] {
    let i = s.iter().position(|&x| x != 0).unwrap_or(s.len());
    s.get(i..).unwrap_or(&[])
}

fn trim_zeros_back(s: &[u8]) -> &[u8] {
    let n = s.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
    s.get(..n).unwrap_or(&[])
}

fn split(n: &Numeric) -> (&[u8], &[u8]) {
    let d = n.digits();
    let int = d.len().saturating_sub(n.scale() as usize);
    d.split_at_checked(int).unwrap_or((d, &[]))
}

fn magnitude_cmp(a: &Numeric, b: &Numeric) -> Ordering {
    let ((ai, af), (bi, bf)) = (split(a), split(b));
    let (ai, bi) = (trim_zeros_front(ai), trim_zeros_front(bi));
    ai.len()
        .cmp(&bi.len())
        .then_with(|| ai.cmp(bi))
        .then_with(|| trim_zeros_back(af).cmp(trim_zeros_back(bf)))
}

fn num_cmp(a: &Numeric, b: &Numeric) -> Ordering {
    let sign = |n: &Numeric| -> i8 {
        if n.digits().iter().all(|&d| d == 0) {
            0
        } else if n.is_negative() {
            -1
        } else {
            1
        }
    };
    let (sa, sb) = (sign(a), sign(b));
    match sa.cmp(&sb) {
        Ordering::Equal if sa < 0 => magnitude_cmp(b, a),
        Ordering::Equal if sa > 0 => magnitude_cmp(a, b),
        o => o,
    }
}

impl KeyPart {
    fn rank(&self) -> u8 {
        match self {
            KeyPart::Null => 0,
            KeyPart::Bool(_) => 1,
            KeyPart::Num(_) => 2,
            KeyPart::Date(_) => 3,
            KeyPart::Str(_) => 4,
            KeyPart::Bytes(_) => 5,
        }
    }

    fn cmp(&self, o: &KeyPart) -> Ordering {
        match (self, o) {
            (KeyPart::Bool(a), KeyPart::Bool(b)) => a.cmp(b),
            (KeyPart::Num(a), KeyPart::Num(b)) => num_cmp(a, b),
            (KeyPart::Date(a), KeyPart::Date(b)) => a.cmp(b),
            (KeyPart::Str(a), KeyPart::Str(b)) | (KeyPart::Bytes(a), KeyPart::Bytes(b)) => a.cmp(b),
            _ => self.rank().cmp(&o.rank()),
        }
    }
}

fn cmp_keys(a: &[KeyPart], b: &[KeyPart]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.cmp(y))
        .find(|o| o.is_ne())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

fn cmp_prefix(row: &[KeyPart], bound: &[KeyPart]) -> Ordering {
    row.iter()
        .zip(bound)
        .map(|(x, y)| x.cmp(y))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

#[derive(Clone)]
pub struct MemStore {
    inner: Arc<Inner>,
}

impl MemStore {
    pub fn new(map: Arc<StorageMap>) -> MemStore {
        MemStore::build(map, Order::Utf16)
    }

    pub fn with_collate(map: Arc<StorageMap>, collate: Arc<dyn Collate>, locale: &str) -> MemStore {
        MemStore::build(
            map,
            Order::Icu {
                collate,
                locale: locale.to_string(),
            },
        )
    }

    fn build(map: Arc<StorageMap>, order: Order) -> MemStore {
        let tables = map
            .tables()
            .iter()
            .map(|_| Arc::new(BTreeMap::new()))
            .collect();
        let state = State {
            tables,
            next_handle: 1,
            next_version: 1,
        };
        MemStore {
            inner: Arc::new(Inner {
                map,
                order,
                state: Mutex::new(Arc::new(state)),
                gate: WriterGate::new(),
            }),
        }
    }

    fn snapshot(&self) -> Arc<State> {
        Arc::clone(
            &self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

impl Store for MemStore {
    fn storage(&self) -> &StorageMap {
        &self.inner.map
    }

    fn read(&self) -> StoreResult<Box<dyn ReadTxn + Send>> {
        Ok(Box::new(MemRead {
            inner: Arc::clone(&self.inner),
            state: self.snapshot(),
        }))
    }

    fn write(&self, wait: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        let ticket = self.inner.gate.acquire(wait)?;
        Ok(Box::new(MemWrite {
            inner: Arc::clone(&self.inner),
            state: (*self.snapshot()).clone(),
            _ticket: ticket,
        }))
    }
}

struct MemRead {
    inner: Arc<Inner>,
    state: Arc<State>,
}

struct MemWrite {
    inner: Arc<Inner>,
    state: State,
    _ticket: WriterTicket,
}

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

fn table_of(inner: &Inner, t: TableId) -> StoreResult<&TableInfo> {
    inner
        .map
        .try_table(t)
        .ok_or_else(|| not_found(format!("table #{}", t.0)))
}

fn rows_of(st: &State, t: TableId) -> StoreResult<&Arc<BTreeMap<u64, Arc<Stored>>>> {
    st.tables
        .get(t.0 as usize)
        .ok_or_else(|| not_found(format!("table #{}", t.0)))
}

fn stored_of(inner: &Inner, st: &State, t: TableId, h: RowHandle) -> StoreResult<Arc<Stored>> {
    let ti = table_of(inner, t)?;
    rows_of(st, t)?
        .get(&h.0)
        .cloned()
        .ok_or_else(|| not_found(format!("row {} of {}", h.0, ti.logical)))
}

impl Inner {
    fn part(&self, v: &ColumnValue) -> StoreResult<KeyPart> {
        Ok(match v {
            ColumnValue::Null => KeyPart::Null,
            ColumnValue::Bool(b) => KeyPart::Bool(*b),
            ColumnValue::Number(n) => KeyPart::Num(n.clone()),
            ColumnValue::DateTime(d) => KeyPart::Date(*d),
            ColumnValue::Bytes(b) => KeyPart::Bytes(b.clone()),
            ColumnValue::Str(s) => {
                let s = s.trim_end_matches(' ');
                match &self.order {
                    Order::Utf16 => {
                        KeyPart::Str(s.encode_utf16().flat_map(u16::to_be_bytes).collect())
                    }
                    Order::Icu { collate, locale } => {
                        let u: Vec<u16> = s.encode_utf16().collect();
                        KeyPart::Str(
                            collate
                                .sort_key(locale, Strength::Secondary, &u)
                                .map_err(|e| StoreError::Backend(Box::new(e)))?,
                        )
                    }
                }
            }
        })
    }

    fn parts(&self, cols: &[ColumnValue], out: &mut Vec<KeyPart>) -> StoreResult<()> {
        for c in cols {
            out.push(self.part(c)?);
        }
        Ok(())
    }

    fn row_key(
        &self,
        ti: &TableInfo,
        ix: &IndexInfo,
        enc: &[Option<Vec<ColumnValue>>],
    ) -> StoreResult<Vec<KeyPart>> {
        let mut out = Vec::new();
        for f in &ix.fields {
            match enc.get(usize::from(f.0)) {
                Some(Some(cols)) => self.parts(cols, &mut out)?,
                _ => {
                    return Err(StoreError::Unsupported(format!(
                        "{}: index {} over a field without columns",
                        ti.logical, ix.name
                    )));
                }
            }
        }
        Ok(out)
    }

    fn bound_key(
        &self,
        ti: &TableInfo,
        ix: &IndexInfo,
        b: Bound<&[FieldValue]>,
    ) -> StoreResult<Bound<Vec<KeyPart>>> {
        let (vals, incl) = match b {
            Bound::Unbounded => return Ok(Bound::Unbounded),
            Bound::Included(v) => (v, true),
            Bound::Excluded(v) => (v, false),
        };
        if vals.len() > ix.fields.len() {
            return Err(StoreError::Unsupported(format!(
                "{}: bound of {} values over index {} of {} fields",
                ti.logical,
                vals.len(),
                ix.name,
                ix.fields.len()
            )));
        }
        let mut key = Vec::new();
        for (f, v) in ix.fields.iter().zip(vals) {
            let cols = fields::encode_bound(ti, *f, v).map_err(StoreError::from_schema)?;
            self.parts(&cols, &mut key)?;
        }
        Ok(if incl {
            Bound::Included(key)
        } else {
            Bound::Excluded(key)
        })
    }
}

fn scan_in<'a>(inner: &Inner, st: &'a State, t: TableId) -> StoreResult<RowIter<'a>> {
    table_of(inner, t)?;
    let rows = rows_of(st, t)?;
    Ok(Box::new(
        rows.iter().map(|(h, s)| Ok((RowHandle(*h), s.row.clone()))),
    ))
}

fn range_in<'a>(
    inner: &Inner,
    st: &'a State,
    t: TableId,
    i: IndexId,
    lo: Bound<&[FieldValue]>,
    hi: Bound<&[FieldValue]>,
    dir: Direction,
) -> StoreResult<RowIter<'a>> {
    let ti = table_of(inner, t)?;
    let rows = rows_of(st, t)?;
    let (ix, slot) = match i {
        IndexId::Id => (
            ti.key_index
                .as_ref()
                .ok_or_else(|| StoreError::Unsupported(format!("{}: no key index", ti.logical)))?,
            None,
        ),
        IndexId::Named(k) => (
            ti.indexes
                .get(usize::from(k))
                .ok_or_else(|| not_found(format!("index #{k} of {}", ti.logical)))?,
            Some(usize::from(k)),
        ),
    };
    let lo = inner.bound_key(ti, ix, lo)?;
    let hi = inner.bound_key(ti, ix, hi)?;
    let mut hits: Vec<(&[KeyPart], u64, &Arc<Stored>)> = Vec::new();
    for (h, s) in rows.iter() {
        let key: &[KeyPart] = match slot {
            None => &s.id_key,
            Some(k) => s.keys.get(k).map_or(&[][..], Vec::as_slice),
        };
        let above = match &lo {
            Bound::Unbounded => true,
            Bound::Included(b) => cmp_prefix(key, b).is_ge(),
            Bound::Excluded(b) => cmp_prefix(key, b).is_gt(),
        };
        let below = match &hi {
            Bound::Unbounded => true,
            Bound::Included(b) => cmp_prefix(key, b).is_le(),
            Bound::Excluded(b) => cmp_prefix(key, b).is_lt(),
        };
        if above && below {
            hits.push((key, *h, s));
        }
    }
    hits.sort_by(|a, b| cmp_keys(a.0, b.0).then(a.1.cmp(&b.1)));
    if dir == Direction::Backward {
        hits.reverse();
    }
    Ok(Box::new(
        hits.into_iter()
            .map(|(_, h, s)| Ok((RowHandle(h), s.row.clone()))),
    ))
}

fn open_blob_in(
    inner: &Inner,
    st: &State,
    t: TableId,
    h: RowHandle,
    f: FieldId,
) -> StoreResult<Box<dyn ReadSeek>> {
    let ti = table_of(inner, t)?;
    let s = stored_of(inner, st, t, h)?;
    let fi = ti
        .field(f)
        .ok_or_else(|| not_found(format!("field #{} of {}", f.0, ti.logical)))?;
    let is_nt = match ti.columns.get(fi.columns.clone()) {
        Some([c]) if c.ty == ColType::NT => true,
        Some([c]) if c.ty == ColType::I => false,
        _ => {
            return Err(StoreError::Unsupported(format!(
                "{}.{}: not a blob",
                ti.logical, fi.name
            )));
        }
    };
    let bytes = match s.row.get(usize::from(f.0)) {
        Some(FieldValue::Null) => Vec::new(),
        Some(FieldValue::Storage(b) | FieldValue::Binary(b)) => b.clone(),
        Some(FieldValue::String(x)) if is_nt => {
            x.encode_utf16().flat_map(u16::to_le_bytes).collect()
        }
        _ => {
            return Err(StoreError::Unsupported(format!(
                "{}.{}: not a blob",
                ti.logical, fi.name
            )));
        }
    };
    Ok(Box::new(Cursor::new(bytes)))
}

fn count_in(inner: &Inner, st: &State, t: TableId) -> StoreResult<u64> {
    table_of(inner, t)?;
    Ok(rows_of(st, t)?.len() as u64)
}

macro_rules! impl_read {
    ($ty:ty) => {
        impl ReadTxn for $ty {
            fn scan(&self, t: TableId) -> StoreResult<RowIter<'_>> {
                scan_in(&self.inner, &self.state, t)
            }
            fn range(
                &self,
                t: TableId,
                i: IndexId,
                lo: Bound<&[FieldValue]>,
                hi: Bound<&[FieldValue]>,
                dir: Direction,
            ) -> StoreResult<RowIter<'_>> {
                range_in(&self.inner, &self.state, t, i, lo, hi, dir)
            }
            fn get(&self, t: TableId, h: RowHandle) -> StoreResult<Row> {
                Ok(stored_of(&self.inner, &self.state, t, h)?.row.clone())
            }
            fn open_blob(
                &self,
                t: TableId,
                h: RowHandle,
                f: FieldId,
            ) -> StoreResult<Box<dyn ReadSeek + '_>> {
                open_blob_in(&self.inner, &self.state, t, h, f)
            }
            fn row_count_estimate(&self, t: TableId) -> StoreResult<u64> {
                count_in(&self.inner, &self.state, t)
            }
            fn storage_map(&self) -> Option<&StorageMap> {
                Some(&self.inner.map)
            }
        }
    };
}

impl_read!(MemRead);
impl_read!(MemWrite);

struct Prepared {
    id_key: Vec<KeyPart>,
    keys: Vec<Vec<KeyPart>>,
}

fn prepare(inner: &Inner, ti: &TableInfo, row: &Row) -> StoreResult<Prepared> {
    if row.len() != ti.fields.len() {
        return Err(StoreError::ValueDoesNotFit {
            table: ti.logical.clone(),
            field: String::new(),
            why: format!("{} values for {} fields", row.len(), ti.fields.len()),
        });
    }
    let mut enc: Vec<Option<Vec<ColumnValue>>> = Vec::with_capacity(row.len());
    for (i, (fi, v)) in ti.fields.iter().zip(row).enumerate() {
        if fields::is_version(fi) {
            enc.push(None);
            continue;
        }
        let id = FieldId(u16::try_from(i).map_err(|_| not_found(format!("field #{i}")))?);
        enc.push(Some(
            fields::encode(ti, id, v).map_err(StoreError::from_schema)?,
        ));
    }
    let id_key = match &ti.key_index {
        Some(ix) => inner.row_key(ti, ix, &enc)?,
        None => Vec::new(),
    };
    let keys = ti
        .indexes
        .iter()
        .map(|ix| inner.row_key(ti, ix, &enc))
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(Prepared { id_key, keys })
}

fn check_unique(
    t: TableId,
    ti: &TableInfo,
    rows: &BTreeMap<u64, Arc<Stored>>,
    skip: Option<u64>,
    p: &Prepared,
) -> StoreResult<()> {
    let violation = |index: IndexId, name: &str| StoreError::UniqueViolation {
        table: t,
        index,
        table_name: ti.logical.clone(),
        index_name: name.to_string(),
    };
    let others = || rows.iter().filter(|(h, _)| Some(**h) != skip);
    if let Some(ix) = &ti.key_index
        && others().any(|(_, s)| cmp_keys(&s.id_key, &p.id_key).is_eq())
    {
        return Err(violation(IndexId::Id, &ix.name));
    }
    for (k, ix) in ti.indexes.iter().enumerate() {
        if !ix.unique {
            continue;
        }
        let Some(mine) = p.keys.get(k) else { continue };
        if others().any(|(_, s)| s.keys.get(k).is_some_and(|x| cmp_keys(x, mine).is_eq())) {
            let id = u16::try_from(k).map_err(|_| not_found(format!("index #{k}")))?;
            return Err(violation(IndexId::Named(id), &ix.name));
        }
    }
    Ok(())
}

impl MemWrite {
    fn put(&mut self, t: TableId, h: Option<RowHandle>, row: &Row) -> StoreResult<RowHandle> {
        let inner = Arc::clone(&self.inner);
        let ti = table_of(&inner, t)?;
        let rows = rows_of(&self.state, t)?;
        if let Some(h) = h
            && !rows.contains_key(&h.0)
        {
            return Err(not_found(format!("row {} of {}", h.0, ti.logical)));
        }
        let p = prepare(&inner, ti, row)?;
        check_unique(t, ti, rows, h.map(|h| h.0), &p)?;
        let c = self.state.next_version;
        self.state.next_version = c.checked_add(1).ok_or(StoreError::TxnTooLarge)?;
        let handle = match h {
            Some(h) => h,
            None => {
                let n = self.state.next_handle;
                self.state.next_handle = n.checked_add(1).ok_or(StoreError::TxnTooLarge)?;
                RowHandle(n)
            }
        };
        let mut stored_row = row.clone();
        for (fi, v) in ti.fields.iter().zip(stored_row.iter_mut()) {
            if fields::is_version(fi) {
                *v = FieldValue::RowVersion([c, 0, c, 0]);
            }
        }
        let stored = Arc::new(Stored {
            row: stored_row,
            id_key: p.id_key,
            keys: p.keys,
        });
        let slot = self
            .state
            .tables
            .get_mut(t.0 as usize)
            .ok_or_else(|| not_found(format!("table #{}", t.0)))?;
        Arc::make_mut(slot).insert(handle.0, stored);
        Ok(handle)
    }
}

impl WriteTxn for MemWrite {
    fn insert(&mut self, t: TableId, row: &Row) -> StoreResult<RowHandle> {
        self.put(t, None, row)
    }

    fn update(&mut self, t: TableId, h: RowHandle, row: &Row) -> StoreResult<()> {
        self.put(t, Some(h), row).map(|_| ())
    }

    fn delete(&mut self, t: TableId, h: RowHandle) -> StoreResult<()> {
        let name = table_of(&self.inner, t)?.logical.clone();
        let slot = self
            .state
            .tables
            .get_mut(t.0 as usize)
            .ok_or_else(|| not_found(format!("table #{}", t.0)))?;
        if !slot.contains_key(&h.0) {
            return Err(not_found(format!("row {} of {name}", h.0)));
        }
        Arc::make_mut(slot).remove(&h.0);
        Ok(())
    }

    fn commit(self: Box<Self>) -> StoreResult<()> {
        let this = *self;
        *this
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(this.state);
        Ok(())
    }
}
