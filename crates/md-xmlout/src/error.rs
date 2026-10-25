#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("entry {entry}: {what}")]
    Format { entry: String, what: String },
}

pub type Result<T> = std::result::Result<T, Error>;
