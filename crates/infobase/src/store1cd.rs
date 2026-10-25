pub mod managed;
pub mod managed_table;

use std::io::{Cursor, Read};
use std::ops::Bound;
use std::sync::Arc;
use std::time::Duration;

use ibschema::columns::{ColRole, ColType, Part};
use ibschema::fields::{self, FieldValue};
use ibschema::storage::{FieldId, FieldInfo, IndexId, IndexInfo, StorageMap, TableId, TableInfo};
use ibschema::store::{
    Direction, IndexAccess, IndexPosition, PositionedRows, ReadSeek, ReadTxn, RecordFilter, Row,
    RowHandle, RowIter, Scanned, ScannedPositioned, ScannedRows, Store, StoreError, StoreResult,
    WriteTxn, WriterGate,
};
use ibschema::values::RefId;
use onecd::IndexRead;

use crate::colval;
use crate::error::{Error, Result};

type Rows<'q> = Box<dyn Iterator<Item = onecd::Result<(onecd::RowId, onecd::Row)>> + 'q>;

self_cell::self_cell!(
    struct ScanCell<'a> {
        owner: onecd::TableRead<'a>,
        #[covariant]
        dependent: Rows,
    }
);
self_cell::self_cell!(
    struct IndexCell<'a> {
        owner: onecd::TableRead<'a>,
        #[covariant]
        dependent: IndexRead,
    }
);
self_cell::self_cell!(
    struct RangeCell<'a> {
        owner: IndexCell<'a>,
        #[covariant]
        dependent: Rows,
    }
);

type PositionedEngineRows<'q> =
    Box<dyn Iterator<Item = onecd::Result<(onecd::index::IndexEntry, onecd::Row)>> + 'q>;
self_cell::self_cell!(
    struct PositionedCell<'a> {
        owner: IndexCell<'a>,
        #[covariant]
        dependent: PositionedEngineRows,
    }
);
struct PositionedIter<'a> {
    cell: PositionedCell<'a>,
    map: &'a StorageMap,
    t: &'a TableInfo,
    p: &'a PhysTable,
    fields: Projection,
    prefix: Option<Prefix<'a>>,
    checks: Vec<Check>,
}
impl Iterator for PositionedIter<'_> {
    type Item = StoreResult<Scanned<IndexPosition>>;
    fn next(&mut self) -> Option<Self::Item> {
        let fields = self.fields.as_deref();
        self.cell.with_dependent_mut(|ic, rows| {
            Some(match rows.next()? {
                Ok((entry, row)) => match passes(self.t, &self.checks, &row) {
                    Ok(true) => decode_cells(
                        self.map,
                        self.t,
                        self.p,
                        &row,
                        &mut |r, max| read_blob(ic.borrow_owner(), r, max),
                        fields,
                        self.prefix,
                    )
                    .map_err(|e| at_row(e, u64::from(entry.row)))
                    .map(|row| {
                        Scanned::Row(
                            IndexPosition {
                                key: entry.key,
                                row: handle(entry.row),
                            },
                            row,
                        )
                    }),
                    Ok(false) => Ok(Scanned::Skipped(IndexPosition {
                        key: entry.key,
                        row: handle(entry.row),
                    })),
                    Err(e) => Err(at_row(e, u64::from(entry.row))),
                },
                Err(e) => Err(engine_err(e)),
            })
        })
    }
}

fn rows_only<'a, P: 'a>(
    items: impl Iterator<Item = StoreResult<Scanned<P>>> + 'a,
) -> impl Iterator<Item = StoreResult<(P, Row)>> + 'a {
    items.filter_map(|item| match item {
        Ok(Scanned::Row(p, row)) => Some(Ok((p, row))),
        Ok(Scanned::Skipped(_)) => None,
        Err(e) => Some(Err(e)),
    })
}

#[derive(Clone, Copy, Debug)]
struct Check {
    field: usize,
    col: usize,
    want: bool,
}

fn checks(t: &TableInfo, p: &PhysTable, filter: RecordFilter<'_>) -> StoreResult<Vec<Check>> {
    filter
        .iter()
        .map(|&(f, want)| {
            let field = usize::from(f.0);
            let fi = t
                .fields
                .get(field)
                .ok_or_else(|| StoreError::NotFound(format!("field #{} of {}", f.0, t.logical)))?;
            let col = match t.columns.get(fi.columns.clone()) {
                Some([c]) if c.ty == ColType::L && !c.nullable => p.cols.get(fi.columns.start),
                _ => None,
            }
            .ok_or_else(|| {
                StoreError::Unsupported(format!(
                    "{}.{}: a record filter takes a stored not null boolean field",
                    t.logical, fi.name
                ))
            })?;
            Ok(Check {
                field,
                col: *col,
                want,
            })
        })
        .collect()
}

fn passes(t: &TableInfo, checks: &[Check], row: &onecd::Row) -> StoreResult<bool> {
    for c in checks {
        match row.get(c.col) {
            Some(onecd::Value::Bool(b)) => {
                if *b != c.want {
                    return Ok(false);
                }
            }
            _ => {
                return Err(match t.fields.get(c.field) {
                    Some(f) => bad_value(t, f, "the record filter value is not a boolean"),
                    None => StoreError::NotFound(format!("field #{} of {}", c.field, t.logical)),
                });
            }
        }
    }
    Ok(true)
}

fn condition_reason(
    t: &TableInfo,
    f: FieldId,
    tr: &onecd::TableRead<'_>,
    ir: &IndexRead<'_>,
) -> Option<String> {
    let Some(fi) = t.field(f) else {
        return Some("index field is absent".into());
    };
    if fi.nullable {
        return Some("nullable fields require NULL-aware conditions; unsupported in v1".into());
    }
    let Some([c]) = t.columns.get(fi.columns.clone()) else {
        return Some("composite fields are not supported for exact conditions".into());
    };
    let reason = match (c.ty, c.role) {
        (ColType::N(_, _), _) => "numeric field is not stored fully in this index",
        (ColType::DT, _) => "date field is not stored fully in this index",
        (ColType::NC(_) | ColType::NVC(_), ColRole::Str) => {
            "string field is not stored fully in this index"
        }
        (ColType::B(16), ColRole::Ref) if fields::single_ref_table(fi).is_some() => {
            "reference field is not stored fully in this index"
        }
        _ => {
            return Some(
                "only numeric, date, string and reference fields support exact conditions".into(),
            );
        }
    };
    let physical = tr.descr().field(&c.name).map(|(_, d)| d);
    let indexed = ir
        .descr()
        .fields
        .iter()
        .find(|d| same_name(&d.name, &c.name));
    let whole = |d: &onecd::FieldDescr, i: &onecd::IndexField| {
        i.prefix_len == 0
            || (matches!(c.ty, ColType::NC(_) | ColType::NVC(_) | ColType::B(_))
                && i.prefix_len >= d.length)
    };
    let Some(d) = physical.filter(|d| {
        colval::col_type_of(d) == Some(c.ty)
            && c.nullable == d.nullable
            && indexed.is_some_and(|i| whole(d, i))
    }) else {
        return Some(reason.into());
    };
    if d.ty.is_string() && d.case_sensitive {
        return Some("case-sensitive string fields are not supported for exact conditions".into());
    }
    None
}

fn exact_ref_pair(
    t: &TableInfo,
    f: FieldId,
    tr: &onecd::TableRead<'_>,
    ir: &IndexRead<'_>,
) -> bool {
    let Some(fi) = t.field(f).filter(|fi| !fi.nullable) else {
        return false;
    };
    let Some(cols @ [a, b]) = t.columns.get(fi.columns.clone()) else {
        return false;
    };
    if (a.part, a.ty, b.part, b.ty) != (Part::TRef, ColType::B(4), Part::RRef, ColType::B(16)) {
        return false;
    }
    let at = |name: &str| {
        ir.descr()
            .fields
            .iter()
            .position(|d| same_name(&d.name, name))
    };
    if !matches!((at(&a.name), at(&b.name)), (Some(t), Some(r)) if r == t + 1) {
        return false;
    }
    cols.iter().all(|c| {
        let physical = tr.descr().field(&c.name).map(|(_, d)| d);
        let indexed = ir
            .descr()
            .fields
            .iter()
            .find(|d| same_name(&d.name, &c.name));
        physical.zip(indexed).is_some_and(|(d, i)| {
            colval::col_type_of(d) == Some(c.ty)
                && !d.nullable
                && (i.prefix_len == 0 || i.prefix_len >= d.length)
        })
    })
}

struct PhysTable {
    name: String,
    cols: Vec<usize>,
    indexes: Vec<Option<String>>,
    key_index: Option<String>,
}

struct PhysMap {
    tables: Vec<PhysTable>,
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn find_index(
    d: &onecd::TableDescr,
    ti: &TableInfo,
    ix: &IndexInfo,
    taken: &[Option<String>],
) -> Result<Option<String>> {
    let mut want = Vec::new();
    for f in &ix.fields {
        let fi = ti.field(*f).ok_or_else(|| {
            Error::Inconsistent(format!(
                "{}: index {} over a missing field",
                ti.logical, ix.name
            ))
        })?;
        for i in fi.columns.clone() {
            let c = ti.columns.get(i).ok_or_else(|| {
                Error::Inconsistent(format!("{}: field {} without columns", ti.logical, fi.name))
            })?;
            want.push(c.name.as_str());
        }
    }
    let starts = |x: &onecd::IndexDescr| {
        x.fields.len() >= want.len()
            && x.fields
                .iter()
                .zip(&want)
                .all(|(a, b)| same_name(&a.name, b))
    };
    let exact: Vec<&onecd::IndexDescr> = d
        .indexes
        .iter()
        .filter(|x| x.fields.len() == want.len() && starts(x))
        .collect();
    if let Some(x) = exact
        .iter()
        .find(|x| !taken.iter().any(|t| t.as_deref() == Some(x.name.as_str())))
        .or(exact.first())
    {
        return Ok(Some(x.name.clone()));
    }
    Ok(d.indexes
        .iter()
        .filter(|x| starts(x))
        .min_by_key(|x| x.fields.len())
        .map(|x| x.name.clone()))
}

impl PhysMap {
    fn build(db: &onecd::Database, map: &StorageMap) -> Result<PhysMap> {
        let txn = db.read();
        let mut tables = Vec::with_capacity(map.tables().len());
        for ti in map.tables() {
            let tr = txn.table(&ti.phys).map_err(|e| match e {
                onecd::Error::NotFound(_) => {
                    Error::Inconsistent(format!("{}: the 1CD has no table {}", ti.logical, ti.phys))
                }
                other => other.into(),
            })?;
            let d = tr.descr();
            let mut cols = Vec::with_capacity(ti.columns.len());
            for c in &ti.columns {
                let (pos, f) = d
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| same_name(&f.name, &c.name))
                    .ok_or_else(|| {
                        Error::Inconsistent(format!(
                            "{}: the 1CD table {} has no column {}",
                            ti.logical, ti.phys, c.name
                        ))
                    })?;
                if colval::col_type_of(f) != Some(c.ty) {
                    return Err(Error::Inconsistent(format!(
                        "{}: column {} is {:?} in the 1CD, {:?} in the schema",
                        ti.logical, c.name, f.ty, c.ty
                    )));
                }
                cols.push(pos);
            }
            let mut indexes: Vec<Option<String>> = Vec::with_capacity(ti.indexes.len());
            for ix in &ti.indexes {
                let name = find_index(d, ti, ix, &indexes)?;
                indexes.push(name);
            }
            let key_index = ti
                .key_index
                .as_ref()
                .map(|ix| find_index(d, ti, ix, &indexes))
                .transpose()?
                .flatten();
            tables.push(PhysTable {
                name: ti.phys.clone(),
                cols,
                indexes,
                key_index,
            });
        }
        Ok(PhysMap { tables })
    }
}

pub struct Store1cd {
    db: Arc<onecd::Database>,
    map: Arc<StorageMap>,
    phys: Arc<PhysMap>,
    #[allow(dead_code)]
    gate: Arc<WriterGate>,
    refusal: Option<String>,
}

impl Store1cd {
    pub fn new(
        db: Arc<onecd::Database>,
        map: Arc<StorageMap>,
        gate: Arc<WriterGate>,
        refusal: Option<String>,
    ) -> Result<Store1cd> {
        let phys = Arc::new(PhysMap::build(&db, &map)?);
        Ok(Store1cd {
            db,
            map,
            phys,
            gate,
            refusal,
        })
    }
}

impl Store for Store1cd {
    fn storage(&self) -> &StorageMap {
        &self.map
    }
    fn read(&self) -> StoreResult<Box<dyn ReadTxn + Send>> {
        Ok(Box::new(Read1cd {
            txn: self.db.read(),
            map: Arc::clone(&self.map),
            phys: Arc::clone(&self.phys),
            memory: self.db.memory_budget().map(|budget| RowMemory {
                budget: budget.clone(),
                evictor: Arc::clone(&self.db),
            }),
            last_get: std::sync::Mutex::new(None),
            prefix: std::sync::atomic::AtomicUsize::new(0),
            cuts: std::sync::Mutex::new(Vec::new()),
        }))
    }
    fn write(&self, _wait: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        Err(StoreError::Unsupported(match &self.refusal {
            Some(r) => r.clone(),
            None => "store1cd writes: stage t3-d7w".to_string(),
        }))
    }
}

fn engine_err(e: onecd::Error) -> StoreError {
    match e {
        onecd::Error::Memory(memory_budget::BudgetError::Limit(m)) => StoreError::Memory(m),
        onecd::Error::SnapshotTooOld => StoreError::SnapshotTooOld,
        onecd::Error::TxnTooLarge => StoreError::TxnTooLarge,
        onecd::Error::NotFound(n) => StoreError::NotFound(n.to_string()),
        other => StoreError::Backend(Box::new(other)),
    }
}

fn bad_value(t: &TableInfo, f: &FieldInfo, why: impl Into<String>) -> StoreError {
    StoreError::BadValue {
        table: t.logical.clone(),
        field: f.name.clone(),
        row: 0,
        why: why.into(),
    }
}

fn at_row(e: StoreError, row: u64) -> StoreError {
    match e {
        StoreError::BadValue {
            table,
            field,
            row: 0,
            why,
        } => StoreError::BadValue {
            table,
            field,
            row,
            why,
        },
        other => other,
    }
}

fn lib_err(e: Error) -> StoreError {
    match e {
        Error::Engine(e) => engine_err(e),
        Error::Schema(e) => StoreError::from_schema(e),
        other => StoreError::Backend(Box::new(other)),
    }
}

fn cell_err(e: Error, t: &TableInfo, f: &FieldInfo) -> StoreError {
    match e {
        Error::Engine(e) => engine_err(e),
        Error::Schema(e) => StoreError::from_schema(e),
        Error::Inconsistent(w) | Error::Unsupported(w) => bad_value(t, f, w),
        other => bad_value(t, f, other.to_string()),
    }
}

type Projection = Option<Vec<bool>>;

fn projection(t: &TableInfo, fields: &[FieldId]) -> Projection {
    let mut mask = vec![false; t.fields.len()];
    for f in fields {
        if let Some(m) = mask.get_mut(usize::from(f.0)) {
            *m = true;
        }
    }
    Some(mask)
}

fn selected(fields: &Projection, k: usize) -> bool {
    fields
        .as_ref()
        .is_none_or(|m| m.get(k).copied().unwrap_or(false))
}

fn decode_cells(
    map: &StorageMap,
    t: &TableInfo,
    p: &PhysTable,
    row: &onecd::Row,
    blob: &mut dyn FnMut(onecd::BlobRef, Option<usize>) -> onecd::Result<Vec<u8>>,
    fields: Option<&[bool]>,
    prefix: Option<Prefix<'_>>,
) -> StoreResult<Row> {
    let mut cuts = Vec::new();
    let out = decode_cells_cut(
        map,
        t,
        p,
        row,
        blob,
        fields,
        prefix.map(|p| p.max),
        &mut cuts,
    );
    if let Some(prefix) = prefix {
        *prefix.cuts.lock().unwrap_or_else(|e| e.into_inner()) = cuts;
    }
    out
}

#[derive(Clone, Copy)]
struct Prefix<'a> {
    max: usize,
    cuts: &'a std::sync::Mutex<Vec<(FieldId, u64)>>,
}

fn utf16_prefix(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.truncate(bytes.len() & !1);
    if let [.., lo, hi] = bytes[..]
        && (0xD800..0xDC00).contains(&u16::from_le_bytes([lo, hi]))
    {
        bytes.truncate(bytes.len() - 2);
    }
    bytes
}

fn blob_read_len(v: &onecd::Value, prefix: Option<usize>) -> Option<u64> {
    match v {
        onecd::Value::NText(onecd::BlobVal::Ref(r))
        | onecd::Value::Image(onecd::BlobVal::Ref(r)) => {
            let len = u64::from(r.len);
            Some(prefix.map_or(len, |max| len.min(max as u64)))
        }
        onecd::Value::Text(onecd::BlobVal::Ref(r)) => Some(u64::from(r.len)),
        _ => None,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "decode_cells and its record of partial values"
)]
fn decode_cells_cut(
    map: &StorageMap,
    t: &TableInfo,
    p: &PhysTable,
    row: &onecd::Row,
    blob: &mut dyn FnMut(onecd::BlobRef, Option<usize>) -> onecd::Result<Vec<u8>>,
    fields: Option<&[bool]>,
    prefix: Option<usize>,
    cuts: &mut Vec<(FieldId, u64)>,
) -> StoreResult<Row> {
    let mut out = Vec::with_capacity(t.fields.len());
    for (k, f) in t.fields.iter().enumerate() {
        if fields.is_some_and(|m| !m.get(k).copied().unwrap_or(false)) {
            out.push(FieldValue::Null);
            continue;
        }
        let cell = |i: usize| p.cols.get(i).and_then(|&c| row.get(c));
        if fields::is_version(f) {
            out.push(match cell(f.columns.start) {
                Some(onecd::Value::RowVersion(v)) => FieldValue::RowVersion(*v),
                _ => return Err(bad_value(t, f, "the version cell is not a row version")),
            });
            continue;
        }
        let id = FieldId(u16::try_from(k).map_err(|_| bad_value(t, f, "field number"))?);
        let mut cols = Vec::with_capacity(f.columns.len());
        for i in f.columns.clone() {
            let (Some(v), Some(c)) = (cell(i), t.columns.get(i)) else {
                return Err(bad_value(t, f, "the row has no value for a column"));
            };
            let part = match (prefix, v) {
                (
                    Some(max),
                    onecd::Value::NText(onecd::BlobVal::Ref(r))
                    | onecd::Value::Image(onecd::BlobVal::Ref(r)),
                ) if u64::from(r.len) > max as u64 => {
                    let bytes = blob(*r, Some(max)).map_err(|e| cell_err(Error::from(e), t, f))?;
                    cuts.push((id, u64::from(r.len)));
                    Some(match v {
                        onecd::Value::NText(_) => {
                            onecd::Value::NText(onecd::BlobVal::Data(utf16_prefix(bytes)))
                        }
                        _ => onecd::Value::Image(onecd::BlobVal::Data(bytes)),
                    })
                }
                _ => None,
            };
            let v = part.as_ref().unwrap_or(v);
            cols.push(
                colval::from_onecd(v, c.ty, &mut |r| blob(r, None))
                    .map_err(|e| cell_err(e, t, f))?,
            );
        }
        let v = fields::decode(t, id, &cols).map_err(StoreError::from_schema)?;
        if let FieldValue::Ref { table, id } = &v
            && *id != RefId::NIL
            && map.ref_table(*table).is_none()
            && t.columns
                .get(f.columns.clone())
                .is_some_and(|c| c.iter().any(|c| matches!(c.part, Part::RTRef | Part::TRef)))
        {
            return Err(bad_value(
                t,
                f,
                format!("reference to table number {table}, which the base does not have"),
            ));
        }
        out.push(v);
    }
    Ok(out)
}

struct Read1cd {
    txn: onecd::ReadTxn,
    map: Arc<StorageMap>,
    phys: Arc<PhysMap>,
    memory: Option<RowMemory>,
    last_get: std::sync::Mutex<Option<memory_budget::Reservation>>,
    prefix: std::sync::atomic::AtomicUsize,
    cuts: std::sync::Mutex<Vec<(FieldId, u64)>>,
}

#[derive(Clone)]
struct RowMemory {
    budget: memory_budget::MemoryBudget,
    evictor: Arc<onecd::Database>,
}

pub mod row_model {
    pub const PER_STORED_BYTE: u64 = 3;
    pub const PER_FIELD: u64 = 128;
    pub const FIXED: u64 = 4 << 10;

    pub fn row(stored: u64, fields: usize) -> u64 {
        PER_STORED_BYTE
            .saturating_mul(stored)
            .saturating_add(PER_FIELD.saturating_mul(fields as u64))
            .saturating_add(FIXED)
    }
}

impl RowMemory {
    fn context(&self) -> memory_budget::RequestMemory {
        memory_budget::RequestMemory {
            row_limit: self
                .budget
                .snapshot()
                .categories
                .get(memory_budget::Category::Row as usize)
                .map_or(0, |c| c.limit),
            budget: self.budget.clone(),
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the parts of one row read")]
    fn read(
        &self,
        tr: &onecd::TableRead<'_>,
        ctx: &memory_budget::RequestMemory,
        map: &StorageMap,
        t: &TableInfo,
        p: &PhysTable,
        id: onecd::RowId,
        fields: &Projection,
    ) -> StoreResult<Option<(Row, memory_budget::Reservation)>> {
        let raw = match tr.get_budgeted(id, ctx) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(None),
            Err(e) => return Err(engine_err(e)),
        };
        let mut stored = u64::from(tr.layout().record_len);
        for (k, f) in t.fields.iter().enumerate() {
            if !selected(fields, k) {
                continue;
            }
            for i in f.columns.clone() {
                if let Some(read) = p
                    .cols
                    .get(i)
                    .and_then(|&c| raw.get(c))
                    .and_then(|v| blob_read_len(v, None))
                {
                    stored = stored.saturating_add(read);
                }
            }
        }
        let lease = self
            .budget
            .try_reserve_or_evict(
                memory_budget::Category::Row,
                row_model::row(stored, t.fields.len()),
                &*self.evictor,
            )
            .map_err(StoreError::Memory)?;
        let row = decode_cells(
            map,
            t,
            p,
            &raw,
            &mut |r, _| tr.blob_bytes_budgeted(r, ctx).map(|bytes| bytes.to_vec()),
            fields.as_deref(),
            None,
        )
        .map_err(|e| at_row(e, u64::from(id)))?;
        drop(raw);
        lease
            .transfer(memory_budget::Category::Execution)
            .map_err(StoreError::Memory)?;
        Ok(Some((row, lease)))
    }
}

enum Slot {
    Free,
    Skipped,
    Row(Row),
}

impl RowMemory {
    #[allow(clippy::too_many_arguments, reason = "the parts of one row read")]
    fn read_into(
        &self,
        tr: &onecd::TableRead<'_>,
        ctx: &memory_budget::RequestMemory,
        scratch: &mut Option<onecd::RowScratch>,
        held: &mut Option<memory_budget::Reservation>,
        map: &StorageMap,
        t: &TableInfo,
        p: &PhysTable,
        id: onecd::RowId,
        fields: &Projection,
        prefix: Option<Prefix<'_>>,
        checks: &[Check],
    ) -> StoreResult<Slot> {
        let once = |scratch: &mut Option<onecd::RowScratch>,
                    held: &mut Option<memory_budget::Reservation>| {
            self.read_into_once(
                tr, ctx, scratch, held, map, t, p, id, fields, prefix, checks,
            )
        };
        match once(scratch, held) {
            Err(StoreError::Memory(m))
                if m.category == memory_budget::Category::Row
                    && m.reason == memory_budget::LimitReason::Category
                    && m.requested <= ctx.row_limit
                    && scratch.as_ref().is_some_and(|s| s.held() > 0) =>
            {
                if let Some(s) = scratch.as_mut() {
                    s.release();
                }
                once(scratch, held)
            }
            other => other,
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the parts of one row read")]
    fn read_into_once(
        &self,
        tr: &onecd::TableRead<'_>,
        ctx: &memory_budget::RequestMemory,
        scratch: &mut Option<onecd::RowScratch>,
        held: &mut Option<memory_budget::Reservation>,
        map: &StorageMap,
        t: &TableInfo,
        p: &PhysTable,
        id: onecd::RowId,
        fields: &Projection,
        prefix: Option<Prefix<'_>>,
        checks: &[Check],
    ) -> StoreResult<Slot> {
        let scratch = match scratch {
            Some(scratch) => scratch,
            None => scratch.insert(onecd::RowScratch::try_new(ctx).map_err(engine_err)?),
        };
        let (raw, blobs) = match tr.get_budgeted_into(id, ctx, scratch) {
            Ok(Some(read)) => read,
            Ok(None) => return Ok(Slot::Free),
            Err(e) => return Err(engine_err(e)),
        };
        if !passes(t, checks, raw).map_err(|e| at_row(e, u64::from(id)))? {
            return Ok(Slot::Skipped);
        }
        let mut stored = u64::from(tr.layout().record_len);
        for (k, f) in t.fields.iter().enumerate() {
            if !selected(fields, k) {
                continue;
            }
            for i in f.columns.clone() {
                if let Some(read) = p
                    .cols
                    .get(i)
                    .and_then(|&c| raw.get(c))
                    .and_then(|v| blob_read_len(v, prefix.map(|p| p.max)))
                {
                    stored = stored.saturating_add(read);
                }
            }
        }
        let need = row_model::row(stored, t.fields.len());
        let fits = held.as_ref().is_some_and(|lease| need <= lease.bytes());
        if fits {
            if need > ctx.row_limit {
                return Err(StoreError::Memory(memory_budget::MemoryLimit {
                    category: memory_budget::Category::Row,
                    requested: need,
                    reason: memory_budget::LimitReason::Category,
                }));
            }
        } else {
            *held = None;
            *held = Some(
                self.budget
                    .try_reserve_or_evict(memory_budget::Category::Row, need, &*self.evictor)
                    .map_err(StoreError::Memory)?,
            );
        }
        let row = decode_cells(
            map,
            t,
            p,
            raw,
            &mut |r, max| tr.blob_bytes_into(r, max, ctx, blobs).map(<[u8]>::to_vec),
            fields.as_deref(),
            prefix,
        )
        .map_err(|e| at_row(e, u64::from(id)));
        let row = match row {
            Ok(row) => row,
            Err(e) => {
                if !fits {
                    *held = None;
                }
                return Err(e);
            }
        };
        if !fits
            && let Some(lease) = held.as_ref()
            && let Err(e) = lease.transfer(memory_budget::Category::Execution)
        {
            *held = None;
            return Err(StoreError::Memory(e));
        }
        Ok(Slot::Row(row))
    }
}

struct ManagedScanIter<'a> {
    tr: onecd::TableRead<'a>,
    memory: RowMemory,
    ctx: memory_budget::RequestMemory,
    next: onecd::RowId,
    end: onecd::RowId,
    map: &'a StorageMap,
    t: &'a TableInfo,
    p: &'a PhysTable,
    fields: Projection,
    prefix: Option<Prefix<'a>>,
    current: Option<memory_budget::Reservation>,
    scratch: Option<onecd::RowScratch>,
    checks: Vec<Check>,
    stopped: bool,
}

impl Iterator for ManagedScanIter<'_> {
    type Item = StoreResult<Scanned<RowHandle>>;
    fn next(&mut self) -> Option<Self::Item> {
        while !self.stopped && self.next < self.end {
            let id = self.next;
            self.next += 1;
            match self.memory.read_into(
                &self.tr,
                &self.ctx,
                &mut self.scratch,
                &mut self.current,
                self.map,
                self.t,
                self.p,
                id,
                &self.fields,
                self.prefix,
                &self.checks,
            ) {
                Ok(Slot::Row(row)) => {
                    return Some(Ok(Scanned::Row(handle(id), row)));
                }
                Ok(Slot::Skipped) => return Some(Ok(Scanned::Skipped(handle(id)))),
                Ok(Slot::Free) => {}
                Err(e) => {
                    self.stopped = matches!(e, StoreError::Memory(_) | StoreError::SnapshotTooOld);
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

type Entries<'q> = Box<dyn onecd::index::BudgetedEntries + 'q>;
self_cell::self_cell!(
    struct ManagedRangeCell<'a> {
        owner: IndexCell<'a>,
        #[covariant]
        dependent: Entries,
    }
);

struct ManagedRangeIter<'a> {
    cell: ManagedRangeCell<'a>,
    memory: RowMemory,
    ctx: memory_budget::RequestMemory,
    map: &'a StorageMap,
    t: &'a TableInfo,
    p: &'a PhysTable,
    fields: Projection,
    prefix: Option<Prefix<'a>>,
    current: Option<memory_budget::Reservation>,
    scratch: Option<onecd::RowScratch>,
    key: Option<memory_budget::Scratch<Vec<u8>>>,
    checks: Vec<Check>,
}

impl ManagedRangeIter<'_> {
    fn next_entry(&mut self) -> Option<StoreResult<Scanned<IndexPosition>>> {
        let (memory, ctx, map, t, p, fields, prefix, scratch, current, checks) = (
            &self.memory,
            &self.ctx,
            self.map,
            self.t,
            self.p,
            &self.fields,
            self.prefix,
            &mut self.scratch,
            &mut self.current,
            &self.checks,
        );
        let key = match &mut self.key {
            Some(key) => key,
            None => {
                match memory_budget::Scratch::try_new(&ctx.budget, memory_budget::Category::Row) {
                    Ok(key) => self.key.insert(key),
                    Err(e) => return Some(Err(StoreError::Memory(e))),
                }
            }
        };
        let out = self.cell.with_dependent_mut(|ic, entries| {
            let row_id = match entries.next_into(key)? {
                Ok(row) => row,
                Err(e) => return Some(Err(engine_err(e))),
            };
            let tr = ic.borrow_owner();
            let position = || IndexPosition {
                key: key.get().cloned().unwrap_or_default(),
                row: handle(row_id),
            };
            Some(
                match memory.read_into(
                    tr, ctx, scratch, current, map, t, p, row_id, fields, prefix, checks,
                ) {
                    Ok(Slot::Row(row)) => Ok(Scanned::Row(position(), row)),
                    Ok(Slot::Skipped) => Ok(Scanned::Skipped(position())),
                    Ok(Slot::Free) => Err(StoreError::Backend(Box::new(Error::Inconsistent(
                        format!("{}: index entry for free slot {}", t.logical, row_id),
                    )))),
                    Err(e) => Err(e),
                },
            )
        })?;
        Some(out)
    }
}

struct ManagedRows<'a>(ManagedRangeIter<'a>);
impl Iterator for ManagedRows<'_> {
    type Item = StoreResult<(RowHandle, Row)>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.0.next_entry()? {
                Ok(Scanned::Row(pos, row)) => return Some(Ok((pos.row, row))),
                Ok(Scanned::Skipped(_)) => {}
                Err(e) => return Some(Err(e)),
            }
        }
    }
}
struct ManagedPositioned<'a>(ManagedRangeIter<'a>);
impl Iterator for ManagedPositioned<'_> {
    type Item = StoreResult<Scanned<IndexPosition>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next_entry()
    }
}

fn handle(r: onecd::RowId) -> RowHandle {
    RowHandle(u64::from(r))
}

struct ScanIter<'a> {
    cell: ScanCell<'a>,
    map: &'a StorageMap,
    t: &'a TableInfo,
    p: &'a PhysTable,
    fields: Projection,
    prefix: Option<Prefix<'a>>,
    checks: Vec<Check>,
}

impl Iterator for ScanIter<'_> {
    type Item = StoreResult<Scanned<RowHandle>>;
    fn next(&mut self) -> Option<Self::Item> {
        let (map, t, p, fields, prefix, checks) = (
            self.map,
            self.t,
            self.p,
            self.fields.as_deref(),
            self.prefix,
            &self.checks,
        );
        self.cell.with_dependent_mut(|tr, rows| {
            Some(match rows.next()? {
                Ok((id, row)) => match passes(t, checks, &row) {
                    Ok(true) => decode_cells(
                        map,
                        t,
                        p,
                        &row,
                        &mut |r, max| read_blob(tr, r, max),
                        fields,
                        prefix,
                    )
                    .map_err(|e| at_row(e, u64::from(id)))
                    .map(|row| Scanned::Row(handle(id), row)),
                    Ok(false) => Ok(Scanned::Skipped(handle(id))),
                    Err(e) => Err(at_row(e, u64::from(id))),
                },
                Err(e) => Err(engine_err(e)),
            })
        })
    }
}

struct RangeIter<'a> {
    cell: RangeCell<'a>,
    map: &'a StorageMap,
    t: &'a TableInfo,
    p: &'a PhysTable,
    fields: Projection,
    prefix: Option<Prefix<'a>>,
}

impl Iterator for RangeIter<'_> {
    type Item = StoreResult<(RowHandle, Row)>;
    fn next(&mut self) -> Option<Self::Item> {
        let (map, t, p, fields, prefix) = (
            self.map,
            self.t,
            self.p,
            self.fields.as_deref(),
            self.prefix,
        );
        self.cell.with_dependent_mut(|ic, rows| {
            let tr = ic.borrow_owner();
            Some(match rows.next()? {
                Ok((id, row)) => decode_cells(
                    map,
                    t,
                    p,
                    &row,
                    &mut |r, max| read_blob(tr, r, max),
                    fields,
                    prefix,
                )
                .map_err(|e| at_row(e, u64::from(id)))
                .map(|row| (handle(id), row)),
                Err(e) => Err(engine_err(e)),
            })
        })
    }
}

fn read_blob(
    tr: &onecd::TableRead<'_>,
    r: onecd::BlobRef,
    max: Option<usize>,
) -> onecd::Result<Vec<u8>> {
    match max {
        Some(max) => tr.blob_prefix(r, max),
        None => tr.blob_bytes(r),
    }
}

impl Read1cd {
    fn prefix(&self) -> Option<Prefix<'_>> {
        let max = self.prefix.load(std::sync::atomic::Ordering::Relaxed);
        (max > 0).then_some(Prefix {
            max,
            cuts: &self.cuts,
        })
    }

    fn table(&self, t: TableId) -> StoreResult<(&TableInfo, &PhysTable)> {
        let ti = self
            .map
            .try_table(t)
            .ok_or_else(|| StoreError::NotFound(format!("table #{}", t.0)))?;
        let p = usize::try_from(t.0)
            .ok()
            .and_then(|i| self.phys.tables.get(i))
            .ok_or_else(|| StoreError::NotFound(format!("table #{}", t.0)))?;
        Ok((ti, p))
    }

    fn reader(&self, p: &PhysTable) -> StoreResult<onecd::TableRead<'_>> {
        self.txn.table(&p.name).map_err(engine_err)
    }
}

fn engine_direction(dir: Direction) -> onecd::Direction {
    match dir {
        Direction::Forward => onecd::Direction::Forward,
        Direction::Backward => onecd::Direction::Backward,
    }
}

fn bound_key(
    ti: &TableInfo,
    ix: &IndexInfo,
    ir: &IndexRead<'_>,
    b: Bound<&[FieldValue]>,
    exact: bool,
) -> StoreResult<Bound<Vec<u8>>> {
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
    let mut ev = Vec::new();
    let mut part_start = 0;
    for (f, v) in ix.fields.iter().zip(vals) {
        let cols = fields::encode_bound(ti, *f, v).map_err(StoreError::from_schema)?;
        let fi = ti
            .field(*f)
            .ok_or_else(|| StoreError::NotFound(format!("field #{} of {}", f.0, ti.logical)))?;
        for (i, c) in fi.columns.clone().zip(cols) {
            let ty = ti
                .columns
                .get(i)
                .map(|c| c.ty)
                .ok_or_else(|| bad_value(ti, fi, "no column"))?;
            ev.push(colval::to_onecd(&c, ty).map_err(lib_err)?);
        }
        if exact {
            let key = ir.key(&ev).map_err(engine_err)?;
            if matches!(v, FieldValue::String(_))
                && key.get(part_start..).and_then(<[u8]>::last) != Some(&0)
            {
                return Err(StoreError::Unsupported(
                    "string value does not fit the index key".into(),
                ));
            }
            part_start = key.len();
        }
    }
    let key = ir.key(&ev).map_err(engine_err)?;
    Ok(if incl {
        Bound::Included(key)
    } else {
        Bound::Excluded(key)
    })
}

impl Read1cd {
    fn scan_projected(
        &self,
        t: TableId,
        after: RowHandle,
        fields: Projection,
        filter: RecordFilter<'_>,
        end: Option<RowHandle>,
    ) -> StoreResult<ScannedRows<'_>> {
        let (ti, p) = self.table(t)?;
        let checks = checks(ti, p, filter)?;
        let start = u32::try_from(after.0).ok().and_then(|n| n.checked_add(1));
        let bound = |slots: u32| {
            end.map_or(slots, |end| {
                u32::try_from(end.0).map_or(slots, |end| end.min(slots))
            })
        };
        if let Some(memory) = &self.memory {
            let tr = self.reader(p)?;
            let end = bound(tr.num_slots());
            return Ok(Box::new(ManagedScanIter {
                tr,
                memory: memory.clone(),
                ctx: memory.context(),
                next: start.unwrap_or(end).max(1),
                end,
                map: &self.map,
                t: ti,
                p,
                fields,
                prefix: self.prefix(),
                current: None,
                scratch: None,
                checks,
                stopped: false,
            }));
        }
        let cell = ScanCell::new(self.reader(p)?, |tr| match start {
            Some(start) => Box::new(tr.scan_range(start, bound(tr.num_slots()))) as Rows,
            None => Box::new(std::iter::empty()) as Rows,
        });
        Ok(Box::new(ScanIter {
            cell,
            map: &self.map,
            t: ti,
            p,
            fields,
            prefix: self.prefix(),
            checks,
        }))
    }

    fn range_projected(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        fields: Projection,
    ) -> StoreResult<RowIter<'_>> {
        let (ti, p) = self.table(t)?;
        let (ix, name) = match i {
            IndexId::Id => (ti.key_index.as_ref(), p.key_index.as_ref()),
            IndexId::Named(k) => (
                ti.indexes.get(usize::from(k)),
                p.indexes.get(usize::from(k)).and_then(Option::as_ref),
            ),
        };
        let Some(ix) = ix else {
            if i == IndexId::Id {
                return Err(StoreError::Unsupported(format!(
                    "{}: no key index",
                    ti.logical
                )));
            }
            return Err(StoreError::NotFound(format!(
                "index {i:?} of {}",
                ti.logical
            )));
        };
        let Some(name) = name else {
            return Err(StoreError::Unsupported(format!(
                "{}: no 1CD index serves {}",
                ti.logical, ix.name
            )));
        };
        let ic = IndexCell::try_new(self.reader(p)?, |tr| tr.index(name)).map_err(engine_err)?;
        let lo = bound_key(ti, ix, ic.borrow_dependent(), lo, false)?;
        let hi = bound_key(ti, ix, ic.borrow_dependent(), hi, false)?;
        let d = engine_direction(dir);
        if let Some(memory) = &self.memory {
            let ctx = memory.context();
            let cell = ManagedRangeCell::try_new(ic, |ic| {
                ic.borrow_dependent()
                    .entries_budgeted(
                        lo.as_ref().map(Vec::as_slice),
                        hi.as_ref().map(Vec::as_slice),
                        d,
                        &ctx,
                    )
                    .map(|it| Box::new(it) as Entries)
            })
            .map_err(engine_err)?;
            return Ok(Box::new(ManagedRows(ManagedRangeIter {
                cell,
                memory: memory.clone(),
                ctx,
                map: &self.map,
                t: ti,
                p,
                fields,
                prefix: self.prefix(),
                current: None,
                scratch: None,
                key: None,
                checks: Vec::new(),
            })));
        }
        let cell = RangeCell::try_new(ic, |ic| {
            ic.borrow_dependent()
                .range(
                    lo.as_ref().map(Vec::as_slice),
                    hi.as_ref().map(Vec::as_slice),
                    d,
                )
                .map(|it| Box::new(it) as Rows)
        })
        .map_err(engine_err)?;
        Ok(Box::new(RangeIter {
            cell,
            map: &self.map,
            t: ti,
            p,
            fields,
            prefix: self.prefix(),
        }))
    }

    fn get_projected(&self, t: TableId, h: RowHandle, fields: Projection) -> StoreResult<Row> {
        let (ti, p) = self.table(t)?;
        let missing = || StoreError::NotFound(format!("{} row {}", ti.logical, h.0));
        let id = u32::try_from(h.0).map_err(|_| missing())?;
        let tr = self.reader(p)?;
        if let Some(memory) = &self.memory {
            let mut held = self.last_get.lock().unwrap_or_else(|e| e.into_inner());
            *held = None;
            let ctx = memory.context();
            return match memory.read(&tr, &ctx, &self.map, ti, p, id, &fields) {
                Ok(Some((row, lease))) => {
                    *held = Some(lease);
                    Ok(row)
                }
                Ok(None) | Err(StoreError::NotFound(_)) => Err(missing()),
                Err(e) => Err(e),
            };
        }
        let row = match tr.get(id) {
            Ok(Some(r)) => r,
            Ok(None) | Err(onecd::Error::NotFound(onecd::NotFound::Row { .. })) => {
                return Err(missing());
            }
            Err(e) => return Err(engine_err(e)),
        };
        decode_cells(
            &self.map,
            ti,
            p,
            &row,
            &mut |r, max| read_blob(&tr, r, max),
            fields.as_deref(),
            None,
        )
        .map_err(|e| at_row(e, h.0))
    }
}

fn bound_value_kind(ti: &TableInfo, f: FieldId, v: &FieldValue) -> StoreResult<()> {
    let fi = ti.field(f);
    let col = fi
        .and_then(|fi| ti.columns.get(fi.columns.clone()))
        .and_then(|cols| match cols {
            [c] => Some((c.ty, c.role)),
            _ => None,
        });
    let unsupported = |why: String| Err(StoreError::Unsupported(why));
    match (col, v) {
        (Some((ColType::N(_, _), _)), FieldValue::Number(_))
        | (Some((ColType::DT, _)), FieldValue::Date(_)) => Ok(()),
        (Some((ColType::NC(n) | ColType::NVC(n), ColRole::Str)), FieldValue::String(s)) => {
            if s.encode_utf16().count() > n as usize {
                return unsupported(format!(
                    "string value is longer than the field ({n} characters)"
                ));
            }
            Ok(())
        }
        (Some((ColType::B(16), ColRole::Ref)), FieldValue::Ref { table, .. }) => {
            if fi.and_then(fields::single_ref_table) == Some(*table) {
                Ok(())
            } else {
                unsupported("reference value of another type".into())
            }
        }
        (Some((ColType::DT, _)), _) => unsupported("date value required".into()),
        (Some((ColType::NC(_) | ColType::NVC(_), ColRole::Str)), _) => {
            unsupported("string value required".into())
        }
        (Some((ColType::B(16), ColRole::Ref)), _) => unsupported("reference value required".into()),
        _ => unsupported("numeric value required".into()),
    }
}

struct Positioned<'a> {
    ti: &'a TableInfo,
    p: &'a PhysTable,
    ic: IndexCell<'a>,
    lo: Bound<Vec<u8>>,
    hi: Bound<Vec<u8>>,
    after: Option<onecd::index::IndexEntry>,
}

impl Read1cd {
    fn positioned(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        after: Option<&IndexPosition>,
    ) -> StoreResult<Positioned<'_>> {
        let (ti, p) = self.table(t)?;
        let access = self
            .indexes(t)?
            .into_iter()
            .find(|a| a.id == i)
            .ok_or_else(|| StoreError::Unsupported("index has no physical mapping".into()))?;
        let (ix, name) = match i {
            IndexId::Id => (ti.key_index.as_ref(), p.key_index.as_ref()),
            IndexId::Named(n) => (
                ti.indexes.get(usize::from(n)),
                p.indexes.get(usize::from(n)).and_then(Option::as_ref),
            ),
        };
        let (Some(ix), Some(name)) = (ix, name) else {
            return Err(StoreError::Unsupported("index unavailable".into()));
        };
        for b in [lo, hi] {
            let vals = match b {
                Bound::Unbounded => continue,
                Bound::Included(v) | Bound::Excluded(v) => v,
            };
            if vals.len() > access.fields.len() {
                return Err(StoreError::Unsupported("too many bound fields".into()));
            }
            for ((f, reason), v) in access.fields.iter().zip(vals) {
                if access.ref_pairs.contains(f) && matches!(v, FieldValue::Ref { .. }) {
                    fields::encode(ti, *f, v).map_err(StoreError::from_schema)?;
                    continue;
                }
                if let Some(reason) = reason {
                    return Err(StoreError::Unsupported(reason.clone()));
                }
                bound_value_kind(ti, *f, v)?;
                fields::encode(ti, *f, v).map_err(StoreError::from_schema)?;
            }
        }
        let ic = IndexCell::try_new(self.reader(p)?, |tr| tr.index(name)).map_err(engine_err)?;
        let lo = bound_key(ti, ix, ic.borrow_dependent(), lo, true)?;
        let hi = bound_key(ti, ix, ic.borrow_dependent(), hi, true)?;
        let after = after
            .map(|a| {
                u32::try_from(a.row.0)
                    .map(|row| onecd::index::IndexEntry {
                        key: a.key.clone(),
                        row,
                    })
                    .map_err(|_| StoreError::Unsupported("invalid index row position".into()))
            })
            .transpose()?;
        Ok(Positioned {
            ti,
            p,
            ic,
            lo,
            hi,
            after,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "range_after, its projection and filter"
    )]
    fn range_after_projected(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
        fields: Projection,
        filter: RecordFilter<'_>,
    ) -> StoreResult<ScannedPositioned<'_>> {
        let d = engine_direction(dir);
        let Positioned {
            ti,
            p,
            ic,
            lo,
            hi,
            after,
        } = self.positioned(t, i, lo, hi, after)?;
        let checks = checks(ti, p, filter)?;
        if let Some(memory) = &self.memory {
            let ctx = memory.context();
            let cell = ManagedRangeCell::try_new(ic, |ic| {
                ic.borrow_dependent()
                    .entries_after_budgeted(
                        lo.as_ref().map(Vec::as_slice),
                        hi.as_ref().map(Vec::as_slice),
                        d,
                        after.as_ref(),
                        &ctx,
                    )
                    .map(|it| Box::new(it) as Entries)
            })
            .map_err(engine_err)?;
            return Ok(Box::new(ManagedPositioned(ManagedRangeIter {
                cell,
                memory: memory.clone(),
                ctx,
                map: &self.map,
                t: ti,
                p,
                fields,
                prefix: self.prefix(),
                current: None,
                scratch: None,
                key: None,
                checks,
            })));
        }
        let cell = PositionedCell::try_new(ic, |ic| {
            ic.borrow_dependent()
                .range_after(
                    lo.as_ref().map(Vec::as_slice),
                    hi.as_ref().map(Vec::as_slice),
                    d,
                    after.as_ref(),
                )
                .map(|it| Box::new(it) as PositionedEngineRows)
        })
        .map_err(engine_err)?;
        Ok(Box::new(PositionedIter {
            cell,
            map: &self.map,
            t: ti,
            p,
            fields,
            prefix: self.prefix(),
            checks,
        }))
    }
}

impl ReadTxn for Read1cd {
    fn set_blob_prefix(&self, max_bytes: Option<usize>) {
        self.prefix
            .store(max_bytes.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }

    fn take_blob_cuts(&self) -> Vec<(FieldId, u64)> {
        std::mem::take(&mut *self.cuts.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn scan(&self, t: TableId) -> StoreResult<RowIter<'_>> {
        self.scan_after(t, RowHandle(0))
    }

    fn scan_after(&self, t: TableId, after: RowHandle) -> StoreResult<RowIter<'_>> {
        Ok(Box::new(rows_only(self.scan_projected(
            t,
            after,
            None,
            &[],
            None,
        )?)))
    }

    fn scan_fields(
        &self,
        t: TableId,
        fields: &[FieldId],
        after: Option<RowHandle>,
    ) -> StoreResult<RowIter<'_>> {
        let ti = self.table(t)?.0;
        Ok(Box::new(rows_only(self.scan_projected(
            t,
            after.unwrap_or(RowHandle(0)),
            projection(ti, fields),
            &[],
            None,
        )?)))
    }

    fn storage_map(&self) -> Option<&StorageMap> {
        Some(&self.map)
    }

    fn scan_fields_filtered(
        &self,
        t: TableId,
        fields: &[FieldId],
        after: RowHandle,
        filter: RecordFilter<'_>,
        end: RowHandle,
    ) -> StoreResult<ScannedRows<'_>> {
        let ti = self.table(t)?.0;
        self.scan_projected(t, after, projection(ti, fields), filter, Some(end))
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
        let ti = self.table(t)?.0;
        self.range_after_projected(t, i, lo, hi, dir, after, projection(ti, fields), filter)
    }

    fn scan_end(&self, t: TableId) -> StoreResult<RowHandle> {
        let (_, p) = self.table(t)?;
        Ok(handle(self.reader(p)?.num_slots().max(1)))
    }

    fn range(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
    ) -> StoreResult<RowIter<'_>> {
        self.range_projected(t, i, lo, hi, dir, None)
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
        let ti = self.table(t)?.0;
        self.range_projected(t, i, lo, hi, dir, projection(ti, fields))
    }

    fn get_fields(&self, t: TableId, h: RowHandle, fields: &[FieldId]) -> StoreResult<Row> {
        let ti = self.table(t)?.0;
        self.get_projected(t, h, projection(ti, fields))
    }

    fn next_used_after(&self, t: TableId, after: RowHandle) -> StoreResult<Option<RowHandle>> {
        let (_, p) = self.table(t)?;
        let tr = self.reader(p)?;
        let Some(start) = u32::try_from(after.0).ok().and_then(|n| n.checked_add(1)) else {
            return Ok(None);
        };
        let ctx = self.memory.as_ref().map(RowMemory::context);
        for id in start.max(1)..tr.num_slots() {
            let used = match &ctx {
                Some(ctx) => tr.get_budgeted(id, ctx).map(|r| r.is_some()),
                None => tr.get(id).map(|r| r.is_some()),
            };
            match used {
                Ok(true) => return Ok(Some(handle(id))),
                Ok(false) => {}
                Err(e) => return Err(engine_err(e)),
            }
        }
        Ok(None)
    }

    fn indexes(&self, t: TableId) -> StoreResult<Vec<IndexAccess>> {
        let (ti, p) = self.table(t)?;
        let tr = self.reader(p)?;
        let mut out = Vec::new();
        let all = ti
            .key_index
            .iter()
            .zip(p.key_index.iter())
            .map(|(ix, name)| (IndexId::Id, ix, Some(name)))
            .chain(ti.indexes.iter().enumerate().map(|(n, ix)| {
                (
                    IndexId::Named(n as u16),
                    ix,
                    p.indexes.get(n).and_then(Option::as_ref),
                )
            }));
        for (id, ix, name) in all {
            let Some(name) = name else {
                continue;
            };
            let ir = tr.index(name).map_err(engine_err)?;
            out.push(IndexAccess {
                id,
                name: ix.name.clone(),
                key_len: ir.key_len() as usize,
                fields: ix
                    .fields
                    .iter()
                    .map(|f| (*f, condition_reason(ti, *f, &tr, &ir)))
                    .collect(),
                ref_pairs: ix
                    .fields
                    .iter()
                    .copied()
                    .filter(|f| exact_ref_pair(ti, *f, &tr, &ir))
                    .collect(),
            });
        }
        Ok(out)
    }

    fn range_after(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        dir: Direction,
        after: Option<&IndexPosition>,
    ) -> StoreResult<PositionedRows<'_>> {
        Ok(Box::new(rows_only(self.range_after_projected(
            t,
            i,
            lo,
            hi,
            dir,
            after,
            None,
            &[],
        )?)))
    }

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
        let ti = self.table(t)?.0;
        Ok(Box::new(rows_only(self.range_after_projected(
            t,
            i,
            lo,
            hi,
            dir,
            after,
            projection(ti, fields),
            &[],
        )?)))
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
        let d = engine_direction(dir);
        let range = self.positioned(t, i, lo, hi, after)?;
        let ir = range.ic.borrow_dependent();
        let (lo, hi) = (
            range.lo.as_ref().map(Vec::as_slice),
            range.hi.as_ref().map(Vec::as_slice),
        );
        let entry = match &self.memory {
            Some(memory) => {
                let ctx = memory.context();
                ir.entries_after_budgeted(lo, hi, d, range.after.as_ref(), &ctx)
                    .map_err(engine_err)?
                    .next()
                    .transpose()
                    .map_err(engine_err)?
                    .map(|e| (e.key.clone(), e.row))
            }
            None => ir
                .entries_after(lo, hi, d, range.after.as_ref())
                .map_err(engine_err)?
                .next()
                .transpose()
                .map_err(engine_err)?
                .map(|e| (e.key, e.row)),
        };
        Ok(entry.map(|(key, row)| IndexPosition {
            key,
            row: handle(row),
        }))
    }

    fn get(&self, t: TableId, h: RowHandle) -> StoreResult<Row> {
        self.get_projected(t, h, None)
    }

    fn open_blob(
        &self,
        t: TableId,
        h: RowHandle,
        f: FieldId,
    ) -> StoreResult<Box<dyn ReadSeek + '_>> {
        let (ti, p) = self.table(t)?;
        let fi = ti
            .field(f)
            .ok_or_else(|| StoreError::NotFound(format!("field #{} of {}", f.0, ti.logical)))?;
        let col = match ti.columns.get(fi.columns.clone()) {
            Some([c]) if matches!(c.ty, ColType::NT | ColType::I) => fi.columns.start,
            _ => {
                return Err(StoreError::Unsupported(format!(
                    "{}.{}: not a blob",
                    ti.logical, fi.name
                )));
            }
        };
        let missing = || StoreError::NotFound(format!("{} row {}", ti.logical, h.0));
        let id = u32::try_from(h.0).map_err(|_| missing())?;
        let tr = self.reader(p)?;
        let name = p
            .cols
            .get(col)
            .and_then(|&c| tr.descr().fields.get(c))
            .map(|d| d.name.clone())
            .ok_or_else(|| bad_value(ti, fi, "no 1CD column"))?;
        let mut rd = tr.blob(id, &name).map_err(|e| match e {
            onecd::Error::NotFound(onecd::NotFound::Row { .. }) => missing(),
            other => engine_err(other),
        })?;
        let mut bytes = Vec::new();
        rd.read_to_end(&mut bytes)
            .map_err(|e| StoreError::Backend(Box::new(e)))?;
        Ok(Box::new(Cursor::new(bytes)))
    }

    fn row_count_estimate(&self, t: TableId) -> StoreResult<u64> {
        let (_, p) = self.table(t)?;
        Ok(u64::from(self.reader(p)?.num_slots().saturating_sub(1)))
    }
}
