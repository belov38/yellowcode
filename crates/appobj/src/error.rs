#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
