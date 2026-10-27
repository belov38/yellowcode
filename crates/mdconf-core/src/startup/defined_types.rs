use super::ManagedObjects;
use crate::{MdKind, TypeDescription, TypeRef, Value};
use brace::Guid;
use memory_budget::BudgetError;

pub(super) trait DefinedTypeLookup {
    fn defined_type(&self, owner: Option<Guid>, name: &str) -> Option<&TypeDescription>;
}
impl DefinedTypeLookup for ManagedObjects {
    fn defined_type(&self, owner: Option<Guid>, name: &str) -> Option<&TypeDescription> {
        fn description(object: &crate::MdObject) -> Option<&TypeDescription> {
            match object.prop("Type") {
                Some(Value::Type(value)) if object.kind == MdKind::DefinedType => Some(value),
                _ => None,
            }
        }
        owner
            .and_then(|id| self.object(id))
            .and_then(description)
            .or_else(|| {
                self.as_slice()
                    .iter()
                    .rev()
                    .filter(|o| o.name == name)
                    .find_map(description)
            })
    }
}

pub(super) struct DefinedType {
    pub id: Guid,
    pub name: String,
    pub description: Option<TypeDescription>,
}
impl DefinedType {
    pub(super) fn retained_bytes(&self) -> Result<u64, BudgetError> {
        let mut bytes = self.name.capacity();
        if let Some(description) = &self.description {
            bytes = description
                .types
                .capacity()
                .checked_mul(std::mem::size_of::<TypeRef>())
                .and_then(|capacity| bytes.checked_add(capacity))
                .ok_or(BudgetError::Overflow)?;
            for reference in &description.types {
                let uri = match &reference.name.namespace {
                    crate::TypeNamespace::Uri(uri) => uri.capacity(),
                    crate::TypeNamespace::Fixed(_) => 0,
                };
                bytes = bytes
                    .checked_add(reference.name.local.capacity())
                    .and_then(|bytes| bytes.checked_add(uri))
                    .ok_or(BudgetError::Overflow)?;
            }
        }
        u64::try_from(bytes).map_err(|_| BudgetError::Overflow)
    }
}
pub(super) struct Prefix<'a>(pub &'a [DefinedType]);
impl DefinedTypeLookup for Prefix<'_> {
    fn defined_type(&self, owner: Option<Guid>, name: &str) -> Option<&TypeDescription> {
        owner
            .and_then(|id| self.0.iter().find(|row| row.id == id))
            .and_then(|row| row.description.as_ref())
            .or_else(|| {
                self.0
                    .iter()
                    .rev()
                    .filter(|row| row.name == name)
                    .find_map(|row| row.description.as_ref())
            })
    }
}
