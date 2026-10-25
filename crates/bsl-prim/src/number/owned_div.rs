use super::{LimbAllocator, NumberOpError, NumberOutput, normalize};
use crate::{Number, limbs::BASE};
use std::cmp::Ordering;

const BASE64: u64 = BASE as u64;

fn multiply_into(input: &[u32], factor: u32, output: &mut [u32]) -> u32 {
    let mut carry = 0u64;
    for (&limb, target) in input.iter().zip(output) {
        let value = u64::from(limb) * u64::from(factor) + carry;
        *target = (value % BASE64) as u32;
        carry = value / BASE64;
    }
    carry as u32
}

fn twice_cmp(remainder: &[u32], divisor: &[u32]) -> Ordering {
    let mut carry = 0u32;
    let mut order = Ordering::Equal;
    for (&r, &d) in remainder.iter().zip(divisor) {
        let value = r * 2 + carry;
        carry = u32::from(value >= BASE);
        let limb = value % BASE;
        let next = limb.cmp(&d);
        if next != Ordering::Equal {
            order = next;
        }
    }
    if carry != 0 { Ordering::Greater } else { order }
}

fn knuth(u: &mut [u32], v: &[u32], quotient: &mut [u32]) {
    let n = v.len();
    let m = u.len() - n - 1;
    let (vt, vs) = (u64::from(v[n - 1]), u64::from(v[n - 2]));
    for j in (0..=m).rev() {
        let num = u64::from(u[j + n]) * BASE64 + u64::from(u[j + n - 1]);
        let mut qhat = num / vt;
        let mut rhat = num % vt;
        while qhat >= BASE64 || qhat * vs > rhat * BASE64 + u64::from(u[j + n - 2]) {
            qhat -= 1;
            rhat += vt;
            if rhat >= BASE64 {
                break;
            }
        }
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let product = qhat * u64::from(v[i]) + carry;
            carry = product / BASE64;
            let mut value = i64::from(u[i + j]) - (product % BASE64) as i64 - borrow;
            borrow = i64::from(value < 0);
            if value < 0 {
                value += i64::from(BASE);
            }
            u[i + j] = value as u32;
        }
        let mut value = i64::from(u[j + n]) - carry as i64 - borrow;
        if value < 0 {
            value += i64::from(BASE);
            u[j + n] = value as u32;
            qhat -= 1;
            let mut carry = 0u32;
            for i in 0..n {
                let mut sum = u[i + j] + v[i] + carry;
                carry = u32::from(sum >= BASE);
                if carry != 0 {
                    sum -= BASE;
                }
                u[i + j] = sum;
            }
            u[j + n] = (u[j + n] + carry) % BASE;
        } else {
            u[j + n] = value as u32;
        }
        quotient[j] = qhat as u32;
    }
}

fn increment(limbs: &mut [u32]) {
    for limb in limbs {
        if *limb < BASE - 1 {
            *limb += 1;
            return;
        }
        *limb = 0;
    }
    unreachable!("missing quotient carry limb");
}

impl Number {
    pub fn try_div_with<A: LimbAllocator>(
        &self,
        other: &Number,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        if other.is_zero() {
            return Err(NumberOpError::DivByZero);
        }
        if self.is_zero() {
            return Ok(NumberOutput::Inline(Number::zero()));
        }
        self.mag.with_limbs(|x| {
            other.mag.with_limbs(|y| {
                let n = y.len();
                let n_signed = i64::try_from(n).map_err(|_| NumberOpError::Overflow)?;
                let fraction = n_signed
                    .checked_add(i64::from(self.scale))
                    .and_then(|value| value.checked_add(2))
                    .and_then(|value| value.checked_sub(i64::from(other.scale)))
                    .ok_or(NumberOpError::Overflow)?;
                let extra = if fraction < 0 {
                    usize::try_from(fraction.unsigned_abs()).map_err(|_| NumberOpError::Overflow)?
                } else {
                    0
                };
                let scale = if fraction < 0 {
                    0
                } else {
                    u32::try_from(fraction).unwrap_or(u32::MAX)
                };
                let shift = n.checked_add(2).ok_or(NumberOpError::Overflow)?;
                let dividend_len = x.len().checked_add(shift).ok_or(NumberOpError::Overflow)?;
                let output_len = x
                    .len()
                    .checked_add(4)
                    .and_then(|len| len.checked_add(extra))
                    .ok_or(NumberOpError::Overflow)?;
                let mut output = alloc
                    .allocate_zeroed(output_len)
                    .map_err(NumberOpError::Buffer)?;
                let up = if n == 1 {
                    let mut remainder = 0u64;
                    for index in (0..dividend_len).rev() {
                        let limb = index
                            .checked_sub(shift)
                            .and_then(|i| x.get(i))
                            .copied()
                            .unwrap_or(0);
                        let value = remainder * BASE64 + u64::from(limb);
                        output.as_mut()[index + extra] = (value / u64::from(y[0])) as u32;
                        remainder = value % u64::from(y[0]);
                    }
                    remainder * 2 >= u64::from(y[0])
                } else {
                    let factor = BASE / (y[n - 1] + 1);
                    let mut divisor = alloc.allocate_zeroed(n).map_err(NumberOpError::Buffer)?;
                    let carry = multiply_into(y, factor, divisor.as_mut());
                    debug_assert_eq!(carry, 0);
                    let scratch_len = dividend_len.checked_add(1).ok_or(NumberOpError::Overflow)?;
                    let mut dividend = alloc
                        .allocate_zeroed(scratch_len)
                        .map_err(NumberOpError::Buffer)?;
                    let carry =
                        multiply_into(x, factor, &mut dividend.as_mut()[shift..dividend_len]);
                    dividend.as_mut()[dividend_len] = carry;
                    knuth(
                        dividend.as_mut(),
                        divisor.as_ref(),
                        &mut output.as_mut()[extra..],
                    );
                    let up = twice_cmp(&dividend.as_ref()[..n], divisor.as_ref()) != Ordering::Less;
                    drop(dividend);
                    drop(divisor);
                    up
                };
                if up {
                    increment(&mut output.as_mut()[extra..]);
                }
                Ok(normalize(self.neg != other.neg, scale, output))
            })
        })
    }
}
