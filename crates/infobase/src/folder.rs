use onecd::{BlobVal, Numeric, Row, RowId, TableDescr, TableRead, Value, WriteTxn};

use crate::error::{Error, Result};
use crate::raw::FolderFile;

pub const PART_SIZE: usize = 10_000_000;

fn bad(table: &str, why: &str) -> Error {
    Error::Inconsistent(format!("{table}: {why}"))
}

struct Columns {
    name: usize,
    creation: usize,
    modified: usize,
    attributes: usize,
    size: usize,
    data: usize,
    part: usize,
}

impl Columns {
    fn read(d: &TableDescr) -> Result<Self> {
        let seed = crate::seed::seed();
        let expected = seed
            .tables
            .iter()
            .find(|t| {
                t.name.eq_ignore_ascii_case(&d.name)
                    && matches!(
                        t.name.as_str(),
                        "CONFIG"
                            | "CONFIGSAVE"
                            | "PARAMS"
                            | "FILES"
                            | "DEPOTFILES"
                            | "CONFIGCAS"
                            | "CONFIGCASSAVE"
                    )
            })
            .ok_or_else(|| {
                Error::Unsupported(format!("{}: not a supported folder table", d.name))
            })?;
        if d.recordlock
            || d.fields.len() != expected.fields.len()
            || expected.fields.iter().any(|f| {
                d.field(&f.name).is_none_or(|(_, actual)| {
                    actual.ty != f.ty
                        || actual.length != f.length
                        || actual.precision != f.precision
                        || actual.nullable != f.nullable
                        || actual.case_sensitive != f.case_sensitive
                })
            })
        {
            return Err(Error::Unsupported(format!(
                "{}: folder descriptor differs from 8.5.1 seed",
                d.name
            )));
        }
        let col = |name: &str| {
            d.field(name)
                .map(|(i, _)| i)
                .ok_or_else(|| bad(&d.name, "missing folder column"))
        };
        Ok(Self {
            name: col("FILENAME")?,
            creation: col("CREATION")?,
            modified: col("MODIFIED")?,
            attributes: col("ATTRIBUTES")?,
            size: col("DATASIZE")?,
            data: col("BINARYDATA")?,
            part: col("PARTNO")?,
        })
    }
}

struct Parts {
    file: FolderFile,
    rows: Vec<(u64, RowId, usize)>,
}

fn cell<'a>(row: &'a Row, i: usize, table: &str) -> Result<&'a Value> {
    row.get(i).ok_or_else(|| bad(table, "short folder row"))
}

fn number(row: &Row, i: usize, table: &str) -> Result<Numeric> {
    match cell(row, i, table)? {
        Value::Numeric(n) => Ok(n.clone()),
        _ => Err(bad(table, "expected numeric folder column")),
    }
}

fn unsigned(n: &Numeric, table: &str) -> Result<u64> {
    n.to_string()
        .parse()
        .map_err(|_| bad(table, "expected nonnegative integer"))
}

struct StoredPart {
    number: u64,
    id: RowId,
    bytes: usize,
    creation: onecd::DateTime,
    modified: onecd::DateTime,
    attributes: Numeric,
    size: Numeric,
}

fn collect(t: &TableRead<'_>, c: &Columns, name: &str) -> Result<Option<Parts>> {
    let table = &t.descr().name;
    t.strings_equal("FILENAME", name, name)?;
    let mut spelling: Option<String> = None;
    let mut found: Vec<StoredPart> = Vec::new();
    for item in t.scan() {
        let (id, row) = item?;
        let stored = match cell(&row, c.name, table)? {
            Value::VarChar(s) => s,
            _ => return Err(bad(table, "FILENAME is not NVC")),
        };
        if !t.strings_equal("FILENAME", stored, name)? {
            continue;
        }
        match &spelling {
            Some(first) if first != stored => return Err(bad(table, "ambiguous file spelling")),
            Some(_) => {}
            None => spelling = Some(stored.clone()),
        }
        let date = |i| match cell(&row, i, table)? {
            Value::DateTime(at) => Ok(*at),
            _ => Err(bad(table, "expected folder timestamp")),
        };
        let number = unsigned(&number(&row, c.part, table)?, table)?;
        let bytes = match cell(&row, c.data, table)? {
            Value::Image(BlobVal::Data(b)) => b.len(),
            Value::Image(BlobVal::Ref(r)) => {
                usize::try_from(r.len).map_err(|_| bad(table, "blob length overflow"))?
            }
            _ => return Err(bad(table, "BINARYDATA is not image")),
        };
        found.push(StoredPart {
            number,
            id,
            bytes,
            creation: date(c.creation)?,
            modified: date(c.modified)?,
            attributes: self::number(&row, c.attributes, table)?,
            size: self::number(&row, c.size, table)?,
        });
    }
    let Some(spelling) = spelling else {
        return Ok(None);
    };
    found.sort_by_key(|p| p.number);
    let mut total = 0usize;
    for (i, part) in found.iter().enumerate() {
        if u64::try_from(i).ok() != Some(part.number) {
            return Err(bad(table, "duplicate or noncontiguous PARTNO"));
        }
        if part.bytes > PART_SIZE
            || (i + 1 < found.len() && part.bytes != PART_SIZE)
            || (found.len() > 1 && part.bytes == 0)
        {
            return Err(bad(table, "invalid folder part length"));
        }
        total = total
            .checked_add(part.bytes)
            .ok_or_else(|| bad(table, "folder length overflow"))?;
    }
    let Some(first) = found.first() else {
        return Ok(None);
    };
    for pair in found.windows(2) {
        let [before, part] = pair else { continue };
        let refuse = |what: &str| bad(table, &format!("part {} of a file: {what}", part.number));
        if part.creation != first.creation {
            return Err(refuse("Creation differs from part 0"));
        }
        if part.attributes != first.attributes {
            return Err(refuse("Attributes differs from part 0"));
        }
        if part.size != first.size {
            return Err(refuse("DataSize differs from part 0"));
        }
        if part.modified < before.modified {
            return Err(refuse(&format!(
                "Modified is earlier than part {}",
                before.number
            )));
        }
    }
    if u64::try_from(total).ok() != Some(unsigned(&first.size, table)?) {
        return Err(bad(table, "DATASIZE differs from total stored length"));
    }
    Ok(Some(Parts {
        file: FolderFile {
            name: spelling,
            creation: first.creation,
            modified: first.modified,
            attributes: first.attributes.clone(),
            data_size: first.size.clone(),
            data: Vec::new(),
        },
        rows: found.iter().map(|p| (p.number, p.id, p.bytes)).collect(),
    }))
}

pub fn read_file(txn: &WriteTxn<'_>, table: &str, name: &str) -> Result<Option<FolderFile>> {
    let t = txn.read_table(table)?;
    let c = Columns::read(t.descr())?;
    let Some(mut parts) = collect(&t, &c, name)? else {
        return Ok(None);
    };
    let size = usize::try_from(unsigned(&parts.file.data_size, table)?)
        .map_err(|_| bad(table, "folder size overflow"))?;
    parts
        .file
        .data
        .try_reserve_exact(size)
        .map_err(|_| Error::Unsupported("folder allocation failed".into()))?;
    for (_, id, expected_len) in parts.rows {
        let row = t
            .get(id)?
            .ok_or_else(|| bad(table, "folder part vanished"))?;
        let bytes = match cell(&row, c.data, table)? {
            Value::Image(BlobVal::Data(b)) => b.clone(),
            Value::Image(BlobVal::Ref(r)) => t.blob_bytes(*r)?,
            _ => return Err(bad(table, "BINARYDATA is not image")),
        };
        if bytes.len() != expected_len {
            return Err(bad(table, "blob length differs from part length"));
        }
        parts.file.data.extend_from_slice(&bytes);
    }
    Ok(Some(parts.file))
}

pub fn write_file(txn: &mut WriteTxn<'_>, table: &str, f: &FolderFile) -> Result<()> {
    let result = write_inner(txn, table, f);
    if result.is_err() {
        txn.abort();
    }
    result
}

pub fn insert_new(txn: &mut WriteTxn<'_>, table: &str, f: &FolderFile) -> Result<()> {
    let result = insert_new_inner(txn, table, f);
    if result.is_err() {
        txn.abort();
    }
    result
}

fn insert_new_inner(txn: &mut WriteTxn<'_>, table: &str, f: &FolderFile) -> Result<()> {
    let t = txn.read_table(table)?;
    let c = Columns::read(t.descr())?;
    let rows = part_rows(t.descr(), &c, table, f, f.creation)?;
    drop(t);
    let mut writer = txn.table(table)?;
    for row in rows {
        writer.insert(&row)?;
    }
    Ok(())
}

pub(crate) fn insert_new_part(
    txn: &mut WriteTxn<'_>,
    table: &str,
    f: &FolderFile,
    part: u64,
    bytes: Vec<u8>,
) -> Result<()> {
    let result = (|| {
        let t = txn.read_table(table)?;
        let c = Columns::read(t.descr())?;
        let size = unsigned(&f.data_size, table)?;
        let start = part
            .checked_mul(PART_SIZE as u64)
            .ok_or_else(|| bad(table, "part offset overflow"))?;
        let expected = size.saturating_sub(start).min(PART_SIZE as u64);
        if start > size
            || (start == size && (size != 0 || part != 0))
            || bytes.len() as u64 != expected
        {
            return Err(bad(table, "invalid folder part length"));
        }
        let part = i64::try_from(part).map_err(|_| bad(table, "PARTNO overflow"))?;
        let mut row = vec![Value::Null; t.descr().fields.len()];
        for (index, value) in [
            (c.name, Value::VarChar(f.name.clone())),
            (c.creation, Value::DateTime(f.creation)),
            (c.modified, Value::DateTime(f.modified)),
            (c.attributes, Value::Numeric(f.attributes.clone())),
            (c.size, Value::Numeric(f.data_size.clone())),
            (c.data, Value::Image(BlobVal::Data(bytes))),
            (c.part, Value::Numeric(Numeric::from_i64(part))),
        ] {
            *row.get_mut(index)
                .ok_or_else(|| bad(table, "folder column outside row"))? = value;
        }
        drop(t);
        txn.table(table)?.insert(&row)?;
        Ok(())
    })();
    if result.is_err() {
        txn.abort();
    }
    result
}

pub fn part_count(len: usize) -> usize {
    len.div_ceil(PART_SIZE).max(1)
}

fn write_inner(txn: &mut WriteTxn<'_>, table: &str, f: &FolderFile) -> Result<()> {
    let t = txn.read_table(table)?;
    let c = Columns::read(t.descr())?;
    let old = collect(&t, &c, &f.name)?;
    let creation = old.as_ref().map_or(f.creation, |old| old.file.creation);
    let rows = part_rows(t.descr(), &c, table, f, creation)?;
    drop(t);
    let mut writer = txn.table(table)?;
    if let Some(old) = old {
        for (_, id, _) in old.rows {
            writer.delete(id)?;
        }
    }
    for row in rows {
        writer.insert(&row)?;
    }
    Ok(())
}

fn part_rows(
    d: &TableDescr,
    c: &Columns,
    table: &str,
    f: &FolderFile,
    creation: onecd::DateTime,
) -> Result<Vec<Row>> {
    let full_len = i64::try_from(f.data.len()).map_err(|_| bad(table, "folder length overflow"))?;
    let size = Numeric::from_i64(full_len);
    let mut rows = Vec::new();
    let count = part_count(f.data.len());
    for i in 0..count {
        let start = i
            .checked_mul(PART_SIZE)
            .ok_or_else(|| bad(table, "part offset overflow"))?;
        let end = start.saturating_add(PART_SIZE).min(f.data.len());
        let bytes = f
            .data
            .get(start..end)
            .ok_or_else(|| bad(table, "part range overflow"))?;
        let mut row = vec![Value::Null; d.fields.len()];
        let values = [
            (c.name, Value::VarChar(f.name.clone())),
            (c.creation, Value::DateTime(creation)),
            (c.modified, Value::DateTime(f.modified)),
            (c.attributes, Value::Numeric(f.attributes.clone())),
            (c.size, Value::Numeric(size.clone())),
            (c.data, Value::Image(BlobVal::Data(bytes.to_vec()))),
            (
                c.part,
                Value::Numeric(Numeric::from_i64(
                    i64::try_from(i).map_err(|_| bad(table, "PARTNO overflow"))?,
                )),
            ),
        ];
        for (index, value) in values {
            *row.get_mut(index)
                .ok_or_else(|| bad(table, "folder column outside row"))? = value;
        }
        rows.push(row);
    }
    Ok(rows)
}

pub fn delete_file(txn: &mut WriteTxn<'_>, table: &str, name: &str) -> Result<bool> {
    let result = delete_inner(txn, table, name);
    if result.is_err() {
        txn.abort();
    }
    result
}

fn delete_inner(txn: &mut WriteTxn<'_>, table: &str, name: &str) -> Result<bool> {
    let t = txn.read_table(table)?;
    let c = Columns::read(t.descr())?;
    let old = collect(&t, &c, name)?;
    drop(t);
    let Some(old) = old else {
        return Ok(false);
    };
    let mut writer = txn.table(table)?;
    for (_, id, _) in old.rows {
        writer.delete(id)?;
    }
    Ok(true)
}
