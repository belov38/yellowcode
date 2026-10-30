mod json;
mod managed;

use memory_budget::{BudgetError, Category, Lock, MemoryBudget, Reservation};
use std::path::Path;
use std::sync::OnceLock;

use brace::Node;
use ibschema::dbnames::DbNames;
use ibschema::logical::LogicalSchema;
use onecd::{BlobVal, Locking, OpenOptions, TableDescr, Value};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::raw::RawIb;
use json::{JLTable, JParams, JRows, JTable};

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsTemplate {
    pub name: String,
    pub text: String,
    pub deflate: bool,
}

#[derive(Debug, Clone)]
pub struct Seed {
    pub tables: Vec<TableDescr>,
    pub schema: LogicalSchema,
    pub dbnames: DbNames,
    pub params: Vec<ParamsTemplate>,
    pub ibversion: (u32, u32),
    pub newgen: Vec<u8>,
    pub schemastorage: (u32, u32),
}

const TABLES_JSON: &str = include_str!("data/tables.json");
const SCHEMA_JSON: &str = include_str!("data/dbschema.json");
const DBNAMES_JSON: &str = include_str!("data/dbnames.json");
const PARAMS_JSON: &str = include_str!("data/params.json");
const ROWS_JSON: &str = include_str!("data/rows.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedFile {
    Tables,
    Schema,
    DbNames,
    Params,
    Rows,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedDataErrorKind {
    Shape,
    String,
    Number,
    Depth,
    DbNames,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedDataError {
    pub file: SeedFile,
    pub offset: usize,
    pub kind: SeedDataErrorKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedMemoryError {
    Budget(BudgetError),
    DifferentRoot,
    Embedded(SeedDataError),
}
impl From<BudgetError> for SeedMemoryError {
    fn from(error: BudgetError) -> Self {
        Self::Budget(error)
    }
}
impl std::fmt::Display for SeedMemoryError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(error) => error.fmt(out),
            Self::DifferentRoot => out.write_str("SEED belongs to a different process memory root"),
            Self::Embedded(error) => write!(
                out,
                "invalid embedded seed {:?} at {}: {:?}",
                error.file, error.offset, error.kind
            ),
        }
    }
}
impl std::error::Error for SeedMemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Budget(error) => Some(error),
            _ => None,
        }
    }
}

struct SeedEntry {
    value: Seed,
    charge: Lock<Option<Reservation>>,
    retained_bytes: u64,
}
impl std::ops::Deref for SeedEntry {
    type Target = Seed;
    fn deref(&self) -> &Seed {
        &self.value
    }
}
static SEED: OnceLock<SeedEntry> = OnceLock::new();
static INITIALIZE: Lock<bool> = Lock::new(false);

struct Initialize;
impl Initialize {
    fn claim() -> Self {
        loop {
            {
                let mut active = INITIALIZE.lock();
                if !*active {
                    *active = true;
                    return Self;
                }
            }
            std::thread::yield_now();
        }
    }
}
impl Drop for Initialize {
    fn drop(&mut self) {
        *INITIALIZE.lock() = false;
    }
}

pub fn seed_with_memory(
    memory: &MemoryBudget,
) -> std::result::Result<&'static Seed, SeedMemoryError> {
    let root = memory.root_budget();
    let _gate = Initialize::claim();
    let entry = match SEED.get() {
        Some(entry) => entry,
        None => {
            let candidate = managed::build(Some(&root))?;
            let _ = SEED.set(candidate);
            SEED.get()
                .ok_or(SeedMemoryError::Budget(BudgetError::Overflow))?
        }
    };
    {
        let owner = entry.charge.lock();
        match owner.as_ref() {
            Some(owner) if !owner.budget().same_instance(&root) => {
                return Err(SeedMemoryError::DifferentRoot);
            }
            Some(_) => return Ok(&entry.value),
            None => {}
        }
    }
    let owner = root
        .try_reserve(Category::Metadata, entry.retained_bytes)
        .map_err(BudgetError::from)?;
    *entry.charge.lock() = Some(owner);
    Ok(&entry.value)
}

#[allow(clippy::expect_used)]
pub fn seed() -> &'static Seed {
    if let Some(entry) = SEED.get() {
        return &entry.value;
    }
    let _gate = Initialize::claim();
    &SEED
        .get_or_init(|| managed::build(None).expect("embedded seed data is valid"))
        .value
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedMemoryState {
    Empty,
    Unmanaged,
    Managed {
        same_root: bool,
        retained_bytes: u64,
    },
}
#[cfg(feature = "calibration")]
pub fn seed_memory_state(memory: &MemoryBudget) -> SeedMemoryState {
    let Some(entry) = SEED.get() else {
        return SeedMemoryState::Empty;
    };
    let owner = entry.charge.lock();
    match owner.as_ref() {
        None => SeedMemoryState::Unmanaged,
        Some(owner) => SeedMemoryState::Managed {
            same_root: owner.budget().same_instance(&memory.root_budget()),
            retained_bytes: entry.retained_bytes,
        },
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::Inconsistent(format!("seed generator: {}", why.into()))
}

fn write_json<T: Serialize>(out: &Path, name: &str, v: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(v).map_err(|e| bad(format!("{name}: {e}")))?;
    text.push('\n');
    std::fs::write(out.join(name), text).map_err(|e| bad(format!("{name}: {e}")))
}

#[derive(Clone, Copy)]
enum Slot {
    Uuid,
    Number,
    Str,
}

fn placeholders(file: &str) -> &'static [(&'static [usize], &'static str, Slot)] {
    match file {
        "locale.inf" => &[
            (&[0], "{locale}", Slot::Str),
            (&[11], "{db_u1}", Slot::Uuid),
            (&[12], "{db_u2}", Slot::Uuid),
        ],
        "log.inf" => &[(&[0], "{log_uuid}", Slot::Uuid)],
        "ibparams.inf" => &[
            (&[7], "{db_u1}", Slot::Uuid),
            (&[8], "{db_u2}", Slot::Uuid),
            (&[15, 9], "{n}", Slot::Number),
        ],
        "DBNamesVersion-DBNames" => &[(&[1], "{version_uuid}", Slot::Uuid)],
        _ => &[],
    }
}

fn params_template(file: &str, data: &[u8]) -> Result<String> {
    let doc = brace::parse_doc(data).map_err(|e| bad(format!("{file}: {e}")))?;
    if brace::write_doc(&doc) != data {
        return Err(bad(format!("{file}: text does not round-trip")));
    }
    let mut root = doc.root.clone();
    let mut back: Vec<(&str, String)> = Vec::new();
    for &(path, ph, slot) in placeholders(file) {
        let node = root
            .get_mut(path)
            .ok_or_else(|| bad(format!("{file}: no element at {path:?}")))?;
        let original = match (slot, &*node) {
            (Slot::Uuid, n) if n.is_guid() => n.as_atom().map(str::to_string),
            (Slot::Number, n) => n
                .as_atom()
                .filter(|a| {
                    !a.is_empty()
                        && a.bytes().all(|b| b.is_ascii_digit())
                        && a.parse::<u64>().is_ok()
                })
                .map(str::to_string),
            (Slot::Str, Node::Str(s)) => Some(format!("\"{}\"", s.replace('"', "\"\""))),
            _ => None,
        }
        .ok_or_else(|| {
            bad(format!(
                "{file}: element {path:?} is not the expected value"
            ))
        })?;
        back.push((ph, original));
        *node = Node::atom(ph);
    }
    let body = format!("{}{}{}", doc.lead, brace::to_text(&root), doc.trailer);
    let mut restored = body.clone();
    for (ph, original) in &back {
        restored = restored.replace(ph, original);
    }
    let mut whole = BOM.to_vec();
    whole.extend_from_slice(restored.as_bytes());
    if !doc.bom {
        whole.drain(..BOM.len());
    }
    if whole != data {
        return Err(bad(format!(
            "{file}: template with the values put back differs from the original"
        )));
    }
    Ok(body)
}

fn blob(v: Option<&Value>, t: &onecd::TableRead<'_>, what: &str) -> Result<Vec<u8>> {
    match v {
        Some(Value::Image(BlobVal::Data(b)) | Value::Text(BlobVal::Data(b))) => Ok(b.clone()),
        Some(Value::Image(BlobVal::Ref(r)) | Value::Text(BlobVal::Ref(r))) => Ok(t.blob_bytes(*r)?),
        _ => Err(bad(format!("{what} is not a blob"))),
    }
}

fn small(v: Option<&Value>, what: &str) -> Result<u32> {
    let Some(Value::Numeric(n)) = v else {
        return Err(bad(format!("{what} is not a number")));
    };
    if n.scale() != 0 || n.is_negative() {
        return Err(bad(format!("{what} is not a natural number")));
    }
    n.digits()
        .iter()
        .try_fold(0u32, |a, &d| a.checked_mul(10)?.checked_add(u32::from(d)))
        .ok_or_else(|| bad(format!("{what} is too large")))
}

pub fn generate(c851_0create: &Path, out: &Path) -> Result<()> {
    let opts = OpenOptions {
        locking: Locking::None,
        ..OpenOptions::read_only()
    };
    let ib = RawIb::open_with(c851_0create, opts)?;
    let txn = ib.db().read();

    let mut tables = Vec::new();
    for name in txn.table_names() {
        let table = txn.table(&name)?;
        let encoded = JTable::from_descr(table.descr());
        let decoded = encoded.to_descr()?;
        let original = table.descr();
        if decoded.name != original.name
            || decoded.fields != original.fields
            || decoded.indexes != original.indexes
            || decoded.recordlock != original.recordlock
        {
            return Err(bad("physical seed JSON does not round-trip"));
        }
        tables.push(encoded);
    }
    write_json(out, "tables.json", &tables)?;

    let dbschema = ib.dbschema_text()?;
    let schema = LogicalSchema::from_text(&dbschema)?;
    if schema.to_text() != dbschema {
        return Err(bad("DBSCHEMA does not round-trip"));
    }
    let jschema: Vec<JLTable> = schema.tables.iter().map(JLTable::from_table).collect();
    for (table, encoded) in schema.tables.iter().zip(&jschema) {
        if &encoded.to_table()? != table {
            return Err(bad("logical seed JSON does not round-trip"));
        }
    }
    write_json(out, "dbschema.json", &jschema)?;

    let dbnames = DbNames::parse(&ib.dbnames_text()?)?;
    let mut names = Vec::new();
    for (i, e) in dbnames.entries().iter().enumerate() {
        if !e.uuid.is_nil() {
            return Err(bad(format!("DBNames entry {i} {:?} has a uuid", e.purpose)));
        }
        names.push((e.purpose.clone(), e.number));
    }
    write_json(out, "dbnames.json", &names)?;

    let mut params = Vec::new();
    for f in ib.folder("PARAMS")? {
        let deflate = f.name == "DBNames";
        let text = if deflate {
            String::new()
        } else {
            params_template(&f.name, &f.data)?
        };
        params.push(JParams {
            name: f.name,
            text,
            deflate,
        });
    }
    write_json(out, "params.json", &params)?;

    let ver = txn.table("IBVERSION")?;
    let mut rows = ver.scan();
    let row = rows.next().ok_or_else(|| bad("IBVERSION has no rows"))??.1;
    if rows.next().is_some() {
        return Err(bad("IBVERSION has more than one row"));
    }
    let ibversion = (
        small(row.first(), "IBVERSION[0]")?,
        small(row.get(1), "IBVERSION[1]")?,
    );
    let ss = txn.table("SCHEMASTORAGE")?;
    let d = ss.descr();
    let col = |n: &str| {
        d.field(n)
            .map(|(i, _)| i)
            .ok_or_else(|| bad(format!("SCHEMASTORAGE has no column {n}")))
    };
    let mut rows = ss.scan();
    let row = rows
        .next()
        .ok_or_else(|| bad("SCHEMASTORAGE has no rows"))??
        .1;
    if rows.next().is_some() {
        return Err(bad("SCHEMASTORAGE has more than one row"));
    }
    let current = blob(row.get(col("CURRENTSCHEMA")?), &ss, "CURRENTSCHEMA")?;
    let created = blob(row.get(col("NEWGENCREATED")?), &ss, "NEWGENCREATED")?;
    let dropped = blob(row.get(col("NEWGENDROPPED")?), &ss, "NEWGENDROPPED")?;
    if current != dbschema {
        return Err(bad("CURRENTSCHEMA differs from DBSCHEMA"));
    }
    if created != dropped {
        return Err(bad("NEWGENCREATED differs from NEWGENDROPPED"));
    }
    let newgen = created
        .strip_prefix(BOM)
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| bad("NEWGENCREATED is not BOM + UTF-8"))?;
    write_json(
        out,
        "rows.json",
        &JRows {
            ibversion,
            schemastorage: (
                small(row.get(col("SCHEMAID")?), "SCHEMAID")?,
                small(row.get(col("STATUS")?), "STATUS")?,
            ),
            newgen: newgen.to_string(),
        },
    )
}
