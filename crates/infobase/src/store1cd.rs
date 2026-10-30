pub mod managed;
pub mod managed_table;
mod writer;

use std::io::{Cursor, Read};
use std::ops::Bound;
use std::sync::Arc;
use std::time::Duration;

use ibschema::columns::{ColRole, ColType, Part};
use ibschema::error::diagnostic_name;
use ibschema::fields::{self, FieldValue};
use ibschema::storage::{FieldId, FieldInfo, IndexId, IndexInfo, StorageMap, TableId, TableInfo};
use ibschema::store::{
    Direction, IndexAccess, IndexPosition, PositionedRows, ReadSeek, ReadTxn, RecordFilter, Row,
    RowHandle, RowIter, Scanned, ScannedPositioned, ScannedRows, Store, StoreError, StoreResult,
    WriteTxn, WriterGate, WriterTicket,
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
    checks: Checks,
}
impl Iterator for PositionedIter<'_> {
    type Item = StoreResult<Scanned<IndexPosition>>;
    fn next(&mut self) -> Option<Self::Item> {
        let fields = self.fields.as_deref();
        self.cell.with_dependent_mut(|ic, rows| {
            Some(match rows.next()? {
                Ok((entry, row)) => match passes(self.t, &self.checks.list, &row) {
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
                    .map(|row| Scanned::Row(IndexPosition::new(entry.key, handle(entry.row)), row)),
                    Ok(false) => Ok(Scanned::Skipped(IndexPosition::new(
                        entry.key,
                        handle(entry.row),
                    ))),
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

struct Checks {
    list: Vec<Check>,
    _paid: Option<memory_budget::Reservation>,
}

impl Checks {
    fn none() -> Checks {
        Checks {
            list: Vec::new(),
            _paid: None,
        }
    }
}

fn checks(t: &TableInfo, p: &PhysTable, filter: RecordFilter<'_>) -> StoreResult<Vec<Check>> {
    let mut list = Vec::with_capacity(filter.len());
    for &(f, want) in filter {
        let field = usize::from(f.0);
        let fi = t.fields.get(field).ok_or_else(|| {
            StoreError::NotFound(format!("field #{} of {}", f.0, diagnostic_name(&t.logical)))
        })?;
        let col = match t.columns.get(fi.columns.clone()) {
            Some([c]) if c.ty == ColType::L && !c.nullable => p.cols.get(fi.columns.start),
            _ => None,
        }
        .ok_or_else(|| {
            StoreError::Unsupported(format!(
                "{}.{}: a record filter takes a stored not null boolean field",
                diagnostic_name(&t.logical),
                diagnostic_name(&fi.name)
            ))
        })?;
        list.push(Check {
            field,
            col: *col,
            want,
        });
    }
    Ok(list)
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
                    None => StoreError::NotFound(format!(
                        "field #{} of {}",
                        c.field,
                        diagnostic_name(&t.logical)
                    )),
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
) -> Option<&'static str> {
    let Some(fi) = t.field(f) else {
        return Some("index field is absent");
    };
    if fi.nullable {
        return Some("nullable fields require NULL-aware conditions; unsupported in v1");
    }
    let Some([c]) = t.columns.get(fi.columns.clone()) else {
        return Some("composite fields are not supported for exact conditions");
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
                "only numeric, date, string and reference fields support exact conditions",
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
        return Some(reason);
    };
    if d.ty.is_string() && d.case_sensitive {
        return Some("case-sensitive string fields are not supported for exact conditions");
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
    write_indexes: Vec<(String, IndexId)>,
    writable: bool,
}

struct PhysMap {
    tables: Vec<PhysTable>,
}

fn same_name(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
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

fn constraint_indexes(d: &onecd::TableDescr, ti: &TableInfo) -> Vec<(String, IndexId)> {
    let logical = ti
        .key_index
        .iter()
        .map(|ix| (IndexId::Id, ix))
        .chain(
            ti.indexes
                .iter()
                .enumerate()
                .filter_map(|(k, ix)| u16::try_from(k).ok().map(|k| (IndexId::Named(k), ix))),
        )
        .filter(|(_, ix)| ix.unique)
        .collect::<Vec<_>>();
    d.indexes
        .iter()
        .filter(|ix| ix.unique)
        .filter_map(|physical| {
            let candidates = logical
                .iter()
                .filter(|(_, ix)| {
                    let columns = ix
                        .fields
                        .iter()
                        .filter_map(|f| ti.field(*f))
                        .flat_map(|f| ti.columns.get(f.columns.clone()).unwrap_or_default());
                    let columns = columns.collect::<Vec<_>>();
                    columns.len() == physical.fields.len()
                        && columns.iter().zip(&physical.fields).all(|(c, p)| {
                            same_name(&c.name, &p.name)
                                && d.field(&c.name).is_some_and(|(_, f)| {
                                    p.prefix_len == 0
                                        || matches!(
                                            c.ty,
                                            ColType::NC(_)
                                                | ColType::NVC(_)
                                                | ColType::B(_)
                                                | ColType::VB(_)
                                        ) && p.prefix_len >= f.length
                                })
                        })
                })
                .collect::<Vec<_>>();
            let id = match candidates.as_slice() {
                [only] => Some(only.0),
                _ => candidates
                    .iter()
                    .find(|(id, ix)| {
                        let width: usize = ix
                            .fields
                            .iter()
                            .take(ix.prefix)
                            .filter_map(|f| ti.field(*f))
                            .map(|f| f.columns.len())
                            .sum();
                        let suffix = "@".repeat(width);
                        let table = ti.phys.to_ascii_uppercase();
                        let expected = match id {
                            IndexId::Id if ti.owner.is_some() => format!("{table}_SK{suffix}"),
                            IndexId::Id if width == 0 => "_IDRREFIDX".to_owned(),
                            IndexId::Id => format!("{table}_S_HPK{suffix}"),
                            IndexId::Named(k) => format!("{table}_{}{suffix}", u32::from(*k) + 1),
                        };
                        same_name(&physical.name, &expected)
                    })
                    .map(|(id, _)| *id),
            }?;
            Some((physical.name.clone(), id))
        })
        .collect()
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
            let write_indexes = constraint_indexes(d, ti);
            let mut positions = cols.clone();
            positions.sort_unstable();
            let writable = positions.iter().copied().eq(0..d.fields.len());
            tables.push(PhysTable {
                name: ti.phys.clone(),
                cols,
                indexes,
                key_index,
                write_indexes,
                writable,
            });
        }
        Ok(PhysMap { tables })
    }
}

pub struct Store1cd {
    db: Arc<onecd::Database>,
    map: Arc<StorageMap>,
    phys: Arc<PhysMap>,
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

    fn transaction<T>(&self, txn: T, ticket: Option<WriterTicket>) -> Read1cd<T> {
        Read1cd {
            txn,
            db: Arc::clone(&self.db),
            map: Arc::clone(&self.map),
            phys: Arc::clone(&self.phys),
            memory: self.db.memory_budget().map(|budget| RowMemory {
                budget: budget.clone(),
                evictor: Arc::clone(&self.db),
            }),
            last_get: std::sync::Mutex::new(None),
            prefix: std::sync::atomic::AtomicUsize::new(0),
            cuts: std::sync::Mutex::new(Vec::new()),
            _ticket: ticket,
        }
    }
}

impl Store for Store1cd {
    fn storage(&self) -> &StorageMap {
        &self.map
    }
    fn read(&self) -> StoreResult<Box<dyn ReadTxn + Send>> {
        Ok(Box::new(self.transaction(self.db.read(), None)))
    }
    fn write(&self, wait: Duration) -> StoreResult<Box<dyn WriteTxn>> {
        self.begin_write(wait)
    }
}

fn engine_err(e: onecd::Error) -> StoreError {
    match e {
        onecd::Error::Memory(memory_budget::BudgetError::Limit(m)) => StoreError::Memory(m),
        onecd::Error::NativeRestart { memory } => StoreError::NativeRestart { memory },
        onecd::Error::Cancelled => StoreError::Cancelled,
        onecd::Error::SnapshotTooOld => StoreError::SnapshotTooOld,
        onecd::Error::TxnTooLarge => StoreError::TxnTooLarge,
        onecd::Error::NotFound(n) => StoreError::NotFound(n.to_string()),
        other => StoreError::Backend(Box::new(other)),
    }
}

fn bad_value(t: &TableInfo, f: &FieldInfo, why: impl Into<String>) -> StoreError {
    StoreError::BadValue {
        table: diagnostic_name(&t.logical),
        field: diagnostic_name(&f.name),
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

struct Mask {
    fields: Vec<bool>,
    _paid: Option<memory_budget::Reservation>,
}

impl std::ops::Deref for Mask {
    type Target = [bool];
    fn deref(&self) -> &[bool] {
        &self.fields
    }
}

type Projection = Option<Mask>;

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

trait TableSource {
    fn table(&self, name: &str) -> StoreResult<onecd::TableRead<'_>>;
    fn set_read_budget(&mut self, budget: memory_budget::MemoryBudget) -> StoreResult<()>;
}

impl TableSource for onecd::ReadTxn {
    fn table(&self, name: &str) -> StoreResult<onecd::TableRead<'_>> {
        self.table(name).map_err(engine_err)
    }
    fn set_read_budget(&mut self, budget: memory_budget::MemoryBudget) -> StoreResult<()> {
        self.set_read_budget(budget);
        Ok(())
    }
}

struct Read1cd<T = onecd::ReadTxn> {
    txn: T,
    db: Arc<onecd::Database>,
    map: Arc<StorageMap>,
    phys: Arc<PhysMap>,
    memory: Option<RowMemory>,
    last_get: std::sync::Mutex<Option<memory_budget::Reservation>>,
    prefix: std::sync::atomic::AtomicUsize,
    cuts: std::sync::Mutex<Vec<(FieldId, u64)>>,
    _ticket: Option<WriterTicket>,
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
    checks: Checks,
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
                &self.checks.list,
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
    checks: Checks,
    positions: bool,
}

impl ManagedRangeIter<'_> {
    fn next_entry(&mut self) -> Option<StoreResult<Scanned<IndexPosition>>> {
        let positions = self.positions;
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
            &self.checks.list,
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
            let paid = if positions {
                let bytes = key.get().map_or(0, Vec::len);
                match ctx.budget.try_reserve(
                    memory_budget::Category::Execution,
                    u64::try_from(bytes).unwrap_or(u64::MAX),
                ) {
                    Ok(paid) => Some(paid),
                    Err(e) => return Some(Err(StoreError::Memory(e))),
                }
            } else {
                None
            };
            let position = move || match paid {
                Some(paid) => IndexPosition::paid(
                    key.get().cloned().unwrap_or_default(),
                    handle(row_id),
                    paid,
                ),
                None => IndexPosition::new(Vec::new(), handle(row_id)),
            };
            Some(
                match memory.read_into(
                    tr, ctx, scratch, current, map, t, p, row_id, fields, prefix, checks,
                ) {
                    Ok(Slot::Row(row)) => Ok(Scanned::Row(position(), row)),
                    Ok(Slot::Skipped) => Ok(Scanned::Skipped(position())),
                    Ok(Slot::Free) => {
                        Err(StoreError::Backend(Box::new(Error::Inconsistent(format!(
                            "{}: index entry for free slot {}",
                            diagnostic_name(&t.logical),
                            row_id
                        )))))
                    }
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
    checks: Checks,
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
            &self.checks.list,
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

impl<T: TableSource> Read1cd<T> {
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
        self.txn.table(&p.name)
    }

    fn key_request(&self) -> Option<onecd::KeyRequest> {
        onecd::KeyRequest::when_bound(self.memory.as_ref().map(|m| &m.budget))
    }

    fn prepare(&self, bytes: u64) -> StoreResult<Option<memory_budget::Reservation>> {
        self.memory
            .as_ref()
            .map(|m| {
                m.budget
                    .try_reserve(memory_budget::Category::Execution, bytes)
                    .map_err(StoreError::Memory)
            })
            .transpose()
    }

    fn projection(&self, t: &TableInfo, fields: &[FieldId]) -> StoreResult<Projection> {
        let paid = self.prepare(u64::try_from(t.fields.len()).unwrap_or(u64::MAX))?;
        let mut mask = vec![false; t.fields.len()];
        for f in fields {
            if let Some(m) = mask.get_mut(usize::from(f.0)) {
                *m = true;
            }
        }
        Ok(Some(Mask {
            fields: mask,
            _paid: paid,
        }))
    }

    fn checks(
        &self,
        t: &TableInfo,
        p: &PhysTable,
        filter: RecordFilter<'_>,
    ) -> StoreResult<Checks> {
        let bytes = filter.len().saturating_mul(std::mem::size_of::<Check>());
        let paid = match bytes {
            0 => None,
            _ => self.prepare(u64::try_from(bytes).unwrap_or(u64::MAX))?,
        };
        Ok(Checks {
            list: checks(t, p, filter)?,
            _paid: paid,
        })
    }
}

fn engine_direction(dir: Direction) -> onecd::Direction {
    match dir {
        Direction::Forward => onecd::Direction::Forward,
        Direction::Backward => onecd::Direction::Backward,
    }
}

enum BoundKey {
    Legacy(Vec<u8>),
    Paid(memory_budget::BudgetedVec<u8>),
}

impl BoundKey {
    fn bytes(&self) -> &[u8] {
        match self {
            BoundKey::Legacy(key) => key,
            BoundKey::Paid(key) => key.as_slice(),
        }
    }
}

fn index_key(
    ir: &IndexRead<'_>,
    vals: &[onecd::Value],
    keys: Option<&onecd::KeyRequest>,
) -> StoreResult<BoundKey> {
    match keys {
        Some(keys) => ir.key_budgeted(vals, keys).map(BoundKey::Paid),
        None => ir.key(vals).map(BoundKey::Legacy),
    }
    .map_err(engine_err)
}

fn admit_conversion(
    keys: Option<&onecd::KeyRequest>,
    ledger: Option<&memory_budget::MemoryBudget>,
    bytes: impl FnOnce() -> u64,
) -> StoreResult<Option<memory_budget::Reservation>> {
    Ok(match (keys, ledger) {
        (Some(keys), _) => Some(keys.try_reserve(bytes()).map_err(engine_err)?),
        (None, Some(ledger)) => Some(
            ledger
                .try_reserve(memory_budget::Category::Execution, bytes())
                .map_err(StoreError::Memory)?,
        ),
        (None, None) => None,
    })
}

fn conversion_bytes(ti: &TableInfo, ix: &IndexInfo, vals: &[FieldValue], columns: usize) -> u64 {
    let mut bytes = columns.saturating_mul(std::mem::size_of::<onecd::Value>());
    for (f, v) in ix.fields.iter().zip(vals) {
        bytes = bytes.saturating_add(fields::encode_bound_bytes(ti, *f, v));
        if let FieldValue::String(s) = v
            && ti
                .field(*f)
                .and_then(|fi| ti.columns.get(fi.columns.clone()))
                .is_some_and(|cols| cols.iter().any(|c| c.ty == ColType::NT))
        {
            bytes = bytes.saturating_add(s.len().saturating_mul(colval::NT_COPY_FACTOR));
        }
    }
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

fn bound_key(
    ti: &TableInfo,
    ix: &IndexInfo,
    ir: &IndexRead<'_>,
    b: Bound<&[FieldValue]>,
    exact: bool,
    keys: Option<&onecd::KeyRequest>,
    ledger: Option<&memory_budget::MemoryBudget>,
) -> StoreResult<Bound<BoundKey>> {
    let (vals, incl) = match b {
        Bound::Unbounded => return Ok(Bound::Unbounded),
        Bound::Included(v) => (v, true),
        Bound::Excluded(v) => (v, false),
    };
    if vals.len() > ix.fields.len() {
        return Err(StoreError::Unsupported(format!(
            "{}: bound of {} values over index {} of {} fields",
            diagnostic_name(&ti.logical),
            vals.len(),
            diagnostic_name(&ix.name),
            ix.fields.len()
        )));
    }
    let columns: usize = ix
        .fields
        .iter()
        .zip(vals)
        .filter_map(|(f, _)| ti.field(*f))
        .map(|fi| fi.columns.len())
        .sum();
    let _admitted = admit_conversion(keys, ledger, || conversion_bytes(ti, ix, vals, columns))?;
    let mut ev = Vec::with_capacity(columns);
    let mut part_start = 0;
    for (f, v) in ix.fields.iter().zip(vals) {
        let cols = fields::encode_bound(ti, *f, v).map_err(StoreError::from_schema)?;
        let fi = ti.field(*f).ok_or_else(|| {
            StoreError::NotFound(format!(
                "field #{} of {}",
                f.0,
                diagnostic_name(&ti.logical)
            ))
        })?;
        for (i, c) in fi.columns.clone().zip(cols) {
            let ty = ti
                .columns
                .get(i)
                .map(|c| c.ty)
                .ok_or_else(|| bad_value(ti, fi, "no column"))?;
            ev.push(colval::into_onecd(c, ty).map_err(lib_err)?);
        }
        if exact {
            let key = index_key(ir, &ev, keys)?;
            let key = key.bytes();
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
    let key = index_key(ir, &ev, keys)?;
    Ok(if incl {
        Bound::Included(key)
    } else {
        Bound::Excluded(key)
    })
}

impl<T: TableSource> Read1cd<T> {
    fn scan_projected(
        &self,
        t: TableId,
        after: RowHandle,
        fields: Projection,
        filter: RecordFilter<'_>,
        end: Option<RowHandle>,
    ) -> StoreResult<ScannedRows<'_>> {
        let (ti, p) = self.table(t)?;
        let checks = self.checks(ti, p, filter)?;
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
                    diagnostic_name(&ti.logical)
                )));
            }
            return Err(StoreError::NotFound(format!(
                "index {i:?} of {}",
                diagnostic_name(&ti.logical)
            )));
        };
        let Some(name) = name else {
            return Err(StoreError::Unsupported(format!(
                "{}: no 1CD index serves {}",
                diagnostic_name(&ti.logical),
                diagnostic_name(&ix.name)
            )));
        };
        let ic = IndexCell::try_new(self.reader(p)?, |tr| tr.index(name)).map_err(engine_err)?;
        let keys = self.key_request();
        let ledger = self.memory.as_ref().map(|m| &m.budget);
        let lo = bound_key(
            ti,
            ix,
            ic.borrow_dependent(),
            lo,
            false,
            keys.as_ref(),
            ledger,
        )?;
        let hi = bound_key(
            ti,
            ix,
            ic.borrow_dependent(),
            hi,
            false,
            keys.as_ref(),
            ledger,
        )?;
        let d = engine_direction(dir);
        if let Some(memory) = &self.memory {
            let ctx = memory.context();
            let cell = ManagedRangeCell::try_new(ic, |ic| {
                ic.borrow_dependent()
                    .entries_budgeted(
                        lo.as_ref().map(BoundKey::bytes),
                        hi.as_ref().map(BoundKey::bytes),
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
                checks: Checks::none(),
                positions: false,
            })));
        }
        let cell = RangeCell::try_new(ic, |ic| {
            ic.borrow_dependent()
                .range(
                    lo.as_ref().map(BoundKey::bytes),
                    hi.as_ref().map(BoundKey::bytes),
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
        let missing =
            || StoreError::NotFound(format!("{} row {}", diagnostic_name(&ti.logical), h.0));
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
    lo: Bound<BoundKey>,
    hi: Bound<BoundKey>,
    after: Option<PaidEntry>,
}

struct PaidEntry {
    entry: onecd::index::IndexEntry,
    _paid: Option<memory_budget::Reservation>,
}

impl<T: TableSource> Read1cd<T> {
    fn positioned(
        &self,
        t: TableId,
        i: IndexId,
        lo: Bound<&[FieldValue]>,
        hi: Bound<&[FieldValue]>,
        after: Option<&IndexPosition>,
    ) -> StoreResult<Positioned<'_>> {
        let (ti, p) = self.table(t)?;
        let (ix, name) = match i {
            IndexId::Id => (ti.key_index.as_ref(), p.key_index.as_ref()),
            IndexId::Named(n) => (
                ti.indexes.get(usize::from(n)),
                p.indexes.get(usize::from(n)).and_then(Option::as_ref),
            ),
        };
        let (Some(ix), Some(name)) = (ix, name) else {
            return Err(StoreError::Unsupported(
                "index has no physical mapping".into(),
            ));
        };
        let ic = IndexCell::try_new(self.reader(p)?, |tr| tr.index(name)).map_err(engine_err)?;
        let keys = self.key_request();
        let ledger = self.memory.as_ref().map(|m| &m.budget);
        let checked = |f: FieldId, v: &FieldValue| -> StoreResult<()> {
            let _admitted = admit_conversion(keys.as_ref(), ledger, || {
                u64::try_from(fields::encode_bound_bytes(ti, f, v)).unwrap_or(u64::MAX)
            })?;
            fields::encode(ti, f, v).map_err(StoreError::from_schema)?;
            Ok(())
        };
        for b in [lo, hi] {
            let vals = match b {
                Bound::Unbounded => continue,
                Bound::Included(v) | Bound::Excluded(v) => v,
            };
            if vals.len() > ix.fields.len() {
                return Err(StoreError::Unsupported("too many bound fields".into()));
            }
            for (f, v) in ix.fields.iter().zip(vals) {
                let (tr, ir) = (ic.borrow_owner(), ic.borrow_dependent());
                if matches!(v, FieldValue::Ref { .. }) && exact_ref_pair(ti, *f, tr, ir) {
                    checked(*f, v)?;
                    continue;
                }
                if let Some(reason) = condition_reason(ti, *f, tr, ir) {
                    return Err(StoreError::Unsupported(reason.into()));
                }
                bound_value_kind(ti, *f, v)?;
                checked(*f, v)?;
            }
        }
        let lo = bound_key(
            ti,
            ix,
            ic.borrow_dependent(),
            lo,
            true,
            keys.as_ref(),
            ledger,
        )?;
        let hi = bound_key(
            ti,
            ix,
            ic.borrow_dependent(),
            hi,
            true,
            keys.as_ref(),
            ledger,
        )?;
        let after = match after {
            None => None,
            Some(a) => {
                let row = u32::try_from(a.row.0)
                    .map_err(|_| StoreError::Unsupported("invalid index row position".into()))?;
                let paid = admit_conversion(keys.as_ref(), ledger, || {
                    u64::try_from(a.key.len()).unwrap_or(u64::MAX)
                })?;
                let entry = onecd::index::IndexEntry {
                    key: a.key.clone(),
                    row,
                };
                Some(PaidEntry { entry, _paid: paid })
            }
        };
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
        let after_entry = after.as_ref().map(|a| &a.entry);
        let checks = self.checks(ti, p, filter)?;
        if let Some(memory) = &self.memory {
            let ctx = memory.context();
            let cell = ManagedRangeCell::try_new(ic, |ic| {
                ic.borrow_dependent()
                    .entries_after_budgeted(
                        lo.as_ref().map(BoundKey::bytes),
                        hi.as_ref().map(BoundKey::bytes),
                        d,
                        after_entry,
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
                positions: true,
            })));
        }
        let cell = PositionedCell::try_new(ic, |ic| {
            ic.borrow_dependent()
                .range_after(
                    lo.as_ref().map(BoundKey::bytes),
                    hi.as_ref().map(BoundKey::bytes),
                    d,
                    after_entry,
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

fn mapped_indexes<'a>(
    ti: &'a TableInfo,
    p: &'a PhysTable,
) -> impl Iterator<Item = (IndexId, &'a IndexInfo, &'a String)> + 'a {
    ti.key_index
        .iter()
        .zip(p.key_index.iter())
        .map(|(ix, name)| (IndexId::Id, ix, name))
        .chain(ti.indexes.iter().enumerate().filter_map(|(n, ix)| {
            p.indexes
                .get(n)
                .and_then(Option::as_ref)
                .map(|name| (IndexId::Named(n as u16), ix, name))
        }))
}

const REASON_BYTES: usize = 128;

fn access_bytes(ix: &IndexInfo) -> u64 {
    let per_field = std::mem::size_of::<(FieldId, Option<String>)>()
        + REASON_BYTES
        + std::mem::size_of::<FieldId>();
    u64::try_from(
        ix.name
            .len()
            .saturating_add(ix.fields.len().saturating_mul(per_field)),
    )
    .unwrap_or(u64::MAX)
}

fn access_of(
    ti: &TableInfo,
    tr: &onecd::TableRead<'_>,
    id: IndexId,
    ix: &IndexInfo,
    name: &str,
    paid: Option<memory_budget::Reservation>,
) -> StoreResult<IndexAccess> {
    let ir = tr.index(name).map_err(engine_err)?;
    let mut fields = Vec::with_capacity(ix.fields.len());
    for f in &ix.fields {
        let reason = condition_reason(ti, *f, tr, &ir);
        debug_assert!(reason.is_none_or(|r| r.len() <= REASON_BYTES));
        fields.push((*f, reason.map(str::to_owned)));
    }
    let mut ref_pairs = Vec::with_capacity(ix.fields.len());
    ref_pairs.extend(
        ix.fields
            .iter()
            .copied()
            .filter(|f| exact_ref_pair(ti, *f, tr, &ir)),
    );
    Ok(IndexAccess {
        id,
        name: ix.name.clone(),
        key_len: ir.key_len() as usize,
        fields,
        ref_pairs,
        paid,
    })
}

impl<T: TableSource> ReadTxn for Read1cd<T> {
    fn read_memory(&mut self, limit: u64) -> StoreResult<memory_budget::MemoryBudget> {
        let budget = match &self.memory {
            Some(memory) => memory
                .budget
                .try_scope(memory_budget::Category::Execution, limit),
            None => memory_budget::MemoryBudget::try_new(memory_budget::Limits::new(limit)),
        }
        .map_err(StoreError::from_budget)?;
        self.txn.set_read_budget(budget.clone())?;
        self.memory = Some(RowMemory {
            budget: budget.clone(),
            evictor: self.db.clone(),
        });
        Ok(budget)
    }
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
            self.projection(ti, fields)?,
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
        self.scan_projected(t, after, self.projection(ti, fields)?, filter, Some(end))
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
        self.range_after_projected(
            t,
            i,
            lo,
            hi,
            dir,
            after,
            self.projection(ti, fields)?,
            filter,
        )
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
        self.range_projected(t, i, lo, hi, dir, self.projection(ti, fields)?)
    }

    fn get_fields(&self, t: TableId, h: RowHandle, fields: &[FieldId]) -> StoreResult<Row> {
        let ti = self.table(t)?.0;
        self.get_projected(t, h, self.projection(ti, fields)?)
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
        let count = mapped_indexes(ti, p).count();
        let list = u64::try_from(count.saturating_mul(std::mem::size_of::<IndexAccess>()))
            .unwrap_or(u64::MAX);
        let bytes = mapped_indexes(ti, p)
            .fold(list, |sum, (_, ix, _)| sum.saturating_add(access_bytes(ix)));
        let mut paid = self.prepare(bytes)?;
        let tr = self.reader(p)?;
        let mut out = Vec::with_capacity(count);
        for (k, (id, ix, name)) in mapped_indexes(ti, p).enumerate() {
            let own = access_bytes(ix).saturating_add(if k == 0 { list } else { 0 });
            let share = match &mut paid {
                Some(paid) => Some(paid.split(own).map_err(StoreError::Memory)?),
                None => None,
            };
            out.push(access_of(ti, &tr, id, ix, name, share)?);
        }
        Ok(out)
    }

    fn index_named(&self, t: TableId, name: &str) -> StoreResult<Option<IndexAccess>> {
        let (ti, p) = self.table(t)?;
        let Some((id, ix, phys)) =
            mapped_indexes(ti, p).find(|(_, ix, _)| ix.name.eq_ignore_ascii_case(name))
        else {
            return Ok(None);
        };
        let paid = self.prepare(access_bytes(ix))?;
        let tr = self.reader(p)?;
        access_of(ti, &tr, id, ix, phys, paid).map(Some)
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
            self.projection(ti, fields)?,
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
            range.lo.as_ref().map(BoundKey::bytes),
            range.hi.as_ref().map(BoundKey::bytes),
        );
        let after = range.after.as_ref().map(|a| &a.entry);
        match &self.memory {
            Some(memory) => {
                let ctx = memory.context();
                let Some(e) = ir
                    .entries_after_budgeted(lo, hi, d, after, &ctx)
                    .map_err(engine_err)?
                    .next()
                    .transpose()
                    .map_err(engine_err)?
                else {
                    return Ok(None);
                };
                let paid = ctx
                    .budget
                    .try_reserve(
                        memory_budget::Category::Execution,
                        u64::try_from(e.key.len()).unwrap_or(u64::MAX),
                    )
                    .map_err(StoreError::Memory)?;
                Ok(Some(IndexPosition::paid(
                    e.key.clone(),
                    handle(e.row),
                    paid,
                )))
            }
            None => Ok(ir
                .entries_after(lo, hi, d, after)
                .map_err(engine_err)?
                .next()
                .transpose()
                .map_err(engine_err)?
                .map(|e| IndexPosition::new(e.key, handle(e.row)))),
        }
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
        let fi = ti.field(f).ok_or_else(|| {
            StoreError::NotFound(format!(
                "field #{} of {}",
                f.0,
                diagnostic_name(&ti.logical)
            ))
        })?;
        let col = match ti.columns.get(fi.columns.clone()) {
            Some([c]) if matches!(c.ty, ColType::NT | ColType::I) => fi.columns.start,
            _ => {
                return Err(StoreError::Unsupported(format!(
                    "{}.{}: not a blob",
                    diagnostic_name(&ti.logical),
                    diagnostic_name(&fi.name)
                )));
            }
        };
        let missing =
            || StoreError::NotFound(format!("{} row {}", diagnostic_name(&ti.logical), h.0));
        let id = u32::try_from(h.0).map_err(|_| missing())?;
        let tr = self.reader(p)?;
        if let Some(memory) = &self.memory {
            let context = memory.context();
            let raw = tr
                .get_budgeted(id, &context)
                .map_err(engine_err)?
                .ok_or_else(missing)?;
            let value = p.cols.get(col).and_then(|&c| raw.get(c));
            let blob = match value {
                Some(
                    onecd::Value::NText(onecd::BlobVal::Ref(r))
                    | onecd::Value::Image(onecd::BlobVal::Ref(r)),
                ) => *r,
                Some(onecd::Value::Null) => onecd::BlobRef { start: 0, len: 0 },
                _ => {
                    return Err(bad_value(
                        ti,
                        fi,
                        "the column does not hold a BLOB reference",
                    ));
                }
            };
            drop(raw);
            let bytes = tr
                .blob_bytes_managed(
                    blob,
                    &memory.budget,
                    memory_budget::Category::Execution,
                    u64::MAX,
                )
                .map_err(engine_err)?;
            return ibschema::store::charged_blob(bytes);
        }
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
