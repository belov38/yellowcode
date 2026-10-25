use super::enum_of;
use crate::bind::{arg_bool, arg_i64, arg_number, arg_str, arg_value, global, is_given, lib_error};
use bsl_lib::numfn::{self, MathFn};
use bsl_prim::{Number, RoundMode};
use bsl_vm::{Arg, CallResult, Category, CategorySet, Execution, HostError, Registry, Value};
use platform_catalog::messages::{self, Lang, Wrapper, text_key};

const POW_LIMBS_CAP: u64 = 20_000;
const LIMB_BYTES: u64 = 16;

pub(super) fn register(reg: &mut Registry) {
    global(reg, "Pow", "Pow", pow);
    for (ru, en, f) in [
        ("Sqrt", "Sqrt", MathFn::Sqrt),
        ("Exp", "Exp", MathFn::Exp),
        ("Log", "Log", MathFn::Log),
        ("Log10", "Log10", MathFn::Log10),
        ("Sin", "Sin", MathFn::Sin),
        ("Cos", "Cos", MathFn::Cos),
        ("Tan", "Tan", MathFn::Tan),
        ("ASin", "ASin", MathFn::ASin),
        ("ACos", "ACos", MathFn::ACos),
        ("ATan", "ATan", MathFn::ATan),
    ] {
        global(reg, ru, en, move |ex, a| {
            let x = arg_number(ex, a.first(), 1)?;
            Ok(Value::Number(numfn::math(f, &x).map_err(lib_error)?))
        });
    }
    global(reg, "Цел", "Int", |ex, a| {
        Ok(Value::Number(arg_number(ex, a.first(), 1)?.trunc()))
    });
    global(reg, "Окр", "Round", round);
    bitwise(reg);
    global(
        reg,
        "ЧислоИзШестнадцатеричнойСтроки",
        "NumberFromHexString",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            Ok(Value::Number(
                numfn::from_hex_string(s.units()).map_err(conv_error)?,
            ))
        },
    );
    global(
        reg,
        "ЧислоИзДвоичнойСтроки",
        "NumberFromBinaryString",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            Ok(Value::Number(
                numfn::from_bin_string(s.units()).map_err(conv_error)?,
            ))
        },
    );
}

fn conv_error(e: bsl_lib::LibError) -> HostError {
    let line = |lang| {
        let first = text_key("core85:IDS_ERR_ILLEGAL_ARGUMENT", lang);
        debug_assert!(
            first.is_some(),
            "IDS_ERR_ILLEGAL_ARGUMENT is in the catalog"
        );
        let second = text_key(e.key, lang);
        debug_assert!(second.is_some(), "{} is in the catalog", e.key);
        let second = second.unwrap_or(e.key);
        format!("{}\n{second}{}", first.unwrap_or_default(), e.suffix)
    };
    let mut err = HostError::text(&line(Lang::Ru), &line(Lang::En));
    err.categories = Some(
        messages::categories(Wrapper::ContextCall, e.key)
            .map(CategorySet)
            .unwrap_or_else(|| {
                CategorySet::of(&[
                    Category::AllErrors,
                    Category::ScriptRuntimeError,
                    Category::OtherError,
                ])
            }),
    );
    err
}

fn pow(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let base = arg_number(ex, a.first(), 1)?;
    let exp = arg_number(ex, a.get(1), 2)?;
    let trivial = matches!(base.to_i64(), Some(-1..=1));
    if let (Some(e), false) = (exp.to_i64(), trivial) {
        let limbs = base
            .size_hint_limbs()
            .saturating_mul(e.unsigned_abs())
            .min(POW_LIMBS_CAP);
        ex.reserve(limbs.saturating_mul(LIMB_BYTES))?;
    }
    Ok(Value::Number(numfn::pow(&base, &exp).map_err(lib_error)?))
}

fn round(ex: &mut Execution, a: &mut [Arg]) -> CallResult<Value> {
    let x = arg_number(ex, a.first(), 1)?;
    let digits = if is_given(a.get(1)) {
        arg_i64(ex, a.get(1), 2)?
    } else {
        0
    };
    let mode = match arg_value(ex, a.get(2))? {
        Value::Number(n) if n.is_zero() => RoundMode::HalfTowardZero,
        Value::Number(_) => RoundMode::HalfAwayFromZero,
        v => match enum_of(&v, "РежимОкругления", 3)? {
            Some("Окр15как10") => RoundMode::HalfTowardZero,
            _ => RoundMode::HalfAwayFromZero,
        },
    };
    Ok(Value::Number(x.round(digits, mode)))
}

fn word(ex: &mut Execution, a: &[Arg], i: usize) -> CallResult<u32> {
    let n = arg_number(ex, a.get(i), i + 1)?;
    Ok(numfn::to_u32(&n).map_err(lib_error)?)
}

fn word_value(v: u32) -> Value {
    Value::Number(Number::from(v))
}

fn bitwise(reg: &mut Registry) {
    type Op = fn(u32, u32) -> u32;
    let binary: [(&str, &str, Op); 4] = [
        ("ПобитовоеИ", "BitwiseAnd", |x, y| x & y),
        ("ПобитовоеИли", "BitwiseOr", |x, y| x | y),
        ("ПобитовоеИсключительноеИли", "BitwiseXor", |x, y| x ^ y),
        ("ПобитовоеИНе", "BitwiseAndNot", |x, y| x & !y),
    ];
    for (ru, en, op) in binary {
        global(reg, ru, en, move |ex, a| {
            let x = word(ex, a, 0)?;
            let y = word(ex, a, 1)?;
            Ok(word_value(op(x, y)))
        });
    }
    global(reg, "ПобитовоеНе", "BitwiseNot", |ex, a| {
        Ok(word_value(!word(ex, a, 0)?))
    });
    type Shift = fn(u32, &Number) -> Result<u32, bsl_lib::LibError>;
    let shifts: [(&str, &str, Shift); 2] = [
        ("ПобитовыйСдвигВлево", "BitwiseShiftLeft", numfn::shift_left),
        (
            "ПобитовыйСдвигВправо",
            "BitwiseShiftRight",
            numfn::shift_right,
        ),
    ];
    for (ru, en, f) in shifts {
        global(reg, ru, en, move |ex, a| {
            let x = word(ex, a, 0)?;
            let n = arg_number(ex, a.get(1), 2)?;
            Ok(word_value(f(x, &n).map_err(lib_error)?))
        });
    }
    global(reg, "ПроверитьБит", "CheckBit", |ex, a| {
        let x = word(ex, a, 0)?;
        let n = arg_number(ex, a.get(1), 2)?;
        Ok(Value::Bool(numfn::check_bit(x, &n).map_err(lib_error)?))
    });
    global(reg, "УстановитьБит", "SetBit", |ex, a| {
        let x = word(ex, a, 0)?;
        let n = arg_number(ex, a.get(1), 2)?;
        let v = arg_bool(ex, a.get(2))?;
        Ok(word_value(numfn::set_bit(x, &n, v).map_err(lib_error)?))
    });
    global(
        reg,
        "ПроверитьПоБитовойМаске",
        "CheckByBitMask",
        |ex, a| {
            let x = word(ex, a, 0)?;
            let m = word(ex, a, 1)?;
            Ok(Value::Bool(numfn::check_mask(x, m)))
        },
    );
}
