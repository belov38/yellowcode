pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Memory(#[from] memory_budget::BudgetError),
    #[error("{0}")]
    Inflate(#[from] zraw::ManagedInflateError),
    #[error("inconsistent infobase: {0}")]
    InconsistentStatic(&'static str),
    #[error(transparent)]
    Dt(#[from] dtfile::Error),
    #[error(transparent)]
    Engine(#[from] onecd::Error),
    #[error(transparent)]
    Schema(#[from] ibschema::Error),
    #[error(transparent)]
    Config(#[from] mdconf::Error),
    #[error("configuration table {table}: {source}")]
    ConfigTable {
        table: String,
        source: mdconf::Error,
    },
    #[error("inconsistent infobase: {0}")]
    Inconsistent(String),
    #[error("schema built from the configuration differs from DBSCHEMA: {}", .0.join(", "))]
    SchemaMismatch(Vec<String>),
    #[error(transparent)]
    Store(#[from] ibschema::store::StoreError),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("unsupported: {0}")]
    UnsupportedStatic(&'static str),
}
