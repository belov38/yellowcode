use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use onecd::{BlobVal, Database, OpenOptions, Row, Value};

use crate::{Error, Uuid16};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawUserRow {
    pub id: [u8; 16],
    pub name: String,
    pub descr: String,
    pub osname: Option<String>,
    pub changed: String,
    pub rolesid: u32,
    pub show: bool,
    pub data: Vec<u8>,
    pub eauth: bool,
    pub admrole: bool,
    pub ussprh: u64,
    pub email: String,
}

fn src(e: impl std::fmt::Display) -> Error {
    Error::Source(e.to_string())
}

fn ancestor_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|a| a.join("tests/conformance/manifest.json").is_file())
        .map(Path::to_path_buf)
}

pub fn out_dir() -> Option<PathBuf> {
    let p = match std::env::var_os("ONEC_OUT") {
        Some(p) => PathBuf::from(p),
        None => ancestor_root()?.join("out"),
    };
    p.is_dir().then_some(p)
}

pub fn project_root() -> Option<PathBuf> {
    match std::env::var_os("ONEC_PROJECT_ROOT") {
        Some(p) => Some(PathBuf::from(p)),
        None => ancestor_root(),
    }
}

pub fn bench_passwords(bench: &str) -> BTreeMap<String, String> {
    let Some(path) = project_root().map(|r| r.join("tests/conformance/benches.toml")) else {
        return BTreeMap::new();
    };
    let Some(registry) = std::fs::read_to_string(path)
        .ok()
        .and_then(|body| body.parse::<toml::Table>().ok())
    else {
        return BTreeMap::new();
    };
    registry
        .get("bench")
        .and_then(|b| b.get(bench))
        .and_then(|b| b.get("users"))
        .and_then(toml::Value::as_table)
        .map(|users| {
            users
                .iter()
                .filter_map(|(name, p)| Some((name.clone(), p.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

pub fn rights_base() -> Option<PathBuf> {
    let p = out_dir()?.join("runtime/rights/ib/1Cv8.1CD");
    p.is_file().then_some(p)
}

fn field<'a>(row: &'a Row, i: usize, name: &str) -> Result<&'a Value, Error> {
    row.get(i)
        .ok_or_else(|| src(format!("row has no field {name}")))
}

fn text(v: &Value, name: &str) -> Result<String, Error> {
    match v {
        Value::Char(s) | Value::VarChar(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        o => Err(src(format!("{name}: expected string, got {o:?}"))),
    }
}

fn flag(v: &Value, name: &str) -> Result<bool, Error> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Numeric(n) => Ok(n.digits().iter().any(|&d| d != 0)),
        o => Err(src(format!("{name}: expected bool, got {o:?}"))),
    }
}

fn number<T: std::str::FromStr>(v: &Value, name: &str) -> Result<T, Error> {
    match v {
        Value::Numeric(n) => n
            .to_string()
            .parse()
            .map_err(|_| src(format!("{name}: {n}"))),
        o => Err(src(format!("{name}: expected numeric, got {o:?}"))),
    }
}

pub fn read_rows(path_1cd: &Path) -> Result<Vec<RawUserRow>, Error> {
    let db = Database::open(path_1cd, OpenOptions::read_only()).map_err(src)?;
    let txn = db.read();
    let t = txn.table("V8USERS").map_err(src)?;
    let idx = |name: &str| {
        t.descr()
            .field(name)
            .map(|(i, _)| i)
            .ok_or_else(|| src(format!("V8USERS has no column {name}")))
    };
    let (i_id, i_name, i_descr, i_os, i_ch) = (
        idx("ID")?,
        idx("NAME")?,
        idx("DESCR")?,
        idx("OSNAME")?,
        idx("CHANGED")?,
    );
    let (i_roles, i_show, i_data, i_eauth, i_adm, i_prh, i_mail) = (
        idx("ROLESID")?,
        idx("SHOW")?,
        idx("DATA")?,
        idx("EAUTH")?,
        idx("ADMROLE")?,
        idx("USSPRH")?,
        idx("EMAIL")?,
    );
    let mut out = Vec::new();
    for item in t.scan() {
        let (_, row) = item.map_err(src)?;
        let f = |i, n| field(&row, i, n);
        let id = match f(i_id, "ID")? {
            Value::Binary(b) => {
                <[u8; 16]>::try_from(b.as_slice()).map_err(|_| src("ID is not 16 bytes"))?
            }
            o => return Err(src(format!("ID: expected binary, got {o:?}"))),
        };
        let osname = match f(i_os, "OSNAME")? {
            Value::Null => None,
            v => Some(text(v, "OSNAME")?),
        };
        let changed = match f(i_ch, "CHANGED")? {
            Value::DateTime(d) => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            ),
            o => return Err(src(format!("CHANGED: expected datetime, got {o:?}"))),
        };
        let data = match f(i_data, "DATA")? {
            Value::Image(BlobVal::Data(b))
            | Value::Text(BlobVal::Data(b))
            | Value::NText(BlobVal::Data(b)) => b.clone(),
            Value::Image(BlobVal::Ref(r))
            | Value::Text(BlobVal::Ref(r))
            | Value::NText(BlobVal::Ref(r)) => t.blob_bytes(*r).map_err(src)?,
            Value::Binary(b) => b.clone(),
            o => return Err(src(format!("DATA: expected blob, got {o:?}"))),
        };
        out.push(RawUserRow {
            id,
            name: text(f(i_name, "NAME")?, "NAME")?,
            descr: text(f(i_descr, "DESCR")?, "DESCR")?,
            osname,
            changed,
            rolesid: number(f(i_roles, "ROLESID")?, "ROLESID")?,
            show: flag(f(i_show, "SHOW")?, "SHOW")?,
            data,
            eauth: flag(f(i_eauth, "EAUTH")?, "EAUTH")?,
            admrole: flag(f(i_adm, "ADMROLE")?, "ADMROLE")?,
            ussprh: number(f(i_prh, "USSPRH")?, "USSPRH")?,
            email: text(f(i_mail, "EMAIL")?, "EMAIL")?,
        });
    }
    Ok(out)
}

fn text_order(stored: &[u8; 16]) -> Uuid16 {
    let mut out = [0u8; 16];
    let parts: [(usize, usize); 4] = [(12, 4), (10, 2), (8, 2), (0, 8)];
    let mut at = 0;
    for (start, len) in parts {
        for k in 0..len {
            if let (Some(o), Some(s)) = (out.get_mut(at), stored.get(start + k)) {
                *o = *s;
            }
            at += 1;
        }
    }
    Uuid16::from_bytes(out)
}

pub fn decoded_texts(rows: &[RawUserRow]) -> Result<Vec<String>, Error> {
    let path = out_dir()
        .ok_or_else(|| src("out/ not found"))?
        .join("runtime/rights/v8users.jsonl");
    let body = std::fs::read_to_string(&path).map_err(src)?;
    let mut by_id: Vec<(Uuid16, String)> = Vec::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).map_err(src)?;
        let id = v
            .get("ID")
            .and_then(|x| x.as_str())
            .and_then(Uuid16::parse)
            .ok_or_else(|| src("jsonl row without a valid ID"))?;
        let t = v
            .get("data_text")
            .and_then(|x| x.as_str())
            .ok_or_else(|| src("jsonl row without data_text"))?;
        by_id.push((id, t.to_owned()));
    }
    rows.iter()
        .map(|r| {
            let want = text_order(&r.id);
            by_id
                .iter()
                .find(|(id, _)| *id == want)
                .map(|(_, t)| t.clone())
                .ok_or_else(|| src(format!("no jsonl entry for user {}", r.name)))
        })
        .collect()
}
