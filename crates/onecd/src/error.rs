use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Memory(#[from] memory_budget::BudgetError),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt at {location}: {what}")]
    Corrupt { location: Location, what: String },
    #[error("corrupt at {location}: {what}")]
    CorruptStatic {
        location: Location,
        what: &'static str,
    },
    #[error("database is locked by {by:?}")]
    Locked { by: LockedBy },
    #[error("unsupported: {0}")]
    Unsupported(Unsupported),
    #[error("transaction too large")]
    TxnTooLarge,
    #[error("snapshot too old")]
    SnapshotTooOld,
    #[error("constraint: {0}")]
    Constraint(Constraint),
    #[error("not found: {0}")]
    NotFound(NotFound),
    #[error("{}", native_restart_text(*memory))]
    NativeRestart {
        memory: Option<memory_budget::MemoryLimit>,
    },
    #[error("cancelled")]
    Cancelled,
}

const fn native_restart_text(memory: Option<memory_budget::MemoryLimit>) -> &'static str {
    match memory {
        Some(_) => "MEMORY_LIMIT: native: restart the application",
        None => "native ICU allocation failed: restart the application",
    }
}

impl Error {
    pub fn corrupt(location: Location, what: impl Into<String>) -> Error {
        Error::Corrupt {
            location,
            what: what.into(),
        }
    }

    pub(crate) fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::Io(_)
                | Error::SnapshotTooOld
                | Error::Memory(_)
                | Error::NativeRestart { .. }
                | Error::Cancelled
        )
    }

    pub(crate) fn duplicate(&self) -> Error {
        match self {
            Error::Memory(e) => Error::Memory(*e),
            Error::Io(e) => Error::Io(std::io::Error::new(e.kind(), e.to_string())),
            Error::CorruptStatic { location, what } => Error::CorruptStatic {
                location: location.clone(),
                what,
            },
            Error::Corrupt { location, what } => Error::Corrupt {
                location: location.clone(),
                what: what.clone(),
            },
            Error::Locked { by } => Error::Locked { by: *by },
            Error::Unsupported(u) => Error::Unsupported(u.clone()),
            Error::TxnTooLarge => Error::TxnTooLarge,
            Error::SnapshotTooOld => Error::SnapshotTooOld,
            Error::Constraint(c) => Error::Constraint(c.clone()),
            Error::NotFound(n) => Error::NotFound(n.clone()),
            Error::NativeRestart { memory } => Error::NativeRestart { memory: *memory },
            Error::Cancelled => Error::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    File,
    Page(u32),
    InternalFile(u32),
    Root,
    Table(String),
    Record {
        table: String,
        row: u32,
    },
    Blob {
        table: String,
        block: u32,
    },
    Index {
        table: String,
        index: String,
        page: Option<u32>,
    },
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::File => write!(f, "file"),
            Location::Page(no) => write!(f, "page {no}"),
            Location::InternalFile(no) => write!(f, "internal file {no}"),
            Location::Root => write!(f, "root"),
            Location::Table(table) => write!(f, "table {table}"),
            Location::Record { table, row } => write!(f, "record {table}#{row}"),
            Location::Blob { table, block } => write!(f, "blob {table} block {block}"),
            Location::Index {
                table,
                index,
                page: None,
            } => write!(f, "index {table}.{index}"),
            Location::Index {
                table,
                index,
                page: Some(page),
            } => {
                write!(f, "index {table}.{index} page {page}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedBy {
    Ours,
    OneC,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    FormatVersion([u8; 4]),
    WriteLegacyFormat,
    NetworkFs,
    ReadOnly,
    NoCollator,
    ManagedLocking,
    Recordlock {
        table: String,
    },
    CsStringIndex {
        table: String,
        index: String,
    },
    FieldType {
        table: String,
        field: String,
        ty: &'static str,
    },
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::FormatVersion(v) => {
                write!(f, "format version {}.{}.{}.{}", v[0], v[1], v[2], v[3])
            }
            Unsupported::WriteLegacyFormat => write!(f, "writing the 8.2.14 format"),
            Unsupported::NetworkFs => write!(f, "database on a network file system"),
            Unsupported::ReadOnly => write!(f, "database is opened read-only"),
            Unsupported::NoCollator => write!(f, "no collator available"),
            Unsupported::ManagedLocking => write!(f, "managed file opening requires Locking::None"),
            Unsupported::Recordlock { table } => write!(f, "table {table} has Recordlock=1"),
            Unsupported::CsStringIndex { table, index } => {
                write!(f, "case-sensitive string index {table}.{index}")
            }
            Unsupported::FieldType { table, field, ty } => {
                write!(f, "field {table}.{field} of type {ty}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    UniqueViolation {
        table: String,
        index: String,
    },
    NullNotAllowed {
        table: String,
        field: String,
    },
    ValueDoesNotFit {
        table: String,
        field: String,
        why: String,
    },
    TypeMismatch {
        table: String,
        field: String,
    },
    WrongArity {
        table: String,
        expected: usize,
        got: usize,
    },
    RowVersionReadOnly {
        table: String,
        field: String,
    },
    BadDescr(String),
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constraint::UniqueViolation { table, index } => {
                write!(f, "unique index {table}.{index} violated")
            }
            Constraint::NullNotAllowed { table, field } => {
                write!(f, "field {table}.{field} does not allow NULL")
            }
            Constraint::ValueDoesNotFit { table, field, why } => {
                write!(f, "value does not fit field {table}.{field}: {why}")
            }
            Constraint::TypeMismatch { table, field } => {
                write!(f, "wrong value type for field {table}.{field}")
            }
            Constraint::WrongArity {
                table,
                expected,
                got,
            } => {
                write!(f, "table {table} has {expected} fields, row has {got}")
            }
            Constraint::RowVersionReadOnly { table, field } => {
                write!(f, "row version field {table}.{field} is read-only")
            }
            Constraint::BadDescr(why) => write!(f, "bad table description: {why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFound {
    Table(String),
    Index(String),
    Field(String),
    Row { table: String, row: u32 },
}

impl fmt::Display for NotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotFound::Table(name) => write!(f, "table {name}"),
            NotFound::Index(name) => write!(f, "index {name}"),
            NotFound::Field(name) => write!(f, "field {name}"),
            NotFound::Row { table, row } => write!(f, "row {table}#{row}"),
        }
    }
}

pub(crate) const NAME_BYTES: usize = 256;

pub(crate) fn brief(name: &str) -> &str {
    let mut end = name.len().min(NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or_default()
}
