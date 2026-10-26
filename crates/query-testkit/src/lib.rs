pub mod map;
pub mod model;
pub mod registers;
pub mod repr;
pub use map::*;
pub use model::rtq_like;

pub struct FixtureSecurity;

impl query_ir::host::AccessPolicy for FixtureSecurity {
    fn read_policy(
        &self,
        _: &query_ir::host::SourceTable,
        _: &query_ir::host::FieldSet,
    ) -> Result<query_ir::host::Access, query_ir::host::HostError> {
        Ok(query_ir::host::Access::Unrestricted)
    }
}

impl query_ir::host::SessionValues for FixtureSecurity {
    fn session_param(&self, name: &str) -> Result<query_ir::QValue, query_ir::host::HostError> {
        Err(query_ir::host::HostError::new(format!(
            "fixture session parameter {name} is unset"
        )))
    }
    fn separators(
        &self,
    ) -> Result<Vec<query_ir::host::SeparatorFilter>, query_ir::host::HostError> {
        Ok(Vec::new())
    }
    fn user(&self) -> Option<&query_ir::host::UserRef> {
        None
    }
}

impl FixtureSecurity {
    pub fn context(&self) -> query_ir::host::SecurityCtx<'_> {
        query_ir::host::SecurityCtx {
            access: self,
            session: self,
        }
    }
}
