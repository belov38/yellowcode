pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Memory(#[from] memory_budget::BudgetError),
    #[error("{0}")]
    Brace(#[from] brace::ManagedParseError),
    #[error("bad {what}: {why}")]
    ParseStatic {
        what: &'static str,
        why: &'static str,
    },
    #[error("bad {what} at {at}: {why}")]
    Parse {
        what: &'static str,
        at: String,
        why: String,
    },
    #[error("value does not fit {table}.{column}: {why}")]
    ValueDoesNotFit {
        table: String,
        column: String,
        why: String,
    },
    #[error("bad stored value of {table}.{field}: {why}")]
    BadValue {
        table: String,
        field: String,
        why: String,
    },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("unsupported object {0}")]
    UnsupportedObject(String),
}

pub const NAME_BYTES: usize = 256;

pub fn diagnostic_name(name: &str) -> String {
    let mut end = name.len().min(NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or_default().to_owned()
}
