use super::cast::integer_fits;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use query_ir::{
    QType,
    managed_value::{ManagedValue, NumberValueError, ScalarValue, ValueRef, row::ScopedRow},
};
use query_syntax::Func;
use std::fmt::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathError {
    Memory(BudgetError),
    NumberLimit,
    Argument(&'static str),
    NegativeSqrt,
    LogarithmicError,
    FieldOverflow,
}
impl From<BudgetError> for MathError {
    fn from(value: BudgetError) -> Self {
        Self::Memory(value)
    }
}
impl From<NumberValueError> for MathError {
    fn from(value: NumberValueError) -> Self {
        match value {
            NumberValueError::Budget(e) => Self::Memory(e),
            NumberValueError::DivByZero => Self::FieldOverflow,
        }
    }
}
impl fmt::Display for MathError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(out),
            Self::NumberLimit => out.write_str("number limb limit exceeded"),
            Self::Argument(text) => out.write_str(text),
            Self::NegativeSqrt => out.write_str("SELECT_ERROR_NEGATIVESQRT"),
            Self::LogarithmicError => out.write_str("SELECT_ERROR_LOGARITHMICERROR"),
            Self::FieldOverflow => out.write_str("DBERROR_SQLFIELDOVERFLOW"),
        }
    }
}
impl std::error::Error for MathError {}
fn finish_fmt(result: fmt::Result, text: &BudgetedText) -> Result<(), MathError> {
    result.map_err(|_| MathError::Memory(text.error().copied().unwrap_or(BudgetError::Overflow)))
}
fn argument(source: &ScopedRow, index: usize, limit: u64) -> Result<f64, MathError> {
    let Some(ValueRef::Number(n)) = source.values().get(index).map(ManagedValue::view) else {
        return Err(MathError::Argument("a math function takes numbers"));
    };
    if n.size_hint_limbs() > limit {
        return Err(MathError::NumberLimit);
    }
    let mut text = BudgetedText::new(source.budget(), Category::Execution, u64::MAX)?;
    finish_fmt(n.write_xml(&mut text), &text)?;
    Ok(text.as_str().parse::<f64>().unwrap_or(f64::NAN))
}
struct FixedText {
    bytes: [u8; 832],
    len: usize,
}
impl fmt::Write for FixedText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
fn fifteen(value: f64, budget: &MemoryBudget) -> Result<ManagedValue, MathError> {
    if value == 0.0 {
        return ManagedValue::try_number_from_decimal_digits(false, &[0], 0, budget)?
            .ok_or(MathError::FieldOverflow);
    }
    let mut text = FixedText {
        bytes: [0; 832],
        len: 0,
    };
    write!(text, "{:.800e}", value.abs()).map_err(|_| BudgetError::Overflow)?;
    let text = std::str::from_utf8(text.bytes.get(..text.len).ok_or(BudgetError::Overflow)?)
        .map_err(|_| BudgetError::Overflow)?;
    let (mantissa, exponent) = text.split_once('e').ok_or(BudgetError::Overflow)?;
    let mut exponent = exponent.parse::<i32>().map_err(|_| BudgetError::Overflow)?;
    let mut digits = [0u8; 340];
    let mut mantissa = mantissa.bytes().filter(u8::is_ascii_digit);
    for digit in digits.get_mut(..15).ok_or(BudgetError::Overflow)? {
        *digit = mantissa.next().ok_or(BudgetError::Overflow)? - b'0';
    }
    if mantissa.next().ok_or(BudgetError::Overflow)? >= b'5' {
        let mut carry = true;
        for digit in digits
            .get_mut(..15)
            .ok_or(BudgetError::Overflow)?
            .iter_mut()
            .rev()
        {
            if *digit == 9 {
                *digit = 0;
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            *digits.first_mut().ok_or(BudgetError::Overflow)? = 1;
            exponent = exponent.checked_add(1).ok_or(BudgetError::Overflow)?;
        }
    }
    let point = exponent.checked_add(1).ok_or(BudgetError::Overflow)?;
    let (len, scale) = if point >= 15 {
        (
            usize::try_from(point).map_err(|_| BudgetError::Overflow)?,
            0,
        )
    } else {
        (
            15,
            u32::try_from(15 - point).map_err(|_| BudgetError::Overflow)?,
        )
    };
    ManagedValue::try_number_from_decimal_digits(
        value < 0.0,
        digits.get(..len).ok_or(BudgetError::Overflow)?,
        scale,
        budget,
    )?
    .ok_or(MathError::FieldOverflow)
}
fn fixed(value: f64, scale: u8, budget: &MemoryBudget) -> Result<ManagedValue, MathError> {
    let mut text = BudgetedText::new(budget, Category::Execution, u64::MAX)?;
    finish_fmt(write!(text, "{:.*}", usize::from(scale), value), &text)?;
    let mut digits =
        BudgetedVec::try_with_capacity(budget, Category::Execution, text.as_str().len())?;
    for digit in text.as_str().bytes().filter(u8::is_ascii_digit) {
        digits.try_push(digit - b'0')?;
    }
    ManagedValue::try_number_from_decimal_digits(
        value < 0.0,
        digits.as_slice(),
        u32::from(scale),
        budget,
    )?
    .ok_or(MathError::FieldOverflow)
}
pub fn try_math(
    f: Func,
    source: &ScopedRow,
    ty: &QType,
    constant: bool,
    max_number_limbs: u64,
) -> Result<ManagedValue, MathError> {
    if source
        .values()
        .iter()
        .any(|v| matches!(v.view(), ValueRef::Null))
    {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    let x = argument(source, 0, max_number_limbs)?;
    let value = match f {
        Func::Sqrt => {
            if x < 0.0 {
                return Err(MathError::NegativeSqrt);
            }
            x.sqrt()
        }
        Func::Log | Func::Log10 => {
            if x <= 0.0 {
                return Err(MathError::LogarithmicError);
            }
            if f == Func::Log { x.ln() } else { x.log10() }
        }
        Func::Exp => x.exp(),
        Func::Sin => x.sin(),
        Func::Cos => x.cos(),
        Func::ACos => x.acos(),
        Func::ASin => x.asin(),
        Func::ATan => x.atan(),
        Func::Pow => x.powf(argument(source, 1, max_number_limbs)?),
        _ => return Err(MathError::Argument("not a math function")),
    };
    if !value.is_finite() {
        return Err(MathError::FieldOverflow);
    }
    match ty.number {
        Some(q) if !constant && q.digits > 0 => {
            let output = fixed(value, q.scale, source.budget())?;
            if let ValueRef::Number(n) = output.view()
                && !integer_fits(n, usize::from(q.digits.saturating_sub(q.scale)))
            {
                return Err(MathError::FieldOverflow);
            }
            Ok(output)
        }
        _ => fifteen(value, source.budget()),
    }
}
