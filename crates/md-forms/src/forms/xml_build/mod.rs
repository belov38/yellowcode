mod builder;
mod choice;
mod dynamic;
mod embedded;
mod path_admission;
mod properties;
mod records;
mod spreadsheet;
mod ui;
mod values;
mod xml;

use super::FormResolution;
use brace::Guid;
use memory_budget::{MemoryBudget, MemoryLimit, Reservation};

pub trait FormBuildResolution: FormResolution {
    fn metadata_id(&self, name: &str) -> Option<Guid>;
    fn type_id(&self, name: &str) -> Option<Guid>;
    fn metadata_entries(&self, _visit: &mut dyn FnMut(Guid, &str)) {}
    fn predefined_id(&self, _name: &str) -> Option<Guid> {
        None
    }
}

impl FormBuildResolution for md_model::external::ExternalDocument {
    fn metadata_id(&self, name: &str) -> Option<Guid> {
        if self.name(self.object.id) == Some(name) {
            return Some(self.object.id);
        }
        let mut matches = self.names().filter(|(_, n)| *n == name).map(|(id, _)| id);
        let id = matches.next()?;
        matches.next().is_none().then_some(id)
    }
    fn metadata_entries(&self, visit: &mut dyn FnMut(Guid, &str)) {
        for (id, name) in self.names() {
            visit(id, name);
        }
    }
    fn type_id(&self, name: &str) -> Option<Guid> {
        let mut matches = self
            .type_names()
            .filter(|(_, n)| *n == name)
            .map(|(id, _)| id);
        let id = matches.next()?;
        matches.next().is_none().then_some(id)
    }
}

pub struct FormBuildInput<'a> {
    pub xml: &'a [u8],
    pub module: &'a str,
    pub files: &'a [(String, Vec<u8>)],
    pub owner: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum FormBuildError {
    #[error("form build cancelled")]
    Cancelled,
    #[error("form build memory: {0}")]
    Memory(#[from] MemoryLimit),
    #[error("form embedded memory: {0}")]
    Budget(#[from] memory_budget::BudgetError),
    #[error("form build limit: {0}")]
    Limit(&'static str),
    #[error("form XML: {0}")]
    Xml(String),
    #[error("form {path}: {what}")]
    Invalid { path: String, what: String },
    #[error("form resource: {0}")]
    Resource(String),
}
type Result<T> = std::result::Result<T, FormBuildError>;
fn invalid(path: impl Into<String>, what: impl Into<String>) -> FormBuildError {
    FormBuildError::Invalid {
        path: path.into(),
        what: what.into(),
    }
}

#[derive(Debug)]
pub struct BuiltForm {
    bytes: Vec<u8>,
    _native_values: std::collections::HashMap<usize, memory_budget::Charged<brace::Node>>,
    _asset_charges: Vec<Reservation>,
    _charge: Reservation,
}
impl BuiltForm {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn build_form(
    input: FormBuildInput<'_>,
    resolution: &dyn FormBuildResolution,
    memory: &MemoryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<BuiltForm> {
    builder::build(input, resolution, memory, cancelled)
}
