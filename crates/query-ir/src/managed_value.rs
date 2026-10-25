use crate::{ColumnInfo, NestedResult, QStr, QType, QValue, RefId};
use bsl_prim::{BslDate, Number};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Shared,
};
use platform_catalog::TypeIdx;

mod arithmetic;
pub mod row;
pub use arithmetic::{NumberBinaryOp, NumberValueError};

pub type ManagedRow = BudgetedVec<ManagedValue>;
pub type SharedManagedRow = Shared<Vec<ManagedValue>>;

#[derive(Clone, Copy, Debug)]
pub enum ScalarValue {
    Null,
    Undefined,
    Bool(bool),
    Date(BslDate),
    Uuid([u8; 16]),
    Ref { table: u32, id: RefId },
    SysEnum(TypeIdx, u32),
}

#[derive(Clone, Debug)]
pub struct ManagedValue(Backing);
#[derive(Clone, Debug)]
enum Backing {
    Null,
    Undefined,
    Bool(bool),
    Number(ManagedNumber),
    String(QStr),
    Date(BslDate),
    Uuid([u8; 16]),
    Ref { table: u32, id: RefId },
    Type(Shared<QType>),
    Binary(Shared<Vec<u8>>),
    Storage(Shared<Vec<u8>>),
    SysEnum(TypeIdx, u32),
    Nested(Shared<ManagedNestedResult>),
}
#[derive(Clone, Debug)]
enum ManagedNumber {
    Inline(Number),
    Shared(Shared<Number>),
}
impl ManagedNumber {
    fn try_copy(value: &Number, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let charge = budget.try_reserve(Category::Execution, 0)?;
        let mut has_backing = false;
        let value = value
            .try_clone_with(|limbs| {
                let bytes = limbs
                    .checked_mul(std::mem::size_of::<u32>())
                    .ok_or(BudgetError::Overflow)?;
                charge.try_grow(u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?)?;
                has_backing = bytes != 0;
                Ok::<_, BudgetError>(())
            })
            .map_err(|error| match error {
                bsl_prim::number::NumberAllocError::Reserve(error) => error,
                bsl_prim::number::NumberAllocError::AllocationFailed => {
                    BudgetError::AllocationFailed
                }
                bsl_prim::number::NumberAllocError::Overflow => BudgetError::Overflow,
            })?;
        if has_backing {
            Ok(Self::Shared(Shared::try_new(value, charge)?))
        } else {
            Ok(Self::Inline(value))
        }
    }
    fn as_number(&self) -> &Number {
        match self {
            Self::Inline(value) => value,
            Self::Shared(value) => value,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ValueRef<'a> {
    Null,
    Undefined,
    Bool(bool),
    Number(&'a Number),
    String(&'a [u16]),
    Date(BslDate),
    Uuid(&'a [u8; 16]),
    Ref { table: u32, id: &'a RefId },
    Type(&'a QType),
    Binary(&'a [u8]),
    Storage(&'a [u8]),
    SysEnum(TypeIdx, u32),
    Nested(&'a ManagedNestedResult),
}

#[derive(Debug)]
pub struct ManagedColumn {
    pub name: Charged<String>,
    pub ty: Shared<QType>,
    pub width: u32,
    pub nested: Option<Shared<Vec<ManagedColumn>>>,
}
#[derive(Debug)]
pub struct ManagedNestedResult {
    pub columns: BudgetedVec<ManagedColumn>,
    pub rows: BudgetedVec<SharedManagedRow>,
}
impl ManagedValue {
    pub fn scalar(value: ScalarValue) -> Self {
        Self(match value {
            ScalarValue::Null => Backing::Null,
            ScalarValue::Undefined => Backing::Undefined,
            ScalarValue::Bool(value) => Backing::Bool(value),
            ScalarValue::Date(value) => Backing::Date(value),
            ScalarValue::Uuid(value) => Backing::Uuid(value),
            ScalarValue::Ref { table, id } => Backing::Ref { table, id },
            ScalarValue::SysEnum(ty, index) => Backing::SysEnum(ty, index),
        })
    }
    pub fn try_string(text: &str, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self(Backing::String(QStr::try_from_str_budgeted(
            text, budget,
        )?)))
    }
    pub fn try_utf16(text: &[u16], budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let mut units = BudgetedVec::try_with_capacity(budget, Category::Execution, text.len())?;
        units.try_extend_from_slice(text)?;
        Ok(Self(Backing::String(QStr::try_from_budgeted_units(units)?)))
    }
    pub fn try_binary(bytes: &[u8], budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self(Backing::Binary(copy_bytes(bytes, budget)?)))
    }
    pub fn try_storage(bytes: &[u8], budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self(Backing::Storage(copy_bytes(bytes, budget)?)))
    }
    pub fn try_type(ty: &QType, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self(Backing::Type(
            ty.try_clone_budgeted(budget)?.try_into_shared()?,
        )))
    }
    pub fn try_type_atom(
        atom: Option<crate::schema::atoms::TypeAtom>,
        budget: &MemoryBudget,
    ) -> Result<Self, BudgetError> {
        Ok(Self(Backing::Type(
            QType::try_from_atom_budgeted(atom, budget)?.try_into_shared()?,
        )))
    }
    pub fn try_copy(value: &QValue, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Self::copy_at(value, budget, 0)
    }
    fn copy_at(value: &QValue, budget: &MemoryBudget, depth: usize) -> Result<Self, BudgetError> {
        if depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        Ok(Self(match value {
            QValue::Null => Backing::Null,
            QValue::Undefined => Backing::Undefined,
            QValue::Bool(value) => Backing::Bool(*value),
            QValue::Number(value) => Backing::Number(ManagedNumber::try_copy(value, budget)?),
            QValue::String(value) => return Self::try_utf16(&value.0, budget),
            QValue::Date(value) => Backing::Date(*value),
            QValue::Uuid(value) => Backing::Uuid(*value),
            QValue::Ref { table, id } => Backing::Ref {
                table: *table,
                id: *id,
            },
            QValue::Type(value) => return Self::try_type(value, budget),
            QValue::Binary(value) => return Self::try_binary(value, budget),
            QValue::Storage(value) => return Self::try_storage(value, budget),
            QValue::SysEnum(ty, value) => Backing::SysEnum(*ty, *value),
            QValue::Nested(value) => Backing::Nested(copy_nested(value, budget, depth)?),
            QValue::PointInTime(_) | QValue::Boundary(_) => return Err(BudgetError::Overflow),
        }))
    }
    pub fn view(&self) -> ValueRef<'_> {
        match &self.0 {
            Backing::Null => ValueRef::Null,
            Backing::Undefined => ValueRef::Undefined,
            Backing::Bool(value) => ValueRef::Bool(*value),
            Backing::Number(value) => ValueRef::Number(value.as_number()),
            Backing::String(value) => ValueRef::String(&value.0),
            Backing::Date(value) => ValueRef::Date(*value),
            Backing::Uuid(value) => ValueRef::Uuid(value),
            Backing::Ref { table, id } => ValueRef::Ref { table: *table, id },
            Backing::Type(value) => ValueRef::Type(value),
            Backing::Binary(value) => ValueRef::Binary(value),
            Backing::Storage(value) => ValueRef::Storage(value),
            Backing::SysEnum(ty, value) => ValueRef::SysEnum(*ty, *value),
            Backing::Nested(value) => ValueRef::Nested(value),
        }
    }
}
fn copy_bytes(bytes: &[u8], budget: &MemoryBudget) -> Result<Shared<Vec<u8>>, BudgetError> {
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Execution, bytes.len())?;
    output.try_extend_from_slice(bytes)?;
    output.try_into_shared()
}
fn copy_columns(
    columns: &[ColumnInfo],
    budget: &MemoryBudget,
    depth: usize,
) -> Result<BudgetedVec<ManagedColumn>, BudgetError> {
    if depth >= 128 {
        return Err(BudgetError::Overflow);
    }
    let mut output = BudgetedVec::try_with_capacity(budget, Category::Execution, columns.len())?;
    for column in columns {
        let mut name = BudgetedText::new(budget, Category::Execution, u64::MAX)?;
        name.try_push_str(&column.name)?;
        let name = name.into_string()?;
        let ty = column.ty.try_clone_budgeted(budget)?.try_into_shared()?;
        let nested = column
            .nested
            .as_deref()
            .map(|columns| copy_columns(columns, budget, depth + 1)?.try_into_shared())
            .transpose()?;
        output.try_push(ManagedColumn {
            name,
            ty,
            width: column.width,
            nested,
        })?;
    }
    Ok(output)
}
fn copy_nested(
    input: &NestedResult,
    budget: &MemoryBudget,
    depth: usize,
) -> Result<Shared<ManagedNestedResult>, BudgetError> {
    let columns = copy_columns(&input.columns, budget, depth)?;
    let mut rows = BudgetedVec::try_with_capacity(budget, Category::Execution, input.rows.len())?;
    for row in &input.rows {
        let mut output = ManagedRow::try_with_capacity(budget, Category::Execution, row.len())?;
        for value in row {
            output.try_push(ManagedValue::copy_at(value, budget, depth + 1)?)?;
        }
        rows.try_push(output.try_into_shared()?)?;
    }
    let charge = budget.try_reserve(Category::Execution, 0)?;
    Shared::try_new(ManagedNestedResult { columns, rows }, charge)
}
