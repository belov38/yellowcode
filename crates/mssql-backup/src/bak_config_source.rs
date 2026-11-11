use std::{
    cmp::Ordering,
    collections::{HashMap, hash_map::Entry},
    fs::File,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    bak_catalog::{Catalog, DBO_SCHEMA_ID, ObjectName},
    bak_lob::{LobUnits, MAX_LOB_BYTES, read_lob_counted},
    bak_read::{self, PageReader, PageSource},
    bak_record::RecordId,
    bak_table::{Row, RowOptions, TableDef},
    bak_values::{ColumnDef, LobLink, LobRoot, SqlType, Value},
    config_cf::ConfigRow,
    sql_config_spool::{self, Builder, Limits, Spool},
};

pub const SPOOL_LIMITS: Limits = crate::sqlcmd::CONFIG_LIMITS;
const SQL_ROW_FIXED: u64 = 104;
pub const MAX_PART_BYTES: u64 = 100_663_176;
pub const MAX_HELD_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_TOTAL_LOB_RECORDS: u64 = 4 * 1024 * 1024;
pub const ROOT_ENTRY_BYTES: u64 = 48;

const CONFIG_TABLE: &str = "Config";
const CONFIG_TABLE_AS_EXPORTED: &str = "CONFIG";
const YEAR_OFFSET_TABLE: &str = "_YearOffset";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Names {
    #[default]
    Unknown,
    CaseInsensitive,
    Exact,
}

impl Names {
    pub fn from_collation_id(id: u32) -> Names {
        const IGNORE_CASE: u32 = 1 << 20;
        const LOCALE_MASK: u32 = 0x000F_FFFF;
        const TURKISH: u32 = 0x041F;
        const AZERI_LATIN: u32 = 0x042C;
        let locale = id & LOCALE_MASK;
        if id & IGNORE_CASE == 0 || locale == TURKISH || locale == AZERI_LATIN {
            Names::Exact
        } else {
            Names::CaseInsensitive
        }
    }

    fn config_table(self) -> &'static str {
        if self == Names::Exact {
            CONFIG_TABLE_AS_EXPORTED
        } else {
            CONFIG_TABLE
        }
    }
}

const CONFIG_RULES: &str = "the parts of a file must be numbered 0, 1, 2 ... without gaps and agree on name, dates and size, a file name is ASCII and at most 128 characters, and there must be a file named root";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Read(#[from] bak_read::Error),
    #[error("the backup has no table dbo.{table}")]
    MissingTable { table: &'static str },
    #[error("table dbo.{table} has no column {column}")]
    MissingColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("column {column} of dbo.{table} is {found}; the direct reader reads {expected}")]
    ColumnType {
        table: &'static str,
        column: &'static str,
        found: String,
        expected: &'static str,
    },
    #[error("dbo._YearOffset: {0}")]
    YearOffset(YearOffsetProblem),
    #[error("a row of dbo.Config has NULL in column {column}")]
    Null { column: &'static str },
    #[error("a file name in dbo.Config is not ASCII")]
    NonAsciiName { name: String },
    #[error(
        "a file name in dbo.Config holds half of a surrogate pair, which is not text, so it is not ASCII"
    )]
    NameNotText,
    #[error("two file names in dbo.Config differ only in trailing spaces")]
    AmbiguousNames { first: String, second: String },
    #[error(
        "the database has the tables dbo.{first} and dbo.{second}, whose names differ only in case"
    )]
    TablesDifferOnlyInCase { first: String, second: String },
    #[error(
        "the database has {what}, which SQL Server takes for dbo.{table}; the direct reader reads only the user table of that name"
    )]
    OtherObject { table: &'static str, what: String },
    #[error(
        "the database has a security policy (row-level security), which SQL Server applies to the rows it exports and the direct reader does not"
    )]
    SecurityPolicy,
    #[error(
        "table dbo.{table} has the columns {first} and {second}, whose names differ only in case"
    )]
    ColumnsDifferOnlyInCase {
        table: &'static str,
        first: String,
        second: String,
    },
    #[error("column {column} of dbo.Config: {what}")]
    Row {
        column: &'static str,
        what: &'static str,
    },
    #[error("the rows of dbo.Config do not make a configuration ({rules}): {0}", rules = CONFIG_RULES)]
    Spool(#[from] sql_config_spool::Error),
    #[error(
        "part {part_no} of a file does not fit the other rows of dbo.Config ({rules}): {source}",
        rules = CONFIG_RULES
    )]
    PartRefused {
        file: String,
        part_no: i64,
        source: sql_config_spool::Error,
    },
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    #[error("the temporary file of the parts that stayed in their rows: {0}")]
    Staging(io::Error),
    #[error(
        "part {part_no} of a file and part {other_part_no} of another file point at the same large value, which no real table does"
    )]
    SharedValue {
        file: String,
        part_no: i64,
        other_file: String,
        other_part_no: i64,
    },
    #[error(
        "the large values of dbo.Config lead to more than {limit} records in all (reading part {part_no} of a file passed the limit)"
    )]
    LobRecords {
        limit: u64,
        file: String,
        part_no: i64,
    },
    #[error(
        "part {part_no} of a file has more than the {max} bytes that one part may have (the row that carries it in the export of the SQL path would pass 128 MiB)"
    )]
    PartTooLarge {
        file: String,
        part_no: i64,
        size: Option<u64>,
        max: u64,
    },
    #[error("reading part {part_no} of a file: {source}")]
    Part {
        file: String,
        part_no: i64,
        source: bak_read::Error,
    },
    #[error(
        "a file is not intact in the backup: its part {part_no} came as {found} bytes where {expected} were announced"
    )]
    PartLength {
        file: String,
        part_no: i64,
        expected: u64,
        found: u64,
    },
    #[error(
        "a file is not intact in the backup: its parts add up to {found} bytes, but its DataSize says {expected}"
    )]
    FileSize {
        file: String,
        expected: u64,
        found: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum YearOffsetProblem {
    #[error("the table has no row")]
    NoRow,
    #[error("the table has more than one row")]
    MoreThanOneRow,
    #[error("the offset is NULL")]
    Null,
    #[error("the offset {0} is negative")]
    Negative(i64),
    #[error("the offset {0} does not fit an int")]
    OutOfRange(i64),
}

#[derive(Debug, Clone)]
pub struct Tables {
    pub config: TableDef,
    pub year_offset: TableDef,
    pub names: Names,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub max_part_bytes: u64,
    pub max_held_bytes: u64,
    pub max_lob_records: u64,
    pub accept_unreleased_versions: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_part_bytes: MAX_PART_BYTES,
            max_held_bytes: MAX_HELD_BYTES,
            max_lob_records: MAX_TOTAL_LOB_RECORDS,
            accept_unreleased_versions: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Summary {
    pub year_offset: i32,
    pub files: usize,
    pub parts: usize,
    pub bytes: u64,
    pub largest_part: u64,
    pub in_row_parts: usize,
    pub lob_parts: usize,
    pub held_bytes: u64,
    pub lob_records: u64,
    pub unreleased_rows: u64,
}

#[non_exhaustive]
pub struct Staged {
    pub spool: Spool,
    pub summary: Summary,
}

impl std::fmt::Debug for Staged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staged")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

pub fn compare_names(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn is_integer(ty: SqlType) -> bool {
    matches!(
        ty,
        SqlType::TinyInt | SqlType::SmallInt | SqlType::Int | SqlType::BigInt
    )
}

fn is_stamp(ty: SqlType) -> bool {
    matches!(ty, SqlType::DateTime2(0) | SqlType::DateTime)
}

fn is_binary(ty: SqlType) -> bool {
    matches!(ty, SqlType::VarBinary | SqlType::Image)
}

fn is_name(ty: SqlType) -> bool {
    ty == SqlType::NVarChar
}

fn describe(column: &ColumnDef) -> String {
    match column.ty {
        SqlType::DateTime2(scale) => format!("datetime2({scale})"),
        SqlType::Time(scale) => format!("time({scale})"),
        SqlType::Numeric { precision, scale } => format!("numeric({precision},{scale})"),
        SqlType::VarBinary if column.max_len == -1 => "varbinary(max)".to_owned(),
        SqlType::VarChar if column.max_len == -1 => "varchar(max)".to_owned(),
        SqlType::NVarChar if column.max_len == -1 => "nvarchar(max)".to_owned(),
        other => other.name().to_owned(),
    }
}

fn column_index(
    table: &TableDef,
    table_name: &'static str,
    column: &'static str,
    accepts: fn(SqlType) -> bool,
    expected: &'static str,
    names: Names,
) -> Result<usize, Error> {
    let mut candidates = table.columns.iter().enumerate().filter(|(_, candidate)| {
        if names == Names::Exact {
            candidate.name == column
        } else {
            candidate.name.eq_ignore_ascii_case(column)
        }
    });
    let (index, found) = candidates.next().ok_or(Error::MissingColumn {
        table: table_name,
        column,
    })?;
    if let Some((_, second)) = candidates.next() {
        return Err(Error::ColumnsDifferOnlyInCase {
            table: table_name,
            first: found.name.clone(),
            second: second.name.clone(),
        });
    }
    if !accepts(found.ty) {
        return Err(Error::ColumnType {
            table: table_name,
            column,
            found: describe(found),
            expected,
        });
    }
    Ok(index)
}

const INTEGER: &str = "tinyint, smallint, int or bigint";

fn offset_column(table: &TableDef, names: Names) -> Result<usize, Error> {
    column_index(
        table,
        YEAR_OFFSET_TABLE,
        "Offset",
        is_integer,
        INTEGER,
        names,
    )
}

struct Columns {
    name: usize,
    creation: usize,
    modified: usize,
    attributes: usize,
    data_size: usize,
    data: usize,
    part_no: usize,
}

impl Columns {
    fn of(table: &TableDef, names: Names) -> Result<Columns, Error> {
        let table_name = names.config_table();
        let find = |column, accepts, expected| {
            column_index(table, table_name, column, accepts, expected, names)
        };
        let stamp = "datetime2(0) or datetime";
        Ok(Columns {
            name: find("FileName", is_name, "nvarchar")?,
            creation: find("Creation", is_stamp, stamp)?,
            modified: find("Modified", is_stamp, stamp)?,
            attributes: find("Attributes", is_integer, INTEGER)?,
            data_size: find("DataSize", is_integer, INTEGER)?,
            data: find("BinaryData", is_binary, "varbinary or image")?,
            part_no: find("PartNo", is_integer, INTEGER)?,
        })
    }
}

fn loose(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|c| match u32::from(c) {
            0xFF01..=0xFF5E => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .collect();
    folded.trim_end_matches(' ').to_owned()
}

fn describe_object(object: &ObjectName) -> String {
    let what = match &object.kind {
        b"U " => "a user table".to_owned(),
        b"V " => "a view".to_owned(),
        b"SN" => "a synonym".to_owned(),
        b"P " | b"PC" | b"RF" => "a procedure".to_owned(),
        b"FN" | b"IF" | b"TF" | b"FS" | b"FT" | b"AF" => "a function".to_owned(),
        b"TR" | b"TA" => "a trigger".to_owned(),
        b"SQ" => "a service queue".to_owned(),
        b"SO" => "a sequence".to_owned(),
        b"SP" => "a security policy".to_owned(),
        other => format!(
            "an object of type {}",
            String::from_utf8_lossy(other).trim_end()
        ),
    };
    format!("{what} named {}", object.name)
}

fn resolve(wanted: &'static str, names: Names, found: &[ObjectName]) -> Result<(), Error> {
    let key = loose(wanted);
    let meant: Vec<&ObjectName> = found
        .iter()
        .filter(|object| object.schema_id == DBO_SCHEMA_ID && loose(&object.name) == key)
        .filter(|object| names != Names::Exact || object.name.trim_end_matches(' ') == wanted)
        .collect();
    let mut tables = meant.iter().filter(|object| object.is_user_table());
    match (meant.as_slice(), tables.next(), tables.next()) {
        ([], ..) => Err(Error::MissingTable { table: wanted }),
        ([only], ..) if only.is_user_table() && only.name == wanted => Ok(()),
        (_, Some(first), Some(second)) => Err(Error::TablesDifferOnlyInCase {
            first: first.name.clone(),
            second: second.name.clone(),
        }),
        (objects, ..) => Err(Error::OtherObject {
            table: wanted,
            what: objects
                .iter()
                .find(|object| !(object.is_user_table() && object.name == wanted))
                .or(objects.first())
                .map_or_else(String::new, |object| describe_object(object)),
        }),
    }
}

pub fn locate_tables<S: PageSource>(
    reader: &mut PageReader<S>,
    catalog: &Catalog,
) -> Result<Tables, Error> {
    locate_tables_in(reader, catalog, Names::Unknown)
}

pub fn locate_tables_in<S: PageSource>(
    reader: &mut PageReader<S>,
    catalog: &Catalog,
    names: Names,
) -> Result<Tables, Error> {
    let config_name = names.config_table();
    let wanted = [loose(CONFIG_TABLE), loose(YEAR_OFFSET_TABLE)];
    let found = catalog.objects_where(reader, |object| {
        object.kind == *b"SP"
            || (object.schema_id == DBO_SCHEMA_ID && wanted.contains(&loose(&object.name)))
    })?;
    resolve(config_name, names, &found)?;
    resolve(YEAR_OFFSET_TABLE, names, &found)?;
    if found.iter().any(|object| object.kind == *b"SP") {
        return Err(Error::SecurityPolicy);
    }
    let find = |reader: &mut PageReader<S>, table: &'static str| match catalog
        .table(reader, "dbo", table)
    {
        Err(bak_read::Error::NotFound { .. }) => Err(Error::MissingTable { table }),
        other => other.map_err(Error::Read),
    };
    let config = find(reader, config_name)?;
    let year_offset = find(reader, YEAR_OFFSET_TABLE)?;
    Columns::of(&config, names)?;
    offset_column(&year_offset, names)?;
    Ok(Tables {
        config,
        year_offset,
        names,
    })
}

pub fn read_year_offset<S: PageSource>(
    reader: &mut PageReader<S>,
    table: &TableDef,
) -> Result<i32, Error> {
    read_year_offset_in(reader, table, Names::Unknown)
}

pub fn read_year_offset_in<S: PageSource>(
    reader: &mut PageReader<S>,
    table: &TableDef,
    names: Names,
) -> Result<i32, Error> {
    read_year_offset_with(reader, table, names, RowOptions::default()).map(|(offset, _)| offset)
}

fn read_year_offset_with<S: PageSource>(
    reader: &mut PageReader<S>,
    table: &TableDef,
    names: Names,
    row_options: RowOptions,
) -> Result<(i32, u64), Error> {
    let column = offset_column(table, names)?;
    let mut rows = table.rows_with(reader, row_options)?;
    let Some(first) = rows.next() else {
        return Err(Error::YearOffset(YearOffsetProblem::NoRow));
    };
    let first = first?;
    if rows.next().transpose()?.is_some() {
        return Err(Error::YearOffset(YearOffsetProblem::MoreThanOneRow));
    }
    let offset = match first.values.get(column) {
        Some(Value::I64(offset)) => *offset,
        Some(Value::Null) => return Err(Error::YearOffset(YearOffsetProblem::Null)),
        _ => {
            return Err(Error::Read(bak_read::Error::Inconsistent(
                "the offset of _YearOffset is not an integer",
            )));
        }
    };
    if offset < 0 {
        return Err(Error::YearOffset(YearOffsetProblem::Negative(offset)));
    }
    let unreleased = rows.unreleased_records();
    i32::try_from(offset)
        .map(|offset| (offset, unreleased))
        .map_err(|_| Error::YearOffset(YearOffsetProblem::OutOfRange(offset)))
}

enum Source {
    InRow { offset: u64, len: usize },
    Lob(LobRoot),
}

const IN_ROW_BUFFER: usize = 64 * 1024;

struct InRowStore {
    file: BufWriter<File>,
    len: u64,
}

impl InRowStore {
    fn create(dir: &Path) -> Result<InRowStore, Error> {
        let file = tempfile::tempfile_in(dir).map_err(Error::Staging)?;
        Ok(InRowStore {
            file: BufWriter::with_capacity(IN_ROW_BUFFER, file),
            len: 0,
        })
    }

    fn put(&mut self, bytes: &[u8]) -> Result<Source, Error> {
        self.file.write_all(bytes).map_err(Error::Staging)?;
        let offset = self.len;
        self.len = offset.saturating_add(byte_count(bytes.len()));
        Ok(Source::InRow {
            offset,
            len: bytes.len(),
        })
    }

    fn finish(self) -> Result<File, Error> {
        self.file
            .into_inner()
            .map_err(|error| Error::Staging(error.into_error()))
    }
}

fn read_in_row(
    file: &mut File,
    offset: u64,
    len: usize,
    buffer: &mut Vec<u8>,
) -> Result<(), Error> {
    buffer.clear();
    buffer.resize(len, 0);
    file.seek(SeekFrom::Start(offset)).map_err(Error::Staging)?;
    file.read_exact(buffer).map_err(Error::Staging)
}

struct Part {
    name: String,
    creation: String,
    modified: String,
    attributes: i64,
    part_no: i64,
    data_size: u64,
    source: Source,
}

fn byte_count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn allocated(bytes: usize) -> u64 {
    if bytes == 0 {
        return 0;
    }
    byte_count(bytes.saturating_add(8).next_multiple_of(16).max(32))
}

const WRITER_FILE_BYTES: u64 = 384;
const WRITER_NAME_COPIES: u64 = 5;

fn spool_file_bytes(name: &str) -> u64 {
    2 * byte_count(size_of::<ConfigRow>() + size_of::<u64>())
        + WRITER_FILE_BYTES
        + WRITER_NAME_COPIES * allocated(name.len())
}

impl Part {
    fn cost(&self) -> u64 {
        let owned = match &self.source {
            Source::Lob(LobRoot::Inline { links, .. }) => {
                allocated(links.capacity().saturating_mul(size_of::<LobLink>()))
            }
            Source::InRow { .. } | Source::Lob(LobRoot::TextPointer { .. }) => 0,
        };
        2 * byte_count(size_of::<Part>())
            + allocated(self.name.capacity())
            + allocated(self.creation.capacity())
            + allocated(self.modified.capacity())
            + owned
            + spool_file_bytes(&self.name)
    }

    fn known_size(&self) -> Option<u64> {
        match &self.source {
            Source::InRow { len, .. } => Some(byte_count(*len)),
            Source::Lob(root) => root.declared_size(),
        }
    }
}

fn cell(values: &mut [Value], index: usize) -> Result<Value, Error> {
    values
        .get_mut(index)
        .map(|slot| std::mem::replace(slot, Value::Null))
        .ok_or(Error::Read(bak_read::Error::Inconsistent(
            "a row has fewer cells than its table has columns",
        )))
}

fn wrong_kind(column: &'static str) -> Error {
    Error::Row {
        column,
        what: "a value of another kind than the type of the column",
    }
}

fn text(value: Value, column: &'static str) -> Result<String, Error> {
    match value {
        Value::Text(text) | Value::DateTime(text) => Ok(text),
        Value::Null => Err(Error::Null { column }),
        _ => Err(wrong_kind(column)),
    }
}

fn integer(value: Value, column: &'static str) -> Result<i64, Error> {
    match value {
        Value::I64(number) => Ok(number),
        Value::Null => Err(Error::Null { column }),
        _ => Err(wrong_kind(column)),
    }
}

enum Data {
    InRow(Vec<u8>),
    Lob(LobRoot),
}

fn part_of(row: Row, columns: &Columns, store: &mut InRowStore) -> Result<Part, Error> {
    let mut values = row.values;
    let name = text(cell(&mut values, columns.name)?, "FileName")?;
    let creation = text(cell(&mut values, columns.creation)?, "Creation")?;
    let modified = text(cell(&mut values, columns.modified)?, "Modified")?;
    let attributes = integer(cell(&mut values, columns.attributes)?, "Attributes")?;
    let data_size = integer(cell(&mut values, columns.data_size)?, "DataSize")?;
    let data = match cell(&mut values, columns.data)? {
        Value::Bytes(bytes) => Data::InRow(bytes),
        Value::LobRef(root) => Data::Lob(root),
        Value::Null => {
            return Err(Error::Null {
                column: "BinaryData",
            });
        }
        _ => return Err(wrong_kind("BinaryData")),
    };
    let part_no = integer(cell(&mut values, columns.part_no)?, "PartNo")?;
    if !name.is_ascii() {
        return Err(Error::NonAsciiName { name });
    }
    let data_size = u64::try_from(data_size).map_err(|_| Error::Row {
        column: "DataSize",
        what: "a negative size",
    })?;
    let source = match data {
        Data::InRow(bytes) => store.put(&bytes)?,
        Data::Lob(root) => Source::Lob(root),
    };
    Ok(Part {
        name,
        creation,
        modified,
        attributes,
        part_no,
        data_size,
        source,
    })
}

fn note_roots(
    roots: &mut HashMap<RecordId, usize>,
    parts: &[Part],
    part: &Part,
) -> Result<u64, Error> {
    let Source::Lob(root) = &part.source else {
        return Ok(0);
    };
    let index = parts.len();
    let mut added = 0_u64;
    let mut note = |record: RecordId| -> Result<(), Error> {
        match roots.entry(record) {
            Entry::Vacant(slot) => {
                slot.insert(index);
                added += 1;
            }
            Entry::Occupied(first) if *first.get() == index => {}
            Entry::Occupied(first) => {
                let other = parts.get(*first.get());
                return Err(Error::SharedValue {
                    file: part.name.clone(),
                    part_no: part.part_no,
                    other_file: other.map_or_else(String::new, |other| other.name.clone()),
                    other_part_no: other.map_or(0, |other| other.part_no),
                });
            }
        }
        Ok(())
    };
    match root {
        LobRoot::TextPointer { record, .. } => note(*record)?,
        LobRoot::Inline { links, .. } => {
            for link in links {
                note(link.record)?;
            }
        }
    }
    Ok(added)
}

fn json_digits(number: i64) -> u64 {
    let digits = u64::from(
        number
            .unsigned_abs()
            .checked_ilog10()
            .map_or(1, |log| log + 1),
    );
    digits + u64::from(number < 0)
}

fn sql_row_without_data(part: &Part) -> u64 {
    let digits = |number: u64| u64::from(number.checked_ilog10().map_or(1, |log| log + 1));
    SQL_ROW_FIXED
        + 4 * byte_count(part.name.len())
        + byte_count(part.creation.len())
        + byte_count(part.modified.len())
        + json_digits(part.attributes)
        + json_digits(part.part_no)
        + digits(part.data_size)
}

fn sql_part_limit(part: &Part) -> u64 {
    let room =
        byte_count(crate::sqlcmd::CONFIG_ROW_LIMIT).saturating_sub(sql_row_without_data(part));
    for digits in (1..=10_u64).rev() {
        let fits = 3 * (room.saturating_sub(digits) / 4);
        let (least, most) = (
            if digits == 1 {
                0
            } else {
                10_u64.pow(u32::try_from(digits - 1).unwrap_or(0))
            },
            10_u64.pow(u32::try_from(digits).unwrap_or(0)) - 1,
        );
        if fits >= least {
            return fits.min(most);
        }
    }
    0
}

fn part_limit(part: &Part, allowed: u64) -> u64 {
    sql_part_limit(part).min(allowed)
}

struct Collected {
    parts: Vec<Part>,
    held: u64,
    in_row: File,
    unreleased: u64,
}

fn collect_parts<S: PageSource>(
    reader: &mut PageReader<S>,
    config: &TableDef,
    columns: &Columns,
    staging_dir: &Path,
    options: &Options,
) -> Result<Collected, Error> {
    let mut store = InRowStore::create(staging_dir)?;
    let mut parts = Vec::new();
    let mut roots: HashMap<RecordId, usize> = HashMap::new();
    let mut held = 0_u64;
    let row_options = if options.accept_unreleased_versions {
        RowOptions::accepting_unreleased_versions()
    } else {
        RowOptions::default()
    };
    let mut rows = config.rows_with(reader, row_options)?;
    for row in &mut rows {
        let row = row.map_err(|error| match error {
            bak_read::Error::UnsupportedValue { column, .. }
                if column.eq_ignore_ascii_case("FileName") =>
            {
                Error::NameNotText
            }
            other => Error::Read(other),
        })?;
        let part = part_of(row, columns, &mut store)?;
        let limit = part_limit(&part, options.max_part_bytes);
        if let Some(size) = part.known_size().filter(|size| *size > limit) {
            return Err(Error::PartTooLarge {
                file: part.name,
                part_no: part.part_no,
                size: Some(size),
                max: limit,
            });
        }
        let noted = note_roots(&mut roots, &parts, &part)?;
        held = held
            .saturating_add(part.cost())
            .saturating_add(noted.saturating_mul(ROOT_ENTRY_BYTES));
        if held > options.max_held_bytes {
            return Err(Error::Limit(
                "the rows of dbo.Config take more memory than the sort may hold (the names, dates and numbers of every part, and where its bytes are)",
            ));
        }
        parts.push(part);
    }
    let unreleased = rows.unreleased_records();
    Ok(Collected {
        parts,
        held,
        in_row: store.finish()?,
        unreleased,
    })
}

fn check_names_apart(parts: &[Part]) -> Result<(), Error> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for part in parts {
        let key = part.name.trim_end_matches(' ');
        match seen.get(key) {
            Some(first) if *first != part.name => {
                return Err(Error::AmbiguousNames {
                    first: (*first).to_owned(),
                    second: part.name.clone(),
                });
            }
            Some(_) => {}
            None => {
                seen.insert(key, &part.name);
            }
        }
    }
    Ok(())
}

struct PartSink<'a> {
    bytes: &'a mut Vec<u8>,
    max: u64,
    exceeded: bool,
}

impl Write for PartSink<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let total = byte_count(self.bytes.len()).saturating_add(byte_count(data.len()));
        if total > self.max {
            self.exceeded = true;
            return Err(io::Error::other("the part is larger than the buffer"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn check_part_length(announced: Option<u64>, returned: u64, held: usize) -> Result<(), (u64, u64)> {
    if let Some(announced) = announced
        && announced != returned
    {
        return Err((announced, returned));
    }
    let held = byte_count(held);
    if held != returned {
        return Err((returned, held));
    }
    Ok(())
}

fn read_part<S: PageSource>(
    reader: &mut PageReader<S>,
    units: &LobUnits,
    root: &LobRoot,
    buffer: &mut Vec<u8>,
    (file, part_no): (&str, i64),
    max: u64,
) -> Result<u64, Error> {
    buffer.clear();
    let announced = root.declared_size();
    if let Some(size) = announced {
        let size = usize::try_from(size).map_err(|_| Error::Limit("a part larger than memory"))?;
        buffer
            .try_reserve_exact(size)
            .map_err(|_| Error::Limit("memory for a part"))?;
    }
    let mut sink = PartSink {
        bytes: buffer,
        max,
        exceeded: false,
    };
    let (returned, records) = match read_lob_counted(reader, units, root, &mut sink, MAX_LOB_BYTES)
    {
        Ok((Some(returned), stats)) => (returned, byte_count(stats.records)),
        Ok((None, _)) => {
            return Err(Error::Null {
                column: "BinaryData",
            });
        }
        Err(bak_read::Error::Sink(_)) if sink.exceeded => {
            return Err(Error::PartTooLarge {
                file: file.to_owned(),
                part_no,
                size: announced,
                max,
            });
        }
        Err(source) => {
            return Err(Error::Part {
                file: file.to_owned(),
                part_no,
                source,
            });
        }
    };
    check_part_length(announced, returned, sink.bytes.len()).map_err(|(expected, found)| {
        Error::PartLength {
            file: file.to_owned(),
            part_no,
            expected,
            found,
        }
    })?;
    Ok(records)
}

struct Tally {
    file: String,
    data_size: u64,
    bytes: u64,
}

impl Tally {
    fn add(&mut self, bytes: u64) -> Result<(), Error> {
        let sum = self.bytes.saturating_add(bytes);
        if sum > self.data_size {
            return Err(Error::FileSize {
                file: self.file.clone(),
                expected: self.data_size,
                found: sum,
            });
        }
        self.bytes = sum;
        Ok(())
    }

    fn end(self) -> Result<(), Error> {
        if self.bytes != self.data_size {
            return Err(Error::FileSize {
                file: self.file,
                expected: self.data_size,
                found: self.bytes,
            });
        }
        Ok(())
    }
}

pub fn stage_config<S: PageSource>(
    reader: &mut PageReader<S>,
    tables: &Tables,
    staging_dir: &Path,
    options: &Options,
) -> Result<Staged, Error> {
    let row_options = if options.accept_unreleased_versions {
        RowOptions::accepting_unreleased_versions()
    } else {
        RowOptions::default()
    };
    let (year_offset, unreleased_offset) =
        read_year_offset_with(reader, &tables.year_offset, tables.names, row_options)?;
    let columns = Columns::of(&tables.config, tables.names)?;
    let Collected {
        mut parts,
        held: held_bytes,
        mut in_row,
        unreleased,
    } = collect_parts(reader, &tables.config, &columns, staging_dir, options)?;
    parts.sort_unstable_by(|a, b| compare_names(&a.name, &b.name).then(a.part_no.cmp(&b.part_no)));
    check_names_apart(&parts)?;

    let mut builder = Builder::new_in(year_offset, SPOOL_LIMITS, staging_dir)?;
    let mut summary = Summary {
        year_offset,
        files: 0,
        parts: 0,
        bytes: 0,
        largest_part: 0,
        in_row_parts: 0,
        lob_parts: 0,
        held_bytes,
        lob_records: 0,
        unreleased_rows: unreleased.saturating_add(unreleased_offset),
    };
    let mut buffer: Vec<u8> = Vec::new();
    let mut units: Option<LobUnits> = None;
    let mut tally: Option<Tally> = None;
    for part in parts {
        let limit = part_limit(&part, options.max_part_bytes);
        let Part {
            name,
            creation,
            modified,
            attributes,
            part_no,
            data_size,
            source,
        } = part;
        if tally.as_ref().is_some_and(|open| open.file != name)
            && let Some(done) = tally.take()
        {
            done.end()?;
        }
        let data: &[u8] = match &source {
            Source::InRow { offset, len } => {
                summary.in_row_parts += 1;
                read_in_row(&mut in_row, *offset, *len, &mut buffer)?;
                &buffer
            }
            Source::Lob(root) => {
                summary.lob_parts += 1;
                if units.is_none() {
                    units = Some(LobUnits::load(reader, &tables.config)?);
                }
                let loaded = units
                    .as_ref()
                    .ok_or(Error::Read(bak_read::Error::Inconsistent(
                        "the large-object units were not loaded",
                    )))?;
                let records =
                    read_part(reader, loaded, root, &mut buffer, (&name, part_no), limit)?;
                summary.lob_records = summary.lob_records.saturating_add(records);
                if summary.lob_records > options.max_lob_records {
                    return Err(Error::LobRecords {
                        limit: options.max_lob_records,
                        file: name,
                        part_no,
                    });
                }
                &buffer
            }
        };
        let stored_size = byte_count(data.len());
        summary.largest_part = summary.largest_part.max(stored_size);
        tally
            .get_or_insert_with(|| Tally {
                file: name.clone(),
                data_size,
                bytes: 0,
            })
            .add(stored_size)?;
        builder
            .push(
                ConfigRow {
                    name,
                    creation,
                    modified,
                    attributes,
                    part_no,
                    data_size,
                    stored_size,
                },
                data,
            )
            .map_err(|source| Error::PartRefused {
                file: tally
                    .as_ref()
                    .map_or_else(String::new, |open| open.file.clone()),
                part_no,
                source,
            })?;
    }
    if let Some(done) = tally.take() {
        done.end()?;
    }
    let spool = builder.finish()?;
    summary.files = spool.rows().len();
    summary.parts = spool.part_count();
    summary.bytes = spool
        .rows()
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.stored_size));
    Ok(Staged { spool, summary })
}
