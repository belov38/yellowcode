#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(".dt at {offset}: {what}")]
    Dt { offset: u64, what: String },
    #[error(".dt at {offset}: read limit: {what}")]
    Limit { offset: u64, what: &'static str },
    #[error(transparent)]
    Schema(#[from] ibschema::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn again(&self) -> Error {
        use ibschema::Error as S;
        match self {
            Error::Io(e) => Error::Io(std::io::Error::new(e.kind(), e.to_string())),
            Error::Dt { offset, what } => Error::Dt {
                offset: *offset,
                what: what.clone(),
            },
            Error::Limit { offset, what } => Error::Limit {
                offset: *offset,
                what,
            },
            Error::Schema(e) => Error::Schema(match e {
                S::Memory(error) => S::Memory(*error),
                S::Brace(error) => S::Brace(error.clone()),
                S::ParseStatic { what, why } => S::ParseStatic { what, why },
                S::Parse { what, at, why } => S::Parse {
                    what,
                    at: at.clone(),
                    why: why.clone(),
                },
                S::ValueDoesNotFit { table, column, why } => S::ValueDoesNotFit {
                    table: table.clone(),
                    column: column.clone(),
                    why: why.clone(),
                },
                S::BadValue { table, field, why } => S::BadValue {
                    table: table.clone(),
                    field: field.clone(),
                    why: why.clone(),
                },
                S::Unsupported(m) => S::Unsupported(m.clone()),
                S::UnsupportedObject(m) => S::UnsupportedObject(m.clone()),
            }),
            Error::Unsupported(m) => Error::Unsupported(m.clone()),
        }
    }
}

pub(crate) fn schema_note(e: ibschema::Error, note: &str) -> ibschema::Error {
    use ibschema::Error as S;
    match e {
        error @ (S::Memory(_) | S::Brace(_) | S::ParseStatic { .. }) => error,
        S::Parse { what, at, why } => S::Parse {
            what,
            at: format!("{at} ({note})"),
            why,
        },
        S::ValueDoesNotFit { table, column, why } => S::ValueDoesNotFit {
            table,
            column,
            why: format!("{why} ({note})"),
        },
        S::BadValue { table, field, why } => S::BadValue {
            table,
            field,
            why: format!("{why} ({note})"),
        },
        S::Unsupported(m) => S::Unsupported(format!("{m} ({note})")),
        S::UnsupportedObject(m) => S::UnsupportedObject(format!("{m} ({note})")),
    }
}
