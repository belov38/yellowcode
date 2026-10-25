#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a v8 container")]
    NotContainer,
    #[error("corrupt container at 0x{offset:x}: {what}")]
    Corrupt { offset: u64, what: String },
    #[error("{what} {size} exceeds the limit {limit}")]
    TooLarge {
        what: &'static str,
        size: u64,
        limit: u64,
    },
    #[error("entry {entry}: {source}")]
    Deflate { entry: String, source: zraw::Error },
    #[error("layout does not fit: {0}")]
    LayoutMismatch(String),
    #[error("manifest {path}: {what}")]
    Manifest {
        path: std::path::PathBuf,
        what: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
