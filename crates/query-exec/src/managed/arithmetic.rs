use super::cast::integer_fits;
use bsl_prim::{Number, RoundMode};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};
use query_ir::{
    Dec,
    managed_value::{
        ManagedValue, NumberBinaryOp, NumberValueError, ScalarValue, ValueRef, row::ScopedRow,
    },
};
use query_syntax::ast::BinOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    Memory(BudgetError),
    CantFit,
    FieldOverflow,
    DivisionByZero,
    NumberLimit,
    Column { index: usize, columns: usize },
    Argument(&'static str),
}
impl From<BudgetError> for ArithmeticError {
    fn from(value: BudgetError) -> Self {
        Self::Memory(value)
    }
}
impl From<NumberValueError> for ArithmeticError {
    fn from(value: NumberValueError) -> Self {
        match value {
            NumberValueError::Budget(e) => Self::Memory(e),
            NumberValueError::DivByZero => Self::DivisionByZero,
        }
    }
}
impl std::fmt::Display for ArithmeticError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(out),
            Self::CantFit => out.write_str("DBERROR_FIELDVALUECANTFIT"),
            Self::FieldOverflow => out.write_str("DBERROR_SQLFIELDOVERFLOW"),
            Self::DivisionByZero => {
                out.write_str("DBERROR_SQLDIVISIONBYZERO NUMERROR_DIVIDEBYZERO")
            }
            Self::NumberLimit => out.write_str("number limb limit exceeded"),
            Self::Column { index, columns } => {
                write!(out, "column {index} outside {columns} columns")
            }
            Self::Argument(text) => out.write_str(text),
        }
    }
}
impl std::error::Error for ArithmeticError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Trunc,
    Round { digits: i64, dec: Dec },
    RoundTo { dec: Dec },
}

pub fn try_unary(
    op: UnaryOp,
    source: &ScopedRow,
    index: usize,
    max_number_limbs: u64,
) -> Result<ManagedValue, ArithmeticError> {
    let value = source.values().get(index).ok_or(ArithmeticError::Column {
        index,
        columns: source.values().len(),
    })?;
    if matches!(value.view(), ValueRef::Null) {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    let n = num(value)?;
    if op != UnaryOp::Neg {
        guard(n, max_number_limbs)?;
    }
    let (digits, dec) = match op {
        UnaryOp::Neg => return Ok(ManagedValue::try_number_neg(n, source.budget())?),
        UnaryOp::Trunc => return Ok(ManagedValue::try_number_trunc(n, source.budget())?),
        UnaryOp::RoundTo { dec } if !dec.is_known() => return Ok(value.clone()),
        UnaryOp::RoundTo { dec } => (i64::from(dec.s), dec),
        UnaryOp::Round { digits, dec } => (digits, dec),
    };
    let result =
        ManagedValue::try_number_round(n, digits, RoundMode::HalfAwayFromZero, source.budget())?;
    if dec.is_known() && !integer_fits(num(&result)?, usize::from(dec.int())) {
        return Err(ArithmeticError::FieldOverflow);
    }
    Ok(result)
}

fn guard(n: &Number, limit: u64) -> Result<(), ArithmeticError> {
    if n.size_hint_limbs() > limit {
        Err(ArithmeticError::NumberLimit)
    } else {
        Ok(())
    }
}
fn num(value: &ManagedValue) -> Result<&Number, ArithmeticError> {
    match value.view() {
        ValueRef::Number(n) => Ok(n),
        _ => Err(ArithmeticError::Argument("a number is expected")),
    }
}
fn numeric(
    op: BinOp,
    a: &Number,
    da: Dec,
    b: &Number,
    db: Dec,
    limit: u64,
    budget: &MemoryBudget,
) -> Result<(ManagedValue, Dec), ArithmeticError> {
    let (operation, out, error) = match op {
        BinOp::Add => (
            NumberBinaryOp::Add,
            query_ir::lower::dec_add(da, db),
            ArithmeticError::CantFit,
        ),
        BinOp::Sub => (
            NumberBinaryOp::Sub,
            query_ir::lower::dec_add(da, db),
            ArithmeticError::CantFit,
        ),
        BinOp::Mul => (
            NumberBinaryOp::Mul,
            query_ir::lower::dec_mul(da, db),
            ArithmeticError::FieldOverflow,
        ),
        BinOp::Div => (
            NumberBinaryOp::Div,
            query_ir::lower::dec_div(da, db),
            ArithmeticError::CantFit,
        ),
        _ => {
            return Err(ArithmeticError::Argument(
                "a logical operator in an arithmetic chain",
            ));
        }
    };
    if matches!(op, BinOp::Mul | BinOp::Div) {
        guard(a, limit)?;
        guard(b, limit)?;
    }
    let raw = ManagedValue::try_number_binary(operation, a, b, budget)?;
    if !da.is_known() || !db.is_known() {
        if matches!(op, BinOp::Mul | BinOp::Div) {
            guard(num(&raw)?, limit)?;
        }
        return Ok((raw, Dec::UNKNOWN));
    }
    if op == BinOp::Mul
        && (!integer_fits(a, usize::from(38u8.saturating_sub(out.s)))
            || !integer_fits(b, usize::from(38u8.saturating_sub(out.s))))
    {
        return Err(error);
    }
    if op == BinOp::Div {
        guard(num(&raw)?, limit)?;
    }
    let rounded = ManagedValue::try_number_round(
        num(&raw)?,
        i64::from(out.s),
        RoundMode::HalfAwayFromZero,
        budget,
    )?;
    if !integer_fits(num(&rounded)?, usize::from(out.int())) {
        return Err(error);
    }
    Ok((rounded, out))
}

pub fn try_step(
    op: BinOp,
    source: &ScopedRow,
    left: usize,
    dl: Dec,
    right: usize,
    dr: Dec,
    max_number_limbs: u64,
) -> Result<(ManagedValue, Dec), ArithmeticError> {
    let arg = |index| {
        source.values().get(index).ok_or(ArithmeticError::Column {
            index,
            columns: source.values().len(),
        })
    };
    let l = arg(left)?.view();
    let r = arg(right)?.view();
    if matches!(l, ValueRef::Null | ValueRef::Undefined)
        || matches!(r, ValueRef::Null | ValueRef::Undefined)
    {
        return Ok((ManagedValue::scalar(ScalarValue::Null), Dec::UNKNOWN));
    }
    match (l, r) {
        (ValueRef::Number(a), ValueRef::Number(b)) => {
            numeric(op, a, dl, b, dr, max_number_limbs, source.budget())
        }
        (ValueRef::String(a), ValueRef::String(b)) if op == BinOp::Add => {
            let capacity = a.len().checked_add(b.len()).ok_or(BudgetError::Overflow)?;
            let mut text =
                BudgetedVec::try_with_capacity(source.budget(), Category::Execution, capacity)?;
            text.try_extend_from_slice(a)?;
            text.try_extend_from_slice(b)?;
            Ok((
                ManagedValue::try_utf16(text.as_slice(), source.budget())?,
                Dec::UNKNOWN,
            ))
        }
        _ => Err(ArithmeticError::Argument(
            "arithmetic operands have no common type",
        )),
    }
}
