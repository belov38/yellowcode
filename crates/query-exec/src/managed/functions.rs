use super::{
    arithmetic::{self, ArithmeticError, UnaryOp},
    collation::ManagedCollation,
    dates::{self, DateEvalError},
    math::{self, MathError},
    strings::{self, StringEvalError},
    type_info::{self, TypeEvalError},
};
use query_ir::{
    Dec, QType,
    managed_value::{ManagedValue, ScalarValue, ValueRef, row::ScopedRow},
};
use query_syntax::Func;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionError {
    String(StringEvalError),
    Date(DateEvalError),
    Math(MathError),
    Arithmetic(ArithmeticError),
    Type(TypeEvalError),
    Argument(&'static str),
    Unsupported(Func),
}
impl std::fmt::Display for FunctionError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(e) => e.fmt(out),
            Self::Date(e) => e.fmt(out),
            Self::Math(e) => e.fmt(out),
            Self::Arithmetic(e) => e.fmt(out),
            Self::Type(e) => e.fmt(out),
            Self::Argument(text) => out.write_str(text),
            Self::Unsupported(f) => write!(out, "function {f:?} is not a value function"),
        }
    }
}
impl std::error::Error for FunctionError {}

pub fn try_call(
    f: Func,
    args: &ScopedRow,
    ty: &QType,
    coll: &ManagedCollation,
    dec: Dec,
    constant: bool,
    max_number_limbs: u64,
) -> Result<ManagedValue, FunctionError> {
    match f {
        Func::Sqrt
        | Func::Log
        | Func::Log10
        | Func::Exp
        | Func::Sin
        | Func::Cos
        | Func::ACos
        | Func::ASin
        | Func::ATan
        | Func::Pow => {
            return math::try_math(f, args, ty, constant, max_number_limbs)
                .map_err(FunctionError::Math);
        }
        Func::Round => return round(args, dec, max_number_limbs),
        Func::Int => {
            return match args.values() {
                [v] if matches!(v.view(), ValueRef::Null | ValueRef::Number(_)) => {
                    arithmetic::try_unary(UnaryOp::Trunc, args, 0, max_number_limbs)
                        .map_err(FunctionError::Arithmetic)
                }
                _ => Err(FunctionError::Argument("ЦЕЛ takes a number")),
            };
        }
        Func::Sum | Func::Min | Func::Max | Func::Avg | Func::Count => {
            return Err(FunctionError::Argument(
                "an aggregate outside an aggregation",
            ));
        }
        Func::IsNull => {
            let [a, b] = args.values() else {
                return Err(FunctionError::Argument("ЕСТЬNULL takes two arguments"));
            };
            return Ok(if matches!(a.view(), ValueRef::Null) {
                b.clone()
            } else {
                a.clone()
            });
        }
        _ => {}
    }
    if args
        .values()
        .iter()
        .any(|v| matches!(v.view(), ValueRef::Null))
    {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    match f {
        Func::Uuid => {
            if args.values().len() != 1 {
                return Err(FunctionError::Argument(
                    "УНИКАЛЬНЫЙИДЕНТИФИКАТОР takes a reference",
                ));
            }
            type_info::try_reference_uuid(args, 0).map_err(FunctionError::Type)
        }
        Func::Substring
        | Func::String
        | Func::StringLength
        | Func::TrimL
        | Func::TrimR
        | Func::TrimAll
        | Func::Left
        | Func::Right
        | Func::StrFind
        | Func::Upper
        | Func::Lower
        | Func::StrReplace => strings::try_call(f, args, ty, coll).map_err(FunctionError::String),
        Func::Year
        | Func::Quarter
        | Func::Month
        | Func::DayOfYear
        | Func::Day
        | Func::Week
        | Func::WeekDay
        | Func::Hour
        | Func::Minute
        | Func::Second
        | Func::BeginOfPeriod
        | Func::EndOfPeriod
        | Func::DateAdd
        | Func::DateDiff => dates::try_call(f, args).map_err(FunctionError::Date),
        other => Err(FunctionError::Unsupported(other)),
    }
}

fn round(args: &ScopedRow, dec: Dec, max_number_limbs: u64) -> Result<ManagedValue, FunctionError> {
    let values = args.values();
    let first_null = values
        .first()
        .is_some_and(|v| matches!(v.view(), ValueRef::Null));
    let second_null = matches!(values, [_,b] if matches!(b.view(), ValueRef::Null));
    if first_null || second_null {
        return Ok(ManagedValue::scalar(ScalarValue::Null));
    }
    let digits = match values {
        [a] if matches!(a.view(), ValueRef::Number(_)) => 0,
        [a, b] if matches!(a.view(), ValueRef::Number(_)) => match b.view() {
            ValueRef::Number(n) => n.trunc_to_i64_saturating(),
            _ => return Err(FunctionError::Argument("ОКР takes numbers")),
        },
        _ => return Err(FunctionError::Argument("ОКР takes numbers")),
    };
    arithmetic::try_unary(UnaryOp::Round { digits, dec }, args, 0, max_number_limbs)
        .map_err(FunctionError::Arithmetic)
}
