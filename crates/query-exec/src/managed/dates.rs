use crate::eval::dates::{self, DateDiffError};
use bsl_prim::BslDate;
use memory_budget::{BudgetError, MemoryBudget};
use query_ir::{
    fold,
    managed_value::{ManagedValue, NumberValueError, ScalarValue, ValueRef, row::ScopedRow},
};
use query_syntax::{Func, Kw};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateEvalError {
    Memory(BudgetError),
    Argument(&'static str),
    FieldOverflow,
}
impl From<BudgetError> for DateEvalError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl std::fmt::Display for DateEvalError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(e) => e.fmt(out),
            Self::Argument(e) => out.write_str(e),
            Self::FieldOverflow => out.write_str("DBERROR_SQLFIELDOVERFLOW 'DATEDIFF'"),
        }
    }
}
impl std::error::Error for DateEvalError {}

const UNITS: [Kw; 10] = [
    Kw::Second,
    Kw::Minute,
    Kw::Hour,
    Kw::Day,
    Kw::Week,
    Kw::TenDays,
    Kw::Month,
    Kw::Quarter,
    Kw::HalfYear,
    Kw::Year,
];
fn same_upper(text: &[u16], spelling: &str) -> bool {
    char::decode_utf16(text.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .flat_map(char::to_uppercase)
        .eq(spelling.chars().flat_map(char::to_uppercase))
}
pub fn unit_of(value: ValueRef<'_>) -> Option<Kw> {
    let ValueRef::String(text) = value else {
        return None;
    };
    UNITS
        .iter()
        .copied()
        .find(|kw| same_upper(text, kw.ru()) || same_upper(text, kw.en()))
}
fn arg(args: &[ManagedValue], index: usize) -> Result<ValueRef<'_>, DateEvalError> {
    args.get(index)
        .map(ManagedValue::view)
        .ok_or(DateEvalError::Argument("too few arguments"))
}
fn date(args: &[ManagedValue], index: usize) -> Result<BslDate, DateEvalError> {
    match arg(args, index)? {
        ValueRef::Date(d) => Ok(d),
        _ => Err(DateEvalError::Argument("a date argument is expected")),
    }
}
fn unit(args: &[ManagedValue], index: usize) -> Result<Kw, DateEvalError> {
    unit_of(arg(args, index)?).ok_or(DateEvalError::Argument("a period unit is expected"))
}
fn integer(args: &[ManagedValue], index: usize) -> Result<i64, DateEvalError> {
    match arg(args, index)? {
        ValueRef::Number(n) => Ok(n.trunc_to_i64_saturating()),
        _ => Err(DateEvalError::Argument("a number argument is expected")),
    }
}
fn number(value: i64, budget: &MemoryBudget) -> Result<ManagedValue, DateEvalError> {
    let mut magnitude = value.unsigned_abs();
    let mut digits = [0; 20];
    let mut start = digits.len();
    loop {
        start = start.checked_sub(1).ok_or(BudgetError::Overflow)?;
        *digits.get_mut(start).ok_or(BudgetError::Overflow)? = (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    let result = ManagedValue::try_number_from_decimal_digits(
        value < 0,
        digits.get(start..).ok_or(BudgetError::Overflow)?,
        0,
        budget,
    )
    .map_err(|error| match error {
        NumberValueError::Budget(e) => DateEvalError::Memory(e),
        NumberValueError::DivByZero => DateEvalError::Argument("invalid date field"),
    })?;
    result.ok_or(DateEvalError::Memory(BudgetError::Overflow))
}
pub fn try_call(f: Func, source: &ScopedRow) -> Result<ManagedValue, DateEvalError> {
    let args = source.values();
    if args.iter().any(|v| matches!(v.view(), ValueRef::Null)) {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    let result =
        match f {
            Func::DateAdd => dates::add(date(args, 0)?, unit(args, 1)?, integer(args, 2)?),
            Func::DateDiff => {
                let value = dates::diff_value(date(args, 0)?, date(args, 1)?, unit(args, 2)?)
                    .map_err(|e| match e {
                        DateDiffError::Unsupported => {
                            DateEvalError::Argument("unit is not supported by РАЗНОСТЬДАТ")
                        }
                        DateDiffError::FieldOverflow => DateEvalError::FieldOverflow,
                    })?;
                return number(value, source.budget());
            }
            Func::BeginOfPeriod => fold::begin_of_period(date(args, 0)?, unit(args, 1)?),
            Func::EndOfPeriod => dates::end_of_period(date(args, 0)?, unit(args, 1)?),
            _ => {
                let d = date(args, 0)?;
                let p = d.parts();
                let field = match f {
                    Func::Year => i64::from(p.year),
                    Func::Quarter => i64::from((p.month - 1) / 3 + 1),
                    Func::Month => i64::from(p.month),
                    Func::DayOfYear => i64::from(d.day_of_year()),
                    Func::Day => i64::from(p.day),
                    Func::Hour => i64::from(p.hour),
                    Func::Minute => i64::from(p.minute),
                    Func::Second => i64::from(p.second),
                    Func::WeekDay => i64::from(d.day_of_week()),
                    Func::Week => {
                        let jan1 = i64::from(d.day_of_year()) - 1;
                        let dow = (i64::from(d.day_of_week()) - 1 - jan1).rem_euclid(7);
                        (jan1 + dow) / 7 + 1
                    }
                    _ => return Err(DateEvalError::Argument("not a date function")),
                };
                return number(field, source.budget());
            }
        };
    Ok(ManagedValue::scalar(ScalarValue::Date(result)))
}
