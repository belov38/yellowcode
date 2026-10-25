use super::{Backing, ManagedNumber, ManagedValue};
use bsl_prim::{
    Number, RoundMode,
    number::{LimbAllocator, LimbBuffer, NumberOpError, NumberOutput},
};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
impl NumberBinaryOp {
    pub(crate) fn evaluate<A: LimbAllocator>(
        self,
        left: &Number,
        right: &Number,
        allocator: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        match self {
            Self::Add => left.try_add_with(right, allocator),
            Self::Sub => left.try_sub_with(right, allocator),
            Self::Mul => left.try_mul_with(right, allocator),
            Self::Div => left.try_div_with(right, allocator),
            Self::Rem => left.try_rem_with(right, allocator),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberValueError {
    Budget(BudgetError),
    DivByZero,
}
impl From<BudgetError> for NumberValueError {
    fn from(value: BudgetError) -> Self {
        Self::Budget(value)
    }
}
impl From<NumberOpError<BudgetError>> for NumberValueError {
    fn from(value: NumberOpError<BudgetError>) -> Self {
        match value {
            NumberOpError::Buffer(error) => Self::Budget(error),
            NumberOpError::Overflow => Self::Budget(BudgetError::Overflow),
            NumberOpError::DivByZero => Self::DivByZero,
        }
    }
}
impl std::fmt::Display for NumberValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(error) => error.fmt(f),
            Self::DivByZero => f.write_str("DIVISION_BY_ZERO"),
        }
    }
}
impl std::error::Error for NumberValueError {}

struct Factory<'a>(&'a MemoryBudget);
struct Buffer(BudgetedVec<u32>);
impl AsRef<[u32]> for Buffer {
    fn as_ref(&self) -> &[u32] {
        self.0.as_slice()
    }
}
impl AsMut<[u32]> for Buffer {
    fn as_mut(&mut self) -> &mut [u32] {
        self.0.as_mut_slice()
    }
}
impl LimbBuffer for Buffer {
    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}
impl LimbAllocator for Factory<'_> {
    type Error = BudgetError;
    type Buffer = Buffer;
    fn allocate_zeroed(&self, len: usize) -> Result<Buffer, BudgetError> {
        let mut values = BudgetedVec::try_with_capacity(self.0, Category::Execution, len)?;
        values.try_resize(len, 0)?;
        Ok(Buffer(values))
    }
}
fn finish(
    result: Result<NumberOutput<Buffer>, NumberOpError<BudgetError>>,
) -> Result<ManagedValue, NumberValueError> {
    let number = match result? {
        NumberOutput::Inline(number) => ManagedNumber::Inline(number),
        NumberOutput::Buffered { parts, limbs } => ManagedNumber::Shared(
            limbs
                .0
                .into_charged_vec()
                .map(|values| parts.into_number(values))
                .try_into_shared()?,
        ),
    };
    Ok(ManagedValue(Backing::Number(number)))
}
impl ManagedValue {
    pub fn try_number_from_decimal_digits(
        negative: bool,
        digits: &[u8],
        scale: u32,
        budget: &MemoryBudget,
    ) -> Result<Option<Self>, NumberValueError> {
        Number::try_from_decimal_digits_with(negative, digits, scale, &Factory(budget))?
            .map(|output| finish(Ok(output)))
            .transpose()
    }
    pub fn try_number_binary(
        op: NumberBinaryOp,
        left: &Number,
        right: &Number,
        budget: &MemoryBudget,
    ) -> Result<Self, NumberValueError> {
        finish(op.evaluate(left, right, &Factory(budget)))
    }
    pub fn try_number_neg(value: &Number, budget: &MemoryBudget) -> Result<Self, NumberValueError> {
        finish(value.try_neg_with(&Factory(budget)))
    }
    pub fn try_number_trunc(
        value: &Number,
        budget: &MemoryBudget,
    ) -> Result<Self, NumberValueError> {
        finish(value.try_trunc_with(&Factory(budget)))
    }
    pub fn try_number_round(
        value: &Number,
        digits: i64,
        mode: RoundMode,
        budget: &MemoryBudget,
    ) -> Result<Self, NumberValueError> {
        finish(value.try_round_with(digits, mode, &Factory(budget)))
    }
}
