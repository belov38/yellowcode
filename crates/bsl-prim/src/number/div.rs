use super::{DivByZero, Mag, Number, RoundMode, small};
use crate::limbs::Limbs;
use std::cmp::Ordering;

pub(super) fn div(a: &Number, b: &Number) -> Result<Number, DivByZero> {
    if b.is_zero() {
        return Err(DivByZero);
    }
    let (am, bm) = (a.mag.limbs(), b.mag.limbs());
    let lb = bm.0.len();
    let f = i64::from(a.scale) + lb as i64 + 2 - i64::from(b.scale);
    let (mut q, r) = am.shl_limbs(lb + 2).divmod(&bm);
    if r.add(&r).cmp_mag(&bm) != Ordering::Less {
        q = q.add(&Limbs::from_u64(1));
    }
    let (mag, scale) = match u32::try_from(f) {
        Ok(f) => (q, f),
        Err(_) if f > 0 => (q, u32::MAX),
        Err(_) => (q.shl_limbs(f.unsigned_abs() as usize), 0),
    };
    Ok(Number::from_limbs(a.neg != b.neg, scale, mag))
}

pub(super) fn trunc(a: &Number) -> Number {
    if let Mag::Small(m) = a.mag {
        return small::canon(a.neg, 0, small::int_part(m, a.scale));
    }
    Number::from_limbs(a.neg, 0, a.int_limbs())
}

pub(super) fn rem(a: &Number, b: &Number) -> Result<Number, DivByZero> {
    let q = trunc(&div(a, b)?);
    Ok(a.sub(&b.mul(&q)))
}

pub(super) fn round(a: &Number, digits: i64, mode: RoundMode) -> Number {
    let frac_digits = 9 * i64::from(a.scale);
    if digits >= frac_digits {
        return a.clone();
    }
    let shift = frac_digits.saturating_sub(digits);
    let limbs = a.mag.limbs();
    if shift > limbs.decimal_len() as i64 {
        return Number::zero();
    }
    let unit = Limbs::pow10(shift as u32);
    let (mut q, r) = limbs.divmod(&unit);
    let twice = r.add(&r).cmp_mag(&unit);
    let up = match mode {
        RoundMode::HalfAwayFromZero => twice != Ordering::Less,
        RoundMode::HalfTowardZero => twice == Ordering::Greater,
    };
    if up {
        q = q.add(&Limbs::from_u64(1));
    }
    Number::from_limbs(a.neg, a.scale, q.mul(&unit))
}
