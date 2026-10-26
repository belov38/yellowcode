pub mod objects;
mod routes;
use crate::process::ProcessError;
pub use routes::{MethodDef, Route, RouteError, ServiceDef, ServiceTable, TemplateDef};

#[derive(Clone, Default)]
pub struct RequestData {
    pub method: String,
    pub path: String,
    pub base_url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl std::fmt::Debug for RequestData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RequestData(<redacted>)")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseData {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Route(#[from] RouteError),
}
impl ServiceError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Process(e) => e.status(),
            Self::Route(e) => e.status(),
        }
    }
}
