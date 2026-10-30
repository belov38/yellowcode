use std::{
    collections::HashSet,
    io::{Read, Write},
};

use dtfile::{DtWriter, Source, TableHead};
use ibschema::{
    columns::{ColRole, ColType, Column, Origin, Part},
    values::ColumnValue,
};
use onecd::Numeric;

const MAX_ROW_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_FOLDER_BYTES: usize = 64 * 1024 * 1024;
const MAX_PARTS_PER_FILE: u64 = 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported SQL folder: {0}")]
    Unsupported(&'static str),
    #[error("invalid SQL folder catalog: {0}")]
    Catalog(#[from] crate::sql_catalog::Error),
    #[error("writing DT folder: {0}")]
    Dt(#[from] dtfile::Error),
    #[error("reading SQL folder: {0}")]
    Sql(#[from] crate::sql_rows::Error),
}

pub fn write_staged_config<W: Write>(
    writer: &mut DtWriter<W>,
    spool: &mut crate::sql_config_spool::Spool,
) -> Result<u64, Error> {
    let head = TableHead::system(&Source::Folder("Config".into()))
        .ok_or(Error::Unsupported("missing Config layout"))?;
    let creation = head
        .columns
        .get(1)
        .ok_or(Error::Unsupported("missing creation column"))?;
    let modified = head
        .columns
        .get(2)
        .ok_or(Error::Unsupported("missing modified column"))?;
    let count = spool.rows().len();
    let year_offset = spool.year_offset();
    writer.begin_table(&head)?;
    for index in 0..count {
        let row = spool
            .rows()
            .get(index)
            .ok_or(Error::Unsupported("missing staged entry"))?
            .clone();
        if row.stored_size == 64 {
            let mut prefix = [0_u8; 8];
            spool
                .reader(index)
                .map_err(|_| Error::Unsupported("staged entry read"))?
                .read_exact(&mut prefix)
                .map_err(|_| Error::Unsupported("staged entry read"))?;
            if &prefix == b"STORHDR\0" {
                return Err(Error::Unsupported("unresolved binary storage reference"));
            }
        }
        let date = |column, value| {
            crate::sql_values::convert_cell(
                column,
                crate::sql_values::SqlCell::Date(value),
                year_offset,
            )
            .map_err(|_| Error::Unsupported("unsupported staged timestamp"))
        };
        let metadata = [
            ColumnValue::Str(row.name),
            date(creation, row.creation)?,
            date(modified, row.modified)?,
            ColumnValue::Number(Numeric::from_i64(row.attributes)),
            ColumnValue::Number(
                Numeric::parse(&row.stored_size.to_string())
                    .ok_or(Error::Unsupported("invalid staged data size"))?,
            ),
        ];
        let mut input = spool
            .reader(index)
            .map_err(|_| Error::Unsupported("staged entry read"))?;
        writer.row_folder_stream(&metadata, row.stored_size, &mut input)?;
    }
    writer.end_table()?;
    u64::try_from(count).map_err(|_| Error::Unsupported("folder count overflow"))
}

pub fn source_columns(head: &TableHead) -> Result<Vec<Column>, Error> {
    let Source::Folder(folder) = &head.source else {
        return Err(Error::Unsupported("expected folder"));
    };
    if ![
        "Config",
        "ConfigSave",
        "Params",
        "Files",
        "DepotFiles",
        "ConfigCAS",
        "ConfigCASSave",
    ]
    .contains(&folder.as_str())
        || TableHead::system(&head.source).as_ref() != Some(head)
    {
        return Err(Error::Unsupported("unsupported folder layout"));
    }
    let mut columns = head.columns.as_ref().clone();
    columns.push(Column {
        name: "PARTNO".into(),
        ty: ColType::N(10, 0),
        role: ColRole::Number,
        nullable: false,
        origin: Origin::Field(columns.len()),
        part: Part::Single,
    });
    Ok(columns)
}

pub fn write_sql_folder_multipart<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    actual: &[crate::sql_catalog::SqlColumn],
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    struct Pending {
        name: String,
        values: Vec<ColumnValue>,
        next_part: u64,
    }
    fn finish<W: Write>(writer: &mut DtWriter<W>, file: Pending) -> Result<(), String> {
        let Some(ColumnValue::Number(size)) = file.values.get(4) else {
            return Err("missing file size".into());
        };
        let Some(ColumnValue::Bytes(data)) = file.values.get(5) else {
            return Err("missing file data".into());
        };
        let actual = Numeric::parse(&data.len().to_string()).ok_or("invalid data length")?;
        if !size.eq_value(&actual) {
            return Err("folder data length mismatch".into());
        }
        if data.len() == 64 && data.starts_with(b"STORHDR\0") {
            return Err("unresolved SQL binary storage reference".into());
        }
        writer.row(&file.values).map_err(|e| e.to_string())
    }
    let columns = source_columns(head)?;
    if actual.len() != columns.len() {
        return Err(Error::Unsupported("unexpected folder catalog columns"));
    }
    crate::sql_catalog::validate_columns(&columns, actual)?;
    let zero = Numeric::from_i64(0);
    let mut names = HashSet::new();
    let mut pending: Option<Pending> = None;
    let mut count = 0_u64;
    writer.begin_table(head)?;
    crate::sql_rows::read_rows(input, MAX_ROW_BYTES, |row| {
        let mut values = crate::sql_json_row::convert_json_row(&columns, row, year_offset)
            .map_err(|e| e.to_string())?;
        let Some(ColumnValue::Number(part)) = values.pop() else {
            return Err("missing part number".into());
        };
        let [
            ColumnValue::Str(name),
            _,
            _,
            ColumnValue::Number(attributes),
            _,
            ColumnValue::Bytes(data),
        ] = values.as_slice()
        else {
            return Err("invalid folder row".into());
        };
        if name.is_empty() || !name.is_ascii() || name.chars().any(char::is_control) {
            return Err("unsupported folder name".into());
        }
        if !attributes.eq_value(&zero) || data.len() > MAX_FOLDER_BYTES {
            return Err("unsupported folder attributes or size".into());
        }
        if pending.as_ref().is_none_or(|file| file.name != *name) {
            if let Some(file) = pending.take() {
                finish(writer, file)?;
                count = count.checked_add(1).ok_or("folder count overflow")?;
            }
            if names.len() >= MAX_FILES || !names.insert(name.to_ascii_lowercase()) {
                return Err("duplicate or excessive folder names".into());
            }
            if !part.eq_value(&zero) {
                return Err("first folder part is not zero".into());
            }
            pending = Some(Pending {
                name: name.clone(),
                values,
                next_part: 1,
            });
            return Ok(());
        }
        let file = pending.as_mut().ok_or("missing current folder file")?;
        if file.next_part >= MAX_PARTS_PER_FILE
            || !part.eq_value(&Numeric::parse(&file.next_part.to_string()).ok_or("part overflow")?)
        {
            return Err("duplicate, missing or excessive folder part".into());
        }
        if values.get(1..5) != file.values.get(1..5) {
            return Err("inconsistent multipart metadata".into());
        }
        let Some(ColumnValue::Bytes(bytes)) = values.pop() else {
            return Err("missing part data".into());
        };
        let Some(ColumnValue::Bytes(data)) = file.values.get_mut(5) else {
            return Err("missing file data".into());
        };
        if data
            .len()
            .checked_add(bytes.len())
            .is_none_or(|len| len > MAX_FOLDER_BYTES)
        {
            return Err("folder data exceeds limit".into());
        }
        data.extend_from_slice(&bytes);
        file.next_part = file.next_part.checked_add(1).ok_or("part overflow")?;
        Ok(())
    })?;
    if let Some(file) = pending {
        finish(writer, file).map_err(|_| Error::Unsupported("incomplete multipart folder"))?;
        count = count
            .checked_add(1)
            .ok_or(Error::Unsupported("folder count overflow"))?;
    }
    writer.end_table()?;
    Ok(count)
}

pub fn write_sql_folder<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    actual: &[crate::sql_catalog::SqlColumn],
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_sql_folder_inner(writer, head, actual, input, year_offset, false)
}

pub fn write_legacy_sql_folder<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    actual: &[crate::sql_catalog::SqlColumn],
    input: impl Read,
    year_offset: i32,
) -> Result<u64, Error> {
    write_sql_folder_inner(writer, head, actual, input, year_offset, true)
}

fn write_sql_folder_inner<W: Write>(
    writer: &mut DtWriter<W>,
    head: &TableHead,
    actual: &[crate::sql_catalog::SqlColumn],
    input: impl Read,
    year_offset: i32,
    legacy: bool,
) -> Result<u64, Error> {
    let columns = source_columns(head)?;
    if actual.len() != columns.len() {
        return Err(Error::Unsupported("unexpected folder catalog columns"));
    }
    if legacy {
        crate::sql_catalog::validate_legacy_columns(&columns, actual)?;
    } else {
        crate::sql_catalog::validate_columns(&columns, actual)?;
    }
    let zero = Numeric::parse("0").ok_or(Error::Unsupported("invalid zero"))?;
    let mut names = HashSet::new();
    let mut count = 0_u64;
    writer.begin_table(head)?;
    crate::sql_rows::read_rows(input, MAX_ROW_BYTES, |row| {
        let values = crate::sql_json_row::convert_json_row(&columns, row, year_offset)
            .map_err(|e| e.to_string())?;
        let [
            ColumnValue::Str(name),
            _,
            _,
            ColumnValue::Number(attributes),
            ColumnValue::Number(size),
            ColumnValue::Bytes(data),
            ColumnValue::Number(part),
        ] = values.as_slice()
        else {
            return Err("invalid folder row".into());
        };
        if name.is_empty()
            || !name.is_ascii()
            || name.chars().any(char::is_control)
            || names.len() >= MAX_FILES
            || !names.insert(name.to_ascii_lowercase())
        {
            return Err("unsupported or duplicate folder name".into());
        }
        if !part.eq_value(&zero) || !attributes.eq_value(&zero) {
            return Err("multipart or attributed folder entry".into());
        }
        let actual_size = Numeric::parse(&data.len().to_string()).ok_or("invalid data length")?;
        if !size.eq_value(&actual_size) {
            return Err("folder data length mismatch".into());
        }
        let row = values
            .get(..head.columns.len())
            .ok_or("invalid folder row length")?;
        writer.row(row).map_err(|e| e.to_string())?;
        count = count.checked_add(1).ok_or("folder row count overflow")?;
        Ok(())
    })?;
    writer.end_table()?;
    Ok(count)
}
