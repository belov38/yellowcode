mod json;

use std::path::Path;
use std::sync::OnceLock;

use brace::{Guid, Node};
use ibschema::dbnames::{DbName, DbNames};
use ibschema::logical::LogicalSchema;
use onecd::{BlobVal, Locking, OpenOptions, TableDescr, Value};
use serde::Serialize;
use serde::de::DeserializeOwned;

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

fn parse_json<T: DeserializeOwned>(name: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| Error::Inconsistent(format!("seed {name}: {e}")))
}

fn load() -> Result<Seed> {
    let tables = parse_json::<Vec<JTable>>("tables.json", TABLES_JSON)?
        .iter()
        .map(JTable::to_descr)
        .collect::<Result<Vec<_>>>()?;
    let schema = LogicalSchema {
        tables: parse_json::<Vec<JLTable>>("dbschema.json", SCHEMA_JSON)?
            .iter()
            .map(JLTable::to_table)
            .collect::<Result<Vec<_>>>()?,
    };
    let mut dbnames = DbNames::default();
    for (purpose, number) in parse_json::<Vec<(String, u32)>>("dbnames.json", DBNAMES_JSON)? {
        dbnames.push(DbName {
            uuid: Guid::NIL,
            purpose,
            number,
        })?;
    }
    let params = parse_json::<Vec<JParams>>("params.json", PARAMS_JSON)?
        .into_iter()
        .map(|p| ParamsTemplate {
            name: p.name,
            text: p.text,
            deflate: p.deflate,
        })
        .collect();
    let rows: JRows = parse_json("rows.json", ROWS_JSON)?;
    let mut newgen = BOM.to_vec();
    newgen.extend_from_slice(rows.newgen.as_bytes());
    Ok(Seed {
        tables,
        schema,
        dbnames,
        params,
        ibversion: rows.ibversion,
        newgen,
        schemastorage: rows.schemastorage,
    })
}

#[allow(clippy::expect_used)]
pub fn seed() -> &'static Seed {
    static SEED: OnceLock<Seed> = OnceLock::new();
    SEED.get_or_init(|| load().expect("embedded seed data is valid"))
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
        tables.push(JTable::from_descr(txn.table(&name)?.descr()));
    }
    write_json(out, "tables.json", &tables)?;

    let dbschema = ib.dbschema_text()?;
    let schema = LogicalSchema::from_text(&dbschema)?;
    if schema.to_text() != dbschema {
        return Err(bad("DBSCHEMA does not round-trip"));
    }
    let jschema: Vec<JLTable> = schema.tables.iter().map(JLTable::from_table).collect();
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
