use crate::CellKind;
use crate::cell::column_kind;
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, LimitReason, MemoryBudget,
    MemoryLimit, TextCount, TextOutput,
};
use query_ir::managed_value::{ManagedColumn as RawColumn, ManagedValue, ValueRef};
use query_ir::uuid_of;
use query_syntax::Lang;
use query_testhost::{TestBase, measure_managed_repr, write_managed_repr};
use query_testkit::repr::write_type_repr_budgeted;
use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::fmt::Write;

#[derive(Debug)]
pub struct ManagedCell {
    kind: CellKind,
    text: Charged<String>,
    reference: Option<ManagedReference>,
}
#[derive(Debug)]
struct ManagedReference {
    kind: Charged<String>,
    id: Charged<String>,
}
#[derive(Debug)]
pub struct ManagedColumn {
    name: Charged<String>,
    ty: Charged<String>,
    width: u32,
    kind: CellKind,
}

fn limit_content(requested: u64, limit: u64) -> Result<(), BudgetError> {
    if requested > limit {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Row,
            requested,
            reason: LimitReason::Category,
        }));
    }
    Ok(())
}
fn text(
    budget: &MemoryBudget,
    len: u64,
    write: impl FnOnce(&mut BudgetedText) -> Result<(), BudgetError>,
) -> Result<Charged<String>, BudgetError> {
    let mut output = BudgetedText::try_with_capacity(
        budget,
        Category::Result,
        len,
        usize::try_from(len).map_err(|_| BudgetError::Overflow)?,
    )?;
    write(&mut output)?;
    output.into_string()
}
fn formatting(error: std::fmt::Result, output: &impl TextOutput) -> Result<(), BudgetError> {
    error.map_err(|_| output.error().copied().unwrap_or(BudgetError::Overflow))
}
#[derive(Clone, Copy)]
enum ReferenceKind<'a> {
    Known(&'a str),
    Unknown(u32),
}
impl ReferenceKind<'_> {
    fn write(self, output: &mut impl TextOutput) -> Result<(), BudgetError> {
        match self {
            Self::Known(name) => output.try_push_str(name),
            Self::Unknown(table) => {
                let result = write!(output, "Ref#{table}");
                formatting(result, output)
            }
        }
    }
}
impl ManagedCell {
    pub fn try_new(
        value: &ManagedValue,
        base: &TestBase,
        lang: Lang,
        budget: &MemoryBudget,
        content_limit: u64,
    ) -> Result<Self, BudgetError> {
        let len = measure_managed_repr(value, base, lang, budget, content_limit)?;
        let (kind, reference) = match value.view() {
            ValueRef::Null => (CellKind::Null, None),
            ValueRef::Undefined => (CellKind::Undefined, None),
            ValueRef::Bool(_) => (CellKind::Bool, None),
            ValueRef::Number(_) => (CellKind::Number, None),
            ValueRef::String(_) => (CellKind::String, None),
            ValueRef::Date(_) => (CellKind::Date, None),
            ValueRef::Type(_) => (CellKind::Type, None),
            ValueRef::Binary(_) => (CellKind::Binary, None),
            ValueRef::Nested(_) => (CellKind::Nested, None),
            ValueRef::Uuid(_) | ValueRef::Storage(_) | ValueRef::SysEnum(..) => {
                (CellKind::Other, None)
            }
            ValueRef::Ref { table, id } => {
                let kind = base
                    .ref_xml_name(table)
                    .map_or(ReferenceKind::Unknown(table), ReferenceKind::Known);
                let mut measured = TextCount::new(Category::Row, content_limit);
                kind.write(&mut measured)?;
                let total = len
                    .checked_add(measured.len())
                    .and_then(|n| n.checked_add(36))
                    .ok_or(BudgetError::Overflow)?;
                limit_content(total, content_limit)?;
                (CellKind::Ref, Some((kind, measured.len(), id)))
            }
        };
        let text = text(budget, len, |output| {
            write_managed_repr(value, base, lang, budget, Category::Result, output)
        })?;
        let reference = reference
            .map(|(kind, len, id)| {
                let kind = self::text(budget, len, |output| kind.write(output))?;
                let id = self::text(budget, 36, |output| {
                    let result = write!(output, "{}", uuid_of(id));
                    formatting(result, output)
                })?;
                Ok::<_, BudgetError>(ManagedReference { kind, id })
            })
            .transpose()?;
        Ok(Self {
            kind,
            text,
            reference,
        })
    }
}

pub fn try_columns(
    columns: &[RawColumn],
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<ManagedColumn>, BudgetError> {
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Result, columns.len())?;
    for column in columns {
        let name_len = u64::try_from(column.name.len()).map_err(|_| BudgetError::Overflow)?;
        let name = text(budget, name_len, |output| output.try_push_str(&column.name))?;
        let mut measured = TextCount::new(Category::Result, u64::MAX);
        write_type_repr_budgeted(
            base.schema(),
            &column.ty,
            lang,
            budget,
            Category::Execution,
            &mut measured,
            true,
        )?;
        let ty = text(budget, measured.len(), |output| {
            write_type_repr_budgeted(
                base.schema(),
                &column.ty,
                lang,
                budget,
                Category::Result,
                output,
                true,
            )
        })?;
        output.try_push(ManagedColumn {
            name,
            ty,
            width: column.width,
            kind: if column.nested.is_some() {
                CellKind::Nested
            } else {
                column_kind(&column.ty)
            },
        })?;
    }
    Ok(output)
}

impl Serialize for ManagedCell {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output =
            serializer.serialize_struct("Cell", if self.reference.is_some() { 3 } else { 2 })?;
        output.serialize_field("k", &self.kind)?;
        output.serialize_field("t", self.text.as_ref())?;
        if let Some(reference) = &self.reference {
            output.serialize_field("r", reference)?;
        }
        output.end()
    }
}
impl Serialize for ManagedReference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct("RefOut", 2)?;
        output.serialize_field("kind", self.kind.as_ref())?;
        output.serialize_field("id", self.id.as_ref())?;
        output.end()
    }
}
impl Serialize for ManagedColumn {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct("Column", 4)?;
        output.serialize_field("name", self.name.as_ref())?;
        output.serialize_field("ty", self.ty.as_ref())?;
        output.serialize_field("width", &self.width)?;
        output.serialize_field("kind", &self.kind)?;
        output.end()
    }
}
