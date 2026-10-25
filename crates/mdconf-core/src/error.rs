#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Container(#[from] v8cont::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("entry {entry}: brace text at {offset}: {kind:?}")]
    Brace {
        entry: String,
        offset: usize,
        kind: brace::ErrorKind,
    },
    #[error("entry {entry}: {source}")]
    Deflate { entry: String, source: zraw::Error },
    #[error("missing entry {0}")]
    MissingEntry(String),
    #[error("entry {entry}: {what}")]
    Format { entry: String, what: String },
    #[error("XML {path:?}: {what}")]
    Xml {
        path: std::path::PathBuf,
        what: String,
    },
    #[error(transparent)]
    Schema(#[from] mdschema::SchemaError),
    #[error(transparent)]
    SchemaResource(#[from] mdschema::ResourceError),
    #[error("{0}")]
    Memory(memory_budget::MemoryLimit),
    #[error("not loaded by the query load profile")]
    ProfileQuery,
    #[error("{object}: {path}: {what}")]
    Build {
        object: String,
        path: String,
        what: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<md_xmlout::Error> for Error {
    fn from(e: md_xmlout::Error) -> Error {
        match e {
            md_xmlout::Error::Io(e) => Error::Io(e),
            md_xmlout::Error::Format { entry, what } => Error::Format { entry, what },
        }
    }
}
