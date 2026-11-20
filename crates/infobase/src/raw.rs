pub(crate) mod managed;
pub use managed::{
    dbnames_text_in_budgeted, dbschema_text_in_budgeted, folder_file_data_in_budgeted,
};

use crate::colval::value_kind;
use crate::error::{Error, Result};
use onecd::{BlobVal, Database, DateTime, Numeric, OpenOptions, ReadTxn, RowId, TableRead, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderFile {
    pub name: String,
    pub creation: DateTime,
    pub modified: DateTime,
    pub attributes: Numeric,
    pub data_size: Numeric,
    pub data: Vec<u8>,
}

pub struct RawIb {
    db: Arc<Database>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStorage {
    pub schema_id: u32,
    pub status: u32,
    pub current: Vec<u8>,
}

pub fn inflate_raw(b: &[u8]) -> Result<Vec<u8>> {
    zraw::inflate(b, zraw::DEFAULT_LIMIT).map_err(|e| match e {
        zraw::Error::TooLarge(_) => Error::Unsupported(e.to_string()),
        zraw::Error::Zlib(_) => Error::Unsupported(e.to_string()),
        _ => Error::Inconsistent(e.to_string()),
    })
}

struct Part {
    partno: u64,
    file: FolderFile,
}

#[derive(Debug, Clone)]
pub struct FolderEntry {
    pub name: String,
    pub creation: DateTime,
    pub modified: DateTime,
    pub attributes: Numeric,
    pub data_size: Numeric,
    parts: Vec<RowId>,
}

pub struct FolderIndex {
    cell: TxnTable,
    table: String,
    pub entries: Vec<FolderEntry>,
}

self_cell::self_cell!(
    struct TxnTable {
        owner: ReadTxn,
        #[covariant]
        dependent: TableRead,
    }
);

impl FolderIndex {
    pub fn data_bounded(&self, i: usize, limit: usize) -> Result<Vec<u8>> {
        folder_data_bounded(
            self.cell.borrow_dependent(),
            &self.table,
            &self.entries,
            i,
            limit,
        )
    }

    pub fn data(&self, i: usize) -> Result<Vec<u8>> {
        folder_data(self.cell.borrow_dependent(), &self.table, &self.entries, i)
    }
}

fn folder_data_bounded(
    t: &TableRead<'_>,
    table: &str,
    entries: &[FolderEntry],
    i: usize,
    limit: usize,
) -> Result<Vec<u8>> {
    use std::io::Read;
    let e = entries.get(i).ok_or_else(|| bad(table, "no such entry"))?;
    if partno_of(&e.data_size, table)? > limit as u64 {
        return Err(bad(table, "entry size exceeds product limit"));
    }
    let c_data = t
        .descr()
        .field("BINARYDATA")
        .map(|(i, _)| i)
        .ok_or_else(|| bad(table, "no column BINARYDATA"))?;
    let mut out = Vec::new();
    for &slot in &e.parts {
        let row = t.get(slot)?.ok_or_else(|| bad(table, "part vanished"))?;
        match row.get(c_data) {
            Some(Value::Image(BlobVal::Data(b)) | Value::Binary(b)) => {
                if b.len() > limit.saturating_sub(out.len()) {
                    return Err(bad(table, "entry size exceeds product limit"));
                }
                out.extend_from_slice(b);
            }
            Some(Value::Image(BlobVal::Ref(_))) => {
                let mut reader = t.blob(slot, "BINARYDATA")?;
                let mut buf = [0; 8192];
                loop {
                    let n = reader
                        .read(&mut buf)
                        .map_err(|e| bad(table, &format!("blob read: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    if n > limit.saturating_sub(out.len()) {
                        return Err(bad(table, "entry size exceeds product limit"));
                    }
                    out.extend_from_slice(
                        buf.get(..n)
                            .ok_or_else(|| bad(table, "invalid reader length"))?,
                    );
                }
            }
            Some(v) => return Err(bad(table, &format!("BINARYDATA is {}", value_kind(v)))),
            None => return Err(bad(table, "short row")),
        }
    }
    Ok(out)
}

fn folder_data(
    t: &TableRead<'_>,
    table: &str,
    entries: &[FolderEntry],
    i: usize,
) -> Result<Vec<u8>> {
    let e = entries.get(i).ok_or_else(|| bad(table, "no such entry"))?;
    let c_data = t
        .descr()
        .field("BINARYDATA")
        .map(|(i, _)| i)
        .ok_or_else(|| bad(table, "no column BINARYDATA"))?;
    let mut out = Vec::new();
    for &slot in &e.parts {
        let row = t
            .get(slot)?
            .ok_or_else(|| bad(table, &format!("{}: part vanished", e.name)))?;
        match row.get(c_data) {
            Some(Value::Image(BlobVal::Data(b)) | Value::Binary(b)) => out.extend_from_slice(b),
            Some(Value::Image(BlobVal::Ref(r))) => out.extend_from_slice(&t.blob_bytes(*r)?),
            Some(v) => return Err(bad(table, &format!("BINARYDATA is {}", value_kind(v)))),
            None => return Err(bad(table, "short row")),
        }
    }
    Ok(out)
}

pub(crate) struct FolderTxnIndex<'a> {
    table: TableRead<'a>,
    name: String,
    pub entries: Vec<FolderEntry>,
}

impl<'a> FolderTxnIndex<'a> {
    pub fn read(txn: &'a ReadTxn, name: &str) -> Result<Self> {
        let table = txn.table(name)?;
        let entries = folder_entries(&table, name)?;
        Ok(Self {
            table,
            name: name.to_string(),
            entries,
        })
    }

    pub fn read_bounded(
        txn: &'a ReadTxn,
        name: &str,
        max_rows: usize,
        max_name: usize,
    ) -> Result<Self> {
        let table = txn.table(name)?;
        let column = table
            .descr()
            .field("FILENAME")
            .ok_or_else(|| bad(name, "no column FILENAME"))?
            .0;
        for (i, row) in table.scan().enumerate() {
            if i >= max_rows {
                return Err(bad(name, "folder row limit"));
            }
            let (_, row) = row?;
            match row.get(column) {
                Some(Value::VarChar(s) | Value::Char(s)) if s.len() <= max_name => {}
                _ => return Err(bad(name, "folder name limit or type")),
            }
        }
        let entries = folder_entries(&table, name)?;
        Ok(Self {
            table,
            name: name.to_string(),
            entries,
        })
    }

    pub fn data_len(&self, i: usize) -> Result<u64> {
        let e = self
            .entries
            .get(i)
            .ok_or_else(|| bad(&self.name, "no such entry"))?;
        let column = self
            .table
            .descr()
            .field("BINARYDATA")
            .ok_or_else(|| bad(&self.name, "no column BINARYDATA"))?
            .0;
        let mut total = 0u64;
        for &slot in &e.parts {
            let row = self
                .table
                .get(slot)?
                .ok_or_else(|| bad(&self.name, "part vanished"))?;
            let len = match row.get(column) {
                Some(Value::Image(BlobVal::Ref(r))) => u64::from(r.len),
                Some(Value::Image(BlobVal::Data(b)) | Value::Binary(b)) => b.len() as u64,
                _ => return Err(bad(&self.name, "BINARYDATA type")),
            };
            total = total
                .checked_add(len)
                .ok_or_else(|| bad(&self.name, "entry length overflow"))?;
        }
        Ok(total)
    }

    pub fn data(&self, i: usize) -> Result<Vec<u8>> {
        folder_data(&self.table, &self.name, &self.entries, i)
    }

    fn data_metadata_bounded(&self, i: usize, limit: usize) -> Result<Vec<u8>> {
        let entry = self
            .entries
            .get(i)
            .ok_or_else(|| bad(&self.name, "no such entry"))?;
        let context = format!("{}/{}", self.name, entry.name);
        if partno_of(&entry.data_size, &self.name)? > limit as u64 {
            return Err(metadata_limit(&context, limit));
        }
        let (column, _) = self
            .table
            .descr()
            .field("BINARYDATA")
            .ok_or_else(|| bad(&self.name, "no column BINARYDATA"))?;
        let mut out = Vec::new();
        for &slot in &entry.parts {
            let row = self
                .table
                .get(slot)?
                .ok_or_else(|| bad(&self.name, "part vanished"))?;
            let value = row
                .get(column)
                .ok_or_else(|| bad(&self.name, "short row"))?;
            append_metadata_value(value, &mut out, limit, &context, || {
                self.table.blob(slot, "BINARYDATA").map_err(Error::from)
            })?;
        }
        Ok(out)
    }

    pub fn data_bounded(&self, i: usize, limit: usize) -> Result<Vec<u8>> {
        folder_data_bounded(&self.table, &self.name, &self.entries, i, limit)
    }
}

fn folder_entries(t: &TableRead<'_>, table: &str) -> Result<Vec<FolderEntry>> {
    let mut entries: Vec<FolderEntry> = Vec::new();
    {
        let d = t.descr();
        let col = |name: &str| -> Result<usize> {
            d.field(name)
                .map(|(i, _)| i)
                .ok_or_else(|| bad(table, &format!("no column {name}")))
        };
        let (c_name, c_cr, c_mod, c_attr, c_size) = (
            col("FILENAME")?,
            col("CREATION")?,
            col("MODIFIED")?,
            col("ATTRIBUTES")?,
            col("DATASIZE")?,
        );
        let c_data = col("BINARYDATA")?;
        let c_part = d.field("PARTNO").map(|(i, _)| i);
        let mut partnos: Vec<Vec<u64>> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut case_names: HashMap<String, String> = HashMap::new();
        for item in t.scan() {
            let (slot, row) = item?;
            let cell = |i: usize| row.get(i).ok_or_else(|| bad(table, "short row"));
            let name = match cell(c_name)? {
                Value::VarChar(s) | Value::Char(s) => s.clone(),
                v => return Err(bad(table, &format!("FILENAME is {}", value_kind(v)))),
            };
            let lower = name.to_lowercase();
            match case_names.get(&lower) {
                Some(first) if *first != name => {
                    return Err(bad(
                        table,
                        &format!("{name}: file names differ only in case"),
                    ));
                }
                Some(_) => {}
                None => {
                    case_names.insert(lower, name.clone());
                }
            }
            let date = |i: usize, what: &str| -> Result<DateTime> {
                match cell(i)? {
                    Value::DateTime(d) => Ok(*d),
                    v => Err(bad(table, &format!("{what} is {}", value_kind(v)))),
                }
            };
            let num = |i: usize, what: &str| -> Result<Numeric> {
                match cell(i)? {
                    Value::Numeric(n) => Ok(n.clone()),
                    v => Err(bad(table, &format!("{what} is {}", value_kind(v)))),
                }
            };
            match cell(c_data)? {
                Value::Image(_) | Value::Binary(_) => {}
                v => return Err(bad(table, &format!("BINARYDATA is {}", value_kind(v)))),
            }
            let partno = match c_part {
                Some(i) => partno_of(&num(i, "PARTNO")?, table)?,
                None => 0,
            };
            match index.get(&name) {
                Some(&i) => {
                    if let (Some(e), Some(pn)) = (entries.get_mut(i), partnos.get_mut(i)) {
                        if pn.iter().all(|&p| p > partno) {
                            e.creation = date(c_cr, "CREATION")?;
                            e.modified = date(c_mod, "MODIFIED")?;
                            e.attributes = num(c_attr, "ATTRIBUTES")?;
                            e.data_size = num(c_size, "DATASIZE")?;
                        }
                        pn.push(partno);
                        e.parts.push(slot);
                    }
                }
                None => {
                    index.insert(name.clone(), entries.len());
                    entries.push(FolderEntry {
                        creation: date(c_cr, "CREATION")?,
                        modified: date(c_mod, "MODIFIED")?,
                        attributes: num(c_attr, "ATTRIBUTES")?,
                        data_size: num(c_size, "DATASIZE")?,
                        name,
                        parts: vec![slot],
                    });
                    partnos.push(vec![partno]);
                }
            }
        }
        for (e, pn) in entries.iter_mut().zip(partnos) {
            if pn.len() > 1 {
                let mut both: Vec<(u64, RowId)> =
                    pn.into_iter().zip(e.parts.iter().copied()).collect();
                both.sort_by_key(|&(p, _)| p);
                if both
                    .windows(2)
                    .any(|w| w.first().map(|x| x.0) == w.get(1).map(|x| x.0))
                {
                    return Err(bad(table, &format!("{}: repeated PARTNO", e.name)));
                }
                e.parts = both.into_iter().map(|(_, s)| s).collect();
            }
        }
        let mut seen = HashSet::with_capacity(entries.len());
        for e in &entries {
            if !seen.insert(e.name.to_lowercase()) {
                return Err(bad(
                    table,
                    &format!("{}: file names differ only in case", e.name),
                ));
            }
        }
    }
    Ok(entries)
}

fn bad(table: &str, what: &str) -> Error {
    Error::Inconsistent(format!("{table}: {what}"))
}

fn partno_of(n: &Numeric, table: &str) -> Result<u64> {
    if (n.is_negative() && n.digits().iter().any(|&d| d != 0)) || n.scale() != 0 {
        return Err(bad(table, "bad PARTNO"));
    }
    n.digits().iter().try_fold(0u64, |acc, &d| {
        acc.checked_mul(10)
            .and_then(|a| a.checked_add(u64::from(d)))
            .ok_or_else(|| bad(table, "PARTNO is too large"))
    })
}

struct StorageColumns {
    id: usize,
    status: usize,
    current: usize,
    newgen: [Option<usize>; 2],
}
impl StorageColumns {
    fn new(d: &onecd::TableDescr) -> Result<Self> {
        let column = |name: &str| {
            d.field(name)
                .map(|(i, _)| i)
                .ok_or_else(|| bad("SCHEMASTORAGE", &format!("missing column {name}")))
        };
        Ok(Self {
            id: column("SCHEMAID")?,
            status: column("STATUS")?,
            current: column("CURRENTSCHEMA")?,
            newgen: [
                d.field("NEWGENCREATED").map(|(i, _)| i),
                d.field("NEWGENDROPPED").map(|(i, _)| i),
            ],
        })
    }
}

fn storage_number(value: Option<&Value>, name: &str) -> Result<u32> {
    let Some(Value::Numeric(n)) = value else {
        return Err(bad("SCHEMASTORAGE", &format!("{name} is not numeric")));
    };
    if n.scale() != 0 || n.is_negative() {
        return Err(bad(
            "SCHEMASTORAGE",
            &format!("{name} is not an unsigned integer"),
        ));
    }
    n.digits().iter().try_fold(0u32, |value, &digit| {
        value
            .checked_mul(10)
            .and_then(|n| n.checked_add(u32::from(digit)))
            .ok_or_else(|| bad("SCHEMASTORAGE", &format!("{name} exceeds u32")))
    })
}

fn storage_len(value: Option<&Value>, name: &str, limit: u64) -> Result<usize> {
    let len = match value {
        Some(Value::Image(BlobVal::Data(bytes)) | Value::Text(BlobVal::Data(bytes))) => bytes.len(),
        Some(Value::Image(BlobVal::Ref(r)) | Value::Text(BlobVal::Ref(r))) => r.len as usize,
        _ => {
            return Err(bad(
                "SCHEMASTORAGE",
                &format!("{name} is not image or text"),
            ));
        }
    };
    if len as u64 > limit {
        return Err(bad(
            "SCHEMASTORAGE",
            &format!("{name} exceeds value byte limit"),
        ));
    }
    Ok(len)
}

struct StorageRow {
    id: u32,
    status: u32,
    current: usize,
    newgen: [Option<usize>; 2],
}

const STORAGE_ROWS_SCANNED: usize = 64;
const STORAGE_ROWS_LISTED: usize = 16;
const STORAGE_STATUS: u32 = 100;

#[derive(Default)]
struct StorageRows {
    rows: Vec<StorageRow>,
    more: bool,
}
impl StorageRows {
    fn admit(&mut self) -> bool {
        if self.rows.len() == STORAGE_ROWS_SCANNED {
            self.more = true;
            return false;
        }
        true
    }
    fn observe(&mut self, columns: &StorageColumns, row: &[Value], limit: u64) -> Result<u32> {
        let id = storage_number(row.get(columns.id), "SCHEMAID")?;
        let status = storage_number(row.get(columns.status), "STATUS")?;
        let current = storage_len(row.get(columns.current), "CURRENTSCHEMA", limit)?;
        let mut newgen = [None, None];
        for ((column, name), len) in columns
            .newgen
            .iter()
            .zip(["NEWGENCREATED", "NEWGENDROPPED"])
            .zip(&mut newgen)
        {
            if let Some(column) = column {
                *len = Some(storage_len(row.get(*column), name, limit)?);
            }
        }
        self.rows.push(StorageRow {
            id,
            status,
            current,
            newgen,
        });
        Ok(id)
    }
    fn finish(&self, columns: &StorageColumns) -> Result<bool> {
        let has = |id: u32| self.rows.iter().any(|r| r.id == id);
        if self.rows.is_empty() {
            return Err(self.unsupported("no rows"));
        }
        if self.more || self.rows.len() > 2 {
            return Err(self.unsupported("more than two rows"));
        }
        if self.rows.iter().any(|r| r.id > 1) {
            return Err(self.unsupported("SCHEMAID other than 0 (main) and 1 (extensions)"));
        }
        if let [first, second] = self.rows.as_slice()
            && first.id == second.id
        {
            return Err(self.unsupported("duplicate SCHEMAID"));
        }
        if !has(0) {
            return Err(self.unsupported("no main row SCHEMAID 0"));
        }
        if self.rows.iter().any(|r| r.status != STORAGE_STATUS) {
            return Err(self.unsupported("STATUS other than 100"));
        }
        if has(1) && columns.newgen.iter().any(Option::is_none) {
            return Err(bad(
                "SCHEMASTORAGE",
                "extension storage requires both NEWGEN columns",
            ));
        }
        Ok(has(1))
    }
    fn unsupported(&self, why: &str) -> Error {
        use std::fmt::Write as _;
        let count = if self.more {
            format!("more than {STORAGE_ROWS_SCANNED} rows")
        } else {
            format!("{} rows", self.rows.len())
        };
        let mut text = format!("SCHEMASTORAGE layout ({why}); {count}");
        for (n, r) in self.rows.iter().take(STORAGE_ROWS_LISTED).enumerate() {
            let _ = write!(
                text,
                "{} SCHEMAID {} STATUS {} CURRENTSCHEMA {} B",
                if n == 0 { ":" } else { ";" },
                r.id,
                r.status,
                r.current
            );
            for (len, name) in r.newgen.iter().zip(["NEWGENCREATED", "NEWGENDROPPED"]) {
                if let Some(len) = len {
                    let _ = write!(text, " {name} {len} B");
                }
            }
        }
        if self.rows.len() > STORAGE_ROWS_LISTED {
            let _ = write!(text, "; ...");
        }
        Error::Unsupported(text)
    }
}

fn storage_scan(
    t: &TableRead<'_>,
    columns: &StorageColumns,
    limit: u64,
) -> Result<(Vec<Value>, Option<Vec<Value>>)> {
    let mut shape = StorageRows::default();
    let (mut main, mut extension) = (None, None);
    for entry in t.scan() {
        if !shape.admit() {
            break;
        }
        let (_, row) = entry?;
        match shape.observe(columns, &row, limit)? {
            0 if main.is_none() => main = Some(row),
            1 if extension.is_none() => extension = Some(row),
            _ => {}
        }
    }
    let has_extension = shape.finish(columns)?;
    let main = main.ok_or_else(|| bad("SCHEMASTORAGE", "missing main SCHEMAID0"))?;
    Ok((main, extension.filter(|_| has_extension)))
}

fn storage_bytes(t: &TableRead<'_>, value: Option<&Value>) -> Result<Vec<u8>> {
    match value {
        Some(Value::Image(BlobVal::Data(b)) | Value::Text(BlobVal::Data(b))) => Ok(b.clone()),
        Some(Value::Image(BlobVal::Ref(r)) | Value::Text(BlobVal::Ref(r))) => Ok(t.blob_bytes(*r)?),
        _ => Err(bad("SCHEMASTORAGE", "CURRENTSCHEMA is not image or text")),
    }
}

pub(crate) fn extension_schema_in(txn: &ReadTxn) -> Result<Option<Vec<u8>>> {
    let t = match txn.table("SCHEMASTORAGE") {
        Ok(t) => t,
        Err(onecd::Error::NotFound(onecd::NotFound::Table(_))) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let columns = StorageColumns::new(t.descr())?;
    let limit = u64::try_from(SNAPSHOT_METADATA_BYTES).unwrap_or(u64::MAX);
    storage_scan(&t, &columns, limit)?
        .1
        .map(|row| storage_bytes(&t, row.get(columns.current)))
        .transpose()
}

impl RawIb {
    pub fn open(path: &Path) -> Result<RawIb> {
        RawIb::open_with(path, OpenOptions::read_only())
    }

    pub fn open_with(path: &Path, opts: OpenOptions) -> Result<RawIb> {
        Ok(RawIb::from_db(Arc::new(Database::open(path, opts)?)))
    }

    pub fn from_db(db: Arc<Database>) -> RawIb {
        RawIb { db }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn db_arc(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn schema_storage(&self) -> Result<Option<SchemaStorage>> {
        let txn = self.db.read();
        let t = match txn.table("SCHEMASTORAGE") {
            Ok(t) => t,
            Err(onecd::Error::NotFound(onecd::NotFound::Table(_))) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let columns = StorageColumns::new(t.descr())?;
        let (main, _) = storage_scan(&t, &columns, zraw::DEFAULT_LIMIT)?;
        Ok(Some(SchemaStorage {
            schema_id: 0,
            status: storage_number(main.get(columns.status), "STATUS")?,
            current: storage_bytes(&t, main.get(columns.current))?,
        }))
    }

    pub(crate) fn schema_storage_state(&self) -> Result<Option<(SchemaStorage, Option<Vec<u8>>)>> {
        let txn = self.db.read();
        let t = match txn.table("SCHEMASTORAGE") {
            Ok(t) => t,
            Err(onecd::Error::NotFound(onecd::NotFound::Table(_))) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let columns = StorageColumns::new(t.descr())?;
        let (main, extension) = storage_scan(&t, &columns, zraw::DEFAULT_LIMIT)?;
        let extension = extension
            .map(|row| storage_bytes(&t, row.get(columns.current)))
            .transpose()?;
        Ok(Some((
            SchemaStorage {
                schema_id: 0,
                status: storage_number(main.get(columns.status), "STATUS")?,
                current: storage_bytes(&t, main.get(columns.current))?,
            },
            extension,
        )))
    }

    pub fn has_table(&self, table: &str) -> bool {
        self.db
            .read()
            .table_names()
            .iter()
            .any(|n| n.eq_ignore_ascii_case(table))
    }

    pub fn has_rows(&self, table: &str) -> Result<bool> {
        let txn = self.db.read();
        let t = match txn.table(table) {
            Ok(t) => t,
            Err(onecd::Error::NotFound(onecd::NotFound::Table(_))) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        Ok(t.scan().next().transpose()?.is_some())
    }

    pub fn row_count(&self, table: &str) -> Result<u64> {
        let txn = self.db.read();
        let t = match txn.table(table) {
            Ok(t) => t,
            Err(onecd::Error::NotFound(onecd::NotFound::Table(_))) => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut n = 0u64;
        for r in t.scan() {
            r?;
            n += 1;
        }
        Ok(n)
    }

    pub fn folder(&self, table: &str) -> Result<Vec<FolderFile>> {
        self.read_folder(table, None)
    }

    fn read_folder(&self, table: &str, only: Option<&str>) -> Result<Vec<FolderFile>> {
        let txn = self.db.read();
        let t = txn.table(table)?;
        let d = t.descr();
        let col = |name: &str| -> Result<usize> {
            d.field(name)
                .map(|(i, _)| i)
                .ok_or_else(|| bad(table, &format!("no column {name}")))
        };
        let (c_name, c_cr, c_mod, c_attr, c_size, c_data) = (
            col("FILENAME")?,
            col("CREATION")?,
            col("MODIFIED")?,
            col("ATTRIBUTES")?,
            col("DATASIZE")?,
            col("BINARYDATA")?,
        );
        let c_part = d.field("PARTNO").map(|(i, _)| i);

        let mut groups: Vec<(String, Vec<Part>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut case_names: HashMap<String, String> = HashMap::new();
        for item in t.scan() {
            let (_, row) = item?;
            let cell = |i: usize| row.get(i).ok_or_else(|| bad(table, "short row"));
            let name = match cell(c_name)? {
                Value::VarChar(s) | Value::Char(s) => s.clone(),
                v => return Err(bad(table, &format!("FILENAME is {}", value_kind(v)))),
            };
            let lower = name.to_lowercase();
            match case_names.get(&lower) {
                Some(first) if *first != name => {
                    return Err(bad(
                        table,
                        &format!("{name}: file names differ only in case"),
                    ));
                }
                Some(_) => {}
                None => {
                    case_names.insert(lower.clone(), name.clone());
                }
            }
            if let Some(w) = only
                && lower != w
            {
                continue;
            }
            let date = |i: usize, what: &str| -> Result<DateTime> {
                match cell(i)? {
                    Value::DateTime(d) => Ok(*d),
                    v => Err(bad(table, &format!("{what} is {}", value_kind(v)))),
                }
            };
            let num = |i: usize, what: &str| -> Result<Numeric> {
                match cell(i)? {
                    Value::Numeric(n) => Ok(n.clone()),
                    v => Err(bad(table, &format!("{what} is {}", value_kind(v)))),
                }
            };
            let data = match cell(c_data)? {
                Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.clone(),
                Value::Image(BlobVal::Ref(r)) => t.blob_bytes(*r)?,
                v => return Err(bad(table, &format!("BINARYDATA is {}", value_kind(v)))),
            };
            let partno = match c_part {
                Some(i) => partno_of(&num(i, "PARTNO")?, table)?,
                None => 0,
            };
            let part = Part {
                partno,
                file: FolderFile {
                    creation: date(c_cr, "CREATION")?,
                    modified: date(c_mod, "MODIFIED")?,
                    attributes: num(c_attr, "ATTRIBUTES")?,
                    data_size: num(c_size, "DATASIZE")?,
                    name: name.clone(),
                    data,
                },
            };
            match index.get(&name) {
                Some(&i) => {
                    if let Some((_, parts)) = groups.get_mut(i) {
                        parts.push(part);
                    }
                }
                None => {
                    index.insert(name.clone(), groups.len());
                    groups.push((name, vec![part]));
                }
            }
        }

        let mut out = Vec::with_capacity(groups.len());
        for (name, mut parts) in groups {
            parts.sort_by_key(|p| p.partno);
            if parts
                .windows(2)
                .any(|w| w.first().map(|p| p.partno) == w.get(1).map(|p| p.partno))
            {
                return Err(bad(table, &format!("{name}: repeated PARTNO")));
            }
            let mut it = parts.into_iter();
            let Some(first) = it.next() else { continue };
            let mut file = first.file;
            for p in it {
                file.data.extend_from_slice(&p.file.data);
            }
            out.push(file);
        }
        let mut seen = HashSet::with_capacity(out.len());
        for f in &out {
            if !seen.insert(f.name.to_lowercase()) {
                return Err(bad(
                    table,
                    &format!("{}: file names differ only in case", f.name),
                ));
            }
        }
        Ok(out)
    }

    pub fn folder_index(&self, table: &str) -> Result<FolderIndex> {
        let cell = TxnTable::try_new(self.db.read(), |txn| txn.table(table))?;
        let entries = folder_entries(cell.borrow_dependent(), table)?;
        Ok(FolderIndex {
            cell,
            table: table.to_string(),
            entries,
        })
    }

    pub fn folder_file(&self, table: &str, name: &str) -> Result<Option<FolderFile>> {
        let lower = name.to_lowercase();
        Ok(self
            .read_folder(table, Some(&lower))?
            .into_iter()
            .find(|f| f.name.to_lowercase() == lower))
    }

    fn inflated(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.folder_file("PARAMS", name)? {
            Some(f) => inflate_raw(&f.data).map(Some).map_err(|e| match e {
                Error::Inconsistent(m) => Error::Inconsistent(format!("PARAMS: {name}: {m}")),
                Error::Unsupported(m) => Error::Unsupported(format!("PARAMS: {name}: {m}")),
                other => other,
            }),
            None => Ok(None),
        }
    }

    pub fn dbnames_text(&self) -> Result<Vec<u8>> {
        self.inflated("DBNames")?
            .ok_or_else(|| Error::Inconsistent("PARAMS has no DBNames".into()))
    }

    pub fn dbnames_version_text(&self) -> Result<Option<Vec<u8>>> {
        Ok(self
            .folder_file("PARAMS", "DBNamesVersion-DBNames")?
            .map(|f| f.data))
    }

    pub fn dbschema_text(&self) -> Result<Vec<u8>> {
        dbschema_text_with(&self.db.read(), None)
    }
}

pub fn dbschema_text_in(txn: &ReadTxn) -> Result<Vec<u8>> {
    dbschema_text_in_bounded(txn, SNAPSHOT_METADATA_BYTES)
}

pub const SNAPSHOT_METADATA_BYTES: usize = 64 * 1024 * 1024;

pub fn dbschema_text_in_bounded(txn: &ReadTxn, limit: usize) -> Result<Vec<u8>> {
    dbschema_text_with(txn, Some(limit))
}

fn dbschema_text_with(txn: &ReadTxn, limit: Option<usize>) -> Result<Vec<u8>> {
    let t = txn.table("DBSCHEMA")?;
    let (c, _) = t
        .descr()
        .field("SERIALIZEDDATA")
        .ok_or_else(|| bad("DBSCHEMA", "no column SERIALIZEDDATA"))?;
    let mut rows = t.scan();
    let first = rows
        .next()
        .ok_or_else(|| bad("DBSCHEMA", "no rows, expected one"))??;
    if rows.next().transpose()?.is_some() {
        return Err(bad("DBSCHEMA", "more than one row"));
    }
    if let Some(limit) = limit {
        let value = first
            .1
            .get(c)
            .ok_or_else(|| bad("DBSCHEMA", "SERIALIZEDDATA is missing"))?;
        match value {
            Value::Image(_) | Value::Text(_) => {
                let mut out = Vec::new();
                append_metadata_value(value, &mut out, limit, "DBSCHEMA/SERIALIZEDDATA", || {
                    t.blob(first.0, "SERIALIZEDDATA").map_err(Error::from)
                })?;
                return Ok(out);
            }
            v => {
                return Err(bad(
                    "DBSCHEMA",
                    &format!("SERIALIZEDDATA is {}", value_kind(v)),
                ));
            }
        }
    }
    match first.1.get(c) {
        Some(Value::Image(BlobVal::Data(b)) | Value::Text(BlobVal::Data(b))) => Ok(b.clone()),
        Some(Value::Image(BlobVal::Ref(r)) | Value::Text(BlobVal::Ref(r))) => Ok(t.blob_bytes(*r)?),
        v => Err(bad(
            "DBSCHEMA",
            &format!("SERIALIZEDDATA is {}", v.map_or("missing", value_kind)),
        )),
    }
}

pub fn dbnames_text_in(txn: &ReadTxn) -> Result<Vec<u8>> {
    dbnames_text_in_bounded(txn, SNAPSHOT_METADATA_BYTES, SNAPSHOT_METADATA_BYTES)
}

pub fn dbnames_text_in_bounded(
    txn: &ReadTxn,
    stored_limit: usize,
    inflated_limit: usize,
) -> Result<Vec<u8>> {
    let index = FolderTxnIndex::read(txn, "PARAMS")?;
    let i = index
        .entries
        .iter()
        .position(|e| e.name.eq_ignore_ascii_case("DBNames"))
        .ok_or_else(|| Error::Inconsistent("PARAMS has no DBNames".into()))?;
    let data = index.data_metadata_bounded(i, stored_limit)?;
    let inflated_limit = u64::try_from(inflated_limit)
        .map_err(|e| Error::Unsupported(format!("PARAMS/DBNames: inflated limit: {e}")))?;
    zraw::inflate(&data, inflated_limit).map_err(|e| match e {
        zraw::Error::TooLarge(_) | zraw::Error::Zlib(_) => {
            Error::Unsupported(format!("PARAMS/DBNames: {e}"))
        }
        _ => Error::Inconsistent(format!("PARAMS/DBNames: {e}")),
    })
}

fn metadata_limit(context: &str, limit: usize) -> Error {
    Error::Unsupported(format!(
        "{context}: stored size exceeds {limit} byte product limit"
    ))
}

fn append_metadata_bytes(
    out: &mut Vec<u8>,
    bytes: &[u8],
    limit: usize,
    context: &str,
) -> Result<()> {
    let length = out
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| metadata_limit(context, limit))?;
    if length > limit {
        return Err(metadata_limit(context, limit));
    }
    out.try_reserve(bytes.len())
        .map_err(|e| Error::Unsupported(format!("{context}: allocation: {e}")))?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_metadata_value<R: std::io::Read>(
    value: &Value,
    out: &mut Vec<u8>,
    limit: usize,
    context: &str,
    open: impl FnOnce() -> Result<R>,
) -> Result<()> {
    match value {
        Value::Image(BlobVal::Data(bytes))
        | Value::Text(BlobVal::Data(bytes))
        | Value::Binary(bytes) => append_metadata_bytes(out, bytes, limit, context),
        Value::Image(BlobVal::Ref(reference)) | Value::Text(BlobVal::Ref(reference)) => {
            let remaining = limit
                .checked_sub(out.len())
                .ok_or_else(|| metadata_limit(context, limit))?;
            if u64::from(reference.len) > remaining as u64 {
                return Err(metadata_limit(context, limit));
            }
            let initial = out.len();
            let mut reader = open()?;
            let mut buffer = [0; 8192];
            loop {
                let remaining = limit
                    .checked_sub(out.len())
                    .ok_or_else(|| metadata_limit(context, limit))?;
                let take = remaining.saturating_add(1).min(buffer.len());
                let n = reader
                    .read(
                        buffer
                            .get_mut(..take)
                            .ok_or_else(|| bad(context, "invalid read bound"))?,
                    )
                    .map_err(|e| bad(context, &format!("blob read: {e}")))?;
                if n == 0 {
                    break;
                }
                let bytes = buffer
                    .get(..n)
                    .ok_or_else(|| bad(context, "invalid reader length"))?;
                append_metadata_bytes(out, bytes, limit, context)?;
            }
            let actual = out
                .len()
                .checked_sub(initial)
                .ok_or_else(|| bad(context, "invalid accumulated length"))?;
            if actual as u64 != u64::from(reference.len) {
                return Err(bad(context, "blob length differs from declared length"));
            }
            Ok(())
        }
        value => Err(bad(
            context,
            &format!("binary data is {}", value_kind(value)),
        )),
    }
}
