use crate::error::{LibError, msg};
use bsl_prim::Number;

const EXACT_LIMBS_CAP: u64 = 20_000;
const EXACT_EXP_MAX: u64 = 100_000;
const RADIX_BITS_CAP: usize = EXACT_LIMBS_CAP as usize * 29;

fn bad_arg(name: &str) -> LibError {
    LibError::new(msg::BADARGVALUE).with_suffix(format!(" ({name})"))
}

fn pow_exact(base: &Number, mut e: u64) -> Number {
    let mut acc = Number::one();
    let mut sq = base.clone();
    while e > 0 {
        if e & 1 == 1 {
            acc = acc.mul(&sq);
        }
        e >>= 1;
        if e > 0 {
            sq = sq.mul(&sq);
        }
    }
    acc
}

fn exact_limbs_estimate(base: &Number, e: u64) -> u64 {
    let ef = e as f64;
    let f = base.to_f64().abs();
    let l = if f.is_finite() {
        f.log10()
    } else {
        base.int_digits().len() as f64
    };
    let int_digits = if l > 0.0 { l * ef + 1.0 } else { 0.0 };
    let frac_digits = base.frac_digits().len() as f64 * ef;
    let total = int_digits + frac_digits;
    if total.is_finite() && total < 1e15 {
        total as u64 / 9 + 1
    } else {
        u64::MAX
    }
}

pub fn pow(base: &Number, exp: &Number) -> Result<Number, LibError> {
    if let Some(e) = exp.to_i64() {
        let ae = e.unsigned_abs();
        if ae <= EXACT_EXP_MAX {
            if e < 0 && base.is_zero() {
                return Err(bad_arg("Pow"));
            }
            if exact_limbs_estimate(base, ae) <= EXACT_LIMBS_CAP {
                let p = pow_exact(base, ae);
                return if e < 0 {
                    Number::one().div(&p).map_err(|_| bad_arg("Pow"))
                } else {
                    Ok(p)
                };
            }
        }
    } else if base.is_negative() && !exp.is_integer() {
        return Err(LibError::new(msg::NEGATIVE_POW));
    }
    let r = base.to_f64().powf(exp.to_f64());
    Number::from_f64_15(r).ok_or_else(|| bad_arg("Pow"))
}

#[derive(Clone, Copy, Debug)]
pub enum MathFn {
    Sqrt,
    Exp,
    Log,
    Log10,
    Sin,
    Cos,
    Tan,
    ASin,
    ACos,
    ATan,
}

pub fn math(f: MathFn, x: &Number) -> Result<Number, LibError> {
    let v = x.to_f64();
    let (name, r) = match f {
        MathFn::Sqrt => ("Sqrt", v.sqrt()),
        MathFn::Exp => ("Exp", v.exp()),
        MathFn::Log => ("Log", v.ln()),
        MathFn::Log10 => ("Log10", v.log10()),
        MathFn::Sin => ("Sin", v.sin()),
        MathFn::Cos => ("Cos", v.cos()),
        MathFn::Tan => ("Tan", v.tan()),
        MathFn::ASin => ("ASin", v.asin()),
        MathFn::ACos => ("ACos", v.acos()),
        MathFn::ATan => ("ATan", v.atan()),
    };
    Number::from_f64_15(r).ok_or_else(|| bad_arg(name))
}

pub fn to_u32(x: &Number) -> Result<u32, LibError> {
    x.to_i64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| LibError::new(msg::INT_RANGE))
}

fn bit_index(n: &Number) -> Result<u32, LibError> {
    n.to_i64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v <= 31)
        .ok_or_else(|| LibError::new(msg::PARAM_VALUE).with_arg(2))
}

pub fn shift_left(x: u32, n: &Number) -> Result<u32, LibError> {
    Ok(x << bit_index(n)?)
}

pub fn shift_right(x: u32, n: &Number) -> Result<u32, LibError> {
    Ok(x >> bit_index(n)?)
}

pub fn check_bit(x: u32, n: &Number) -> Result<bool, LibError> {
    Ok(x >> bit_index(n)? & 1 == 1)
}

pub fn set_bit(x: u32, n: &Number, v: bool) -> Result<u32, LibError> {
    let m = 1u32 << bit_index(n)?;
    Ok(if v { x | m } else { x & !m })
}

pub fn check_mask(x: u32, mask: u32) -> bool {
    x & mask == mask
}

fn from_radix(s: &[u16], prefix: u16, bits: u32, err: &'static str) -> Result<Number, LibError> {
    let fail = || LibError::new(err);
    let lower = |c: u16| {
        if (0x41..=0x5a).contains(&c) {
            c + 32
        } else {
            c
        }
    };
    let digits = match s {
        [0x30, p, rest @ ..] if lower(*p) == prefix => rest,
        _ => return Err(fail()),
    };
    if digits.is_empty() {
        return Err(fail());
    }
    let first = digits
        .iter()
        .position(|&c| c != 0x30)
        .unwrap_or(digits.len());
    let digits = &digits[first..];
    if digits.len().saturating_mul(bits as usize) > RADIX_BITS_CAP {
        return Err(fail());
    }
    let radix = 1u32 << bits;
    let per_chunk = (60 / bits) as usize;
    let mut acc = Number::zero();
    for chunk in digits.chunks(per_chunk) {
        let mut v: u64 = 0;
        for &c in chunk {
            let d = char::from_u32(u32::from(c))
                .and_then(|ch| ch.to_digit(radix))
                .ok_or_else(fail)?;
            v = v << bits | u64::from(d);
        }
        let shift = Number::from_u64(1u64 << (bits as usize * chunk.len()));
        acc = acc.mul(&shift).add(&Number::from_u64(v));
    }
    Ok(acc)
}

pub fn from_hex_string(s: &[u16]) -> Result<Number, LibError> {
    from_radix(s, 0x78, 4, msg::HEX_FORMAT)
}

pub fn from_bin_string(s: &[u16]) -> Result<Number, LibError> {
    from_radix(s, 0x62, 1, msg::BIN_FORMAT)
}
