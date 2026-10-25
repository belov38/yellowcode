use super::QuerySchema;
use crate::{DateQ, NumQ, Other, QType, RefAtom, StrQ};
use brace::Guid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeAtom {
    Null,
    Boolean,
    Number(NumQ),
    String(StrQ),
    Date(DateQ),
    Reference(RefAtom),
    Other(Other),
}

impl QuerySchema {
    pub fn try_visit_type_atoms<E>(
        &self,
        ty: &QType,
        mut visit: impl FnMut(Guid, TypeAtom) -> Result<(), E>,
    ) -> Result<(), E> {
        if ty.null {
            visit(self.sys.null, TypeAtom::Null)?;
        }
        if ty.boolean {
            visit(self.sys.boolean, TypeAtom::Boolean)?;
        }
        if let Some(value) = ty.number {
            visit(self.sys.number, TypeAtom::Number(value))?;
        }
        if let Some(value) = ty.string {
            visit(self.sys.string, TypeAtom::String(value))?;
        }
        if let Some(value) = ty.date {
            visit(self.sys.date, TypeAtom::Date(value))?;
        }
        for value in &ty.refs {
            visit(self.ref_info(*value).type_uuid, TypeAtom::Reference(*value))?;
        }
        for value in &ty.others {
            visit(self.other_uuid(*value), TypeAtom::Other(*value))?;
        }
        Ok(())
    }
}
