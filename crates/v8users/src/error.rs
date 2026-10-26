#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DATA blob is empty")]
    Empty,
    #[error("DATA key length is 0")]
    EmptyKey,
    #[error("DATA key length {key} is longer than the rest of the blob ({have} bytes)")]
    KeyTooLong { key: usize, have: usize },
    #[error("DATA key length {0} is not in 1..=255")]
    BadKeyLength(usize),
    #[error("DATA payload has no UTF-8 BOM after XOR")]
    NoBom,
    #[error("DATA payload is not UTF-8")]
    NotUtf8,
    #[error("DATA text: {0}")]
    Text(#[from] brace::Error),
    #[error("DATA text is not a list")]
    NotList,
    #[error("DATA exceeds the size or nesting limit")]
    Limit,
    #[error("DATA text has {0} fields outside the known groups")]
    FieldCount(usize),
    #[error("DATA field {0} is malformed")]
    BadField(usize),
    #[error("invalid or oversized user-separator configuration")]
    InvalidRebuildContext,
    #[error("CHANGED must have whole-second precision")]
    FractionalChanged,
    #[error("role set numbers are exhausted (ROLESID u32::MAX given out)")]
    RoleSetNumbersExhausted,
    #[error("{0}")]
    Source(String),
}
