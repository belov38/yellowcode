use memory_budget::BudgetError;
use query_ir::{
    DateQ, NumQ, Other, RefAtom, StrQ,
    managed_value::{ManagedValue, ScalarValue, ValueRef, row::ScopedRow},
    schema::atoms::TypeAtom,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeEvalError {
    Memory(BudgetError),
    Column,
    UnknownReference,
    Argument,
}
impl From<BudgetError> for TypeEvalError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl std::fmt::Display for TypeEvalError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(out),
            Self::Column => out.write_str("invalid type-function column"),
            Self::UnknownReference => {
                out.write_str("ТИПЗНАЧЕНИЯ of a reference to an unknown table")
            }
            Self::Argument => out.write_str("УНИКАЛЬНЫЙИДЕНТИФИКАТОР takes a reference"),
        }
    }
}
impl std::error::Error for TypeEvalError {}

pub fn try_type_of(
    input: &ScopedRow,
    index: usize,
    refs: &[(u32, RefAtom)],
) -> Result<ManagedValue, TypeEvalError> {
    let value = input
        .values()
        .get(index)
        .ok_or(TypeEvalError::Column)?
        .view();
    let atom = match value {
        ValueRef::Null => Some(TypeAtom::Null),
        ValueRef::Undefined => None,
        ValueRef::Bool(_) => Some(TypeAtom::Boolean),
        ValueRef::Number(_) => Some(TypeAtom::Number(NumQ {
            digits: 0,
            scale: 0,
            nonneg: false,
        })),
        ValueRef::String(_) => Some(TypeAtom::String(StrQ {
            len: 0,
            fixed: false,
        })),
        ValueRef::Date(_) => Some(TypeAtom::Date(DateQ::DateTime)),
        ValueRef::Uuid(_) => Some(TypeAtom::Other(Other::Uuid)),
        ValueRef::Ref { table, .. } => {
            let index = refs
                .binary_search_by_key(&table, |(number, _)| *number)
                .map_err(|_| TypeEvalError::UnknownReference)?;
            let (_, atom) = refs.get(index).ok_or(TypeEvalError::UnknownReference)?;
            Some(TypeAtom::Reference(*atom))
        }
        ValueRef::Type(_) => Some(TypeAtom::Other(Other::Type)),
        ValueRef::Binary(_) => Some(TypeAtom::Other(Other::Binary)),
        ValueRef::Storage(_) => Some(TypeAtom::Other(Other::Storage)),
        ValueRef::SysEnum(ty, _) => Some(TypeAtom::Other(Other::SysEnum(ty))),
        ValueRef::Nested(_) => Some(TypeAtom::Other(Other::QueryResult)),
    };
    Ok(ManagedValue::try_type_atom(atom, input.budget())?)
}

pub fn try_reference_uuid(input: &ScopedRow, index: usize) -> Result<ManagedValue, TypeEvalError> {
    let value = input
        .values()
        .get(index)
        .ok_or(TypeEvalError::Column)?
        .view();
    Ok(ManagedValue::scalar(match value {
        ValueRef::Null => ScalarValue::Null,
        ValueRef::Ref { id, .. } => ScalarValue::Uuid(*query_ir::uuid_of(id).as_bytes()),
        _ => return Err(TypeEvalError::Argument),
    }))
}
