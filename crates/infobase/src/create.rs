use std::path::{Path, PathBuf};

use onecd::{
    BlobVal, DateTime, FieldType, Format, Locking, Numeric, OpenOptions, Row, TableDescr, Value,
};

use crate::error::{Error, Result};
use crate::params::{ParamsValues, empty_base_files};
use crate::seed::seed;

const DB_FILE: &str = "1Cv8.1CD";

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub locale: String,
    pub page_size: u32,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            locale: "ru_RU".to_string(),
            page_size: 8192,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateReport {
    pub dir: PathBuf,
    pub tables: usize,
    pub values: ParamsValues,
}

pub fn create_empty(dir: &Path, opts: &CreateOptions) -> Result<CreateReport> {
    create_empty_with(dir, opts, &ParamsValues::random_empty_base(), now_utc())
}

#[doc(hidden)]
pub fn create_empty_with(
    dir: &Path,
    opts: &CreateOptions,
    v: &ParamsValues,
    now: DateTime,
) -> Result<CreateReport> {
    if dir.exists() {
        return Err(Error::Inconsistent(format!(
            "cannot create infobase: {} already exists",
            dir.display()
        )));
    }
    let mut tmp_name = dir
        .file_name()
        .ok_or_else(|| Error::Inconsistent(format!("bad infobase path {}", dir.display())))?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.with_file_name(tmp_name);
    if tmp.exists() {
        return Err(Error::Inconsistent(format!(
            "cannot create infobase: {} exists (left by an interrupted creation?); remove it",
            tmp.display()
        )));
    }
    std::fs::create_dir(&tmp)
        .map_err(|e| Error::Inconsistent(format!("create {}: {e}", tmp.display())))?;
    let result = build(&tmp, opts, v, now).and_then(|tables| {
        std::fs::rename(&tmp, dir).map_err(|e| {
            Error::Inconsistent(format!(
                "rename {} to {}: {e}",
                tmp.display(),
                dir.display()
            ))
        })?;
        Ok(tables)
    });
    match result {
        Ok(tables) => Ok(CreateReport {
            dir: dir.to_path_buf(),
            tables,
            values: v.clone(),
        }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

fn build(tmp: &Path, opts: &CreateOptions, v: &ParamsValues, now: DateTime) -> Result<usize> {
    let s = seed();
    let db = onecd::Database::create(
        &tmp.join(DB_FILE),
        onecd::CreateOptions {
            format: Format::V838,
            page_size: opts.page_size,
            locale: opts.locale.clone(),
            open: OpenOptions {
                locking: Locking::None,
                ..OpenOptions::read_write()
            },
        },
    )?;
    let descr = |n: &str| {
        s.tables
            .iter()
            .find(|d| d.name == n)
            .ok_or_else(|| Error::Inconsistent(format!("seed has no table {n}")))
    };
    {
        let mut txn = db.write()?;
        for d in &s.tables {
            txn.create_table(d)?;
        }
        {
            let mut t = txn.table("IBVERSION")?;
            let row = RowBuilder::new(descr("IBVERSION")?)
                .num("IBVERSION", i64::from(s.ibversion.0))?
                .num("PLATFORMVERSIONREQ", i64::from(s.ibversion.1))?
                .done();
            t.insert(&row)?;
        }
        {
            let mut t = txn.table("PARAMS")?;
            for f in empty_base_files(v, &s.dbnames, &opts.locale)? {
                let row = RowBuilder::new(descr("PARAMS")?)
                    .text("FILENAME", &f.name)?
                    .date("CREATION", now)?
                    .date("MODIFIED", now)?
                    .num("ATTRIBUTES", 0)?
                    .num("DATASIZE", f.stored.len() as i64)?
                    .blob("BINARYDATA", f.stored)?
                    .num("PARTNO", 0)?
                    .done();
                t.insert(&row)?;
            }
        }
        let schema = s.schema.to_text();
        {
            let mut t = txn.table("DBSCHEMA")?;
            let row = RowBuilder::new(descr("DBSCHEMA")?)
                .blob("SERIALIZEDDATA", schema.clone())?
                .done();
            t.insert(&row)?;
        }
        {
            let mut t = txn.table("SCHEMASTORAGE")?;
            let row = RowBuilder::new(descr("SCHEMASTORAGE")?)
                .num("SCHEMAID", i64::from(s.schemastorage.0))?
                .num("STATUS", i64::from(s.schemastorage.1))?
                .blob("CURRENTSCHEMA", schema)?
                .blob("NEWGENCREATED", s.newgen.clone())?
                .blob("NEWGENDROPPED", s.newgen.clone())?
                .done();
            t.insert(&row)?;
        }
        txn.commit()?;
    }
    let report = db.verify()?;
    if !report.violations.is_empty() {
        let first: Vec<String> = report
            .violations
            .iter()
            .take(5)
            .map(|x| format!("{x:?}"))
            .collect();
        return Err(Error::Inconsistent(format!(
            "new infobase fails verification ({} violations): {}",
            report.violations.len(),
            first.join("; ")
        )));
    }
    Ok(s.tables.len())
}

struct RowBuilder<'a> {
    descr: &'a TableDescr,
    row: Row,
}

impl<'a> RowBuilder<'a> {
    fn new(descr: &'a TableDescr) -> Self {
        let row = descr
            .fields
            .iter()
            .map(|f| match f.ty {
                FieldType::RV => Value::RowVersion([0; 4]),
                _ => Value::Null,
            })
            .collect();
        RowBuilder { descr, row }
    }

    fn set(mut self, name: &str, f: impl FnOnce(FieldType) -> Result<Value>) -> Result<Self> {
        let (i, fd) = self.descr.field(name).ok_or_else(|| {
            Error::Inconsistent(format!("table {} has no column {name}", self.descr.name))
        })?;
        let val = f(fd.ty)
            .map_err(|e| Error::Inconsistent(format!("{}.{name}: {e}", self.descr.name)))?;
        let slot = self.row.get_mut(i).ok_or_else(|| {
            Error::Inconsistent(format!("{}.{name}: bad column index", self.descr.name))
        })?;
        *slot = val;
        Ok(self)
    }

    fn num(self, name: &str, n: i64) -> Result<Self> {
        self.set(name, |_| Ok(Value::Numeric(Numeric::from_i64(n))))
    }

    fn text(self, name: &str, s: &str) -> Result<Self> {
        self.set(name, |ty| match ty {
            FieldType::C | FieldType::NC => Ok(Value::Char(s.to_string())),
            FieldType::VC | FieldType::NVC => Ok(Value::VarChar(s.to_string())),
            t => Err(Error::Inconsistent(format!("not a string column: {t:?}"))),
        })
    }

    fn date(self, name: &str, d: DateTime) -> Result<Self> {
        self.set(name, |ty| match ty {
            FieldType::DT => Ok(Value::DateTime(d)),
            t => Err(Error::Inconsistent(format!("not a datetime column: {t:?}"))),
        })
    }

    fn blob(self, name: &str, bytes: Vec<u8>) -> Result<Self> {
        self.set(name, |ty| match ty {
            FieldType::T => Ok(Value::Text(BlobVal::Data(bytes))),
            FieldType::NT => Ok(Value::NText(BlobVal::Data(bytes))),
            FieldType::I => Ok(Value::Image(BlobVal::Data(bytes))),
            t => Err(Error::Inconsistent(format!("not a blob column: {t:?}"))),
        })
    }

    fn done(self) -> Row {
        self.row
    }
}

fn now_utc() -> DateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    from_unix(secs)
}

fn from_unix(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    DateTime::new(
        y as u16,
        m as u8,
        d as u8,
        (rem / 3600) as u8,
        (rem % 3600 / 60) as u8,
        (rem % 60) as u8,
    )
    .unwrap_or(DateTime::EMPTY)
}
