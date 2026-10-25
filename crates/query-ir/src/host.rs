use crate::schema::{QTableId, StdRole, VtKind};
use crate::value::QValue;
use brace::Guid;
use std::any::Any;
use std::sync::Arc;

pub use crate::access::AccessPredicate;

pub trait AccessPolicy: Send + Sync {
    fn read_policy(&self, t: &SourceTable, used: &FieldSet) -> Result<Access, HostError>;
}

#[derive(Clone, Debug)]
pub enum Access {
    Deny,
    Unrestricted,
    Restricted(Vec<Arc<AccessPredicate>>),
}

pub trait SessionValues {
    fn session_param(&self, name: &str) -> Result<QValue, HostError>;
    fn separators(&self) -> Result<Vec<SeparatorFilter>, HostError>;
    fn user(&self) -> Option<&UserRef>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceTable {
    pub object: Guid,
    pub full_name: String,
    pub table: QTableId,
    pub part: TablePart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TablePart {
    Main,
    Tabular(Guid),
    Virtual(VtKind),
    Changes,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldSet {
    pub all: bool,
    pub fields: Vec<Guid>,
}

#[derive(Clone, Debug)]
pub struct HostError {
    pub message: String,
    pub cause: Option<Box<HostError>>,
    pub payload: Option<Arc<dyn Any + Send + Sync>>,
}

impl HostError {
    pub fn new(message: impl Into<String>) -> HostError {
        HostError {
            message: message.into(),
            cause: None,
            payload: None,
        }
    }

    pub fn with_cause(mut self, cause: HostError) -> HostError {
        self.cause = Some(Box::new(cause));
        self
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        let mut c = self.cause.as_deref();
        while let Some(e) = c {
            write!(f, ": {}", e.message)?;
            c = e.cause.as_deref();
        }
        Ok(())
    }
}

impl std::error::Error for HostError {}

#[derive(Clone, Debug, PartialEq)]
pub struct SeparatorFilter {
    pub common_attribute: Guid,
    pub value: QValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRef {
    pub id: [u8; 16],
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TxnId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredefinedName {
    pub table: QTableId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationInput {
    pub reference: QValue,
    pub table: Option<QTableId>,
    pub found: bool,
    pub fields: Vec<(StdRole, QValue)>,
}
