#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("malformed event log: {0}")]
    Malformed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
