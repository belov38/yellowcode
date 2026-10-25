use super::{LimbAllocator, NumberOpError, NumberOutput, normalize};
use crate::{Number, RoundMode, limbs::BASE, number::small};
use std::cmp::Ordering;

fn copy<A: LimbAllocator>(
    value: &Number,
    alloc: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    if let Some(magnitude) = value.mag.as_small() {
        return Ok(NumberOutput::Inline(small::canon(
            value.neg,
            value.scale,
            magnitude,
        )));
    }
    value.mag.with_limbs(|data| {
        let mut output = alloc
            .allocate_zeroed(data.len())
            .map_err(NumberOpError::Buffer)?;
        output.as_mut().copy_from_slice(data);
        Ok(normalize(value.neg, value.scale, output))
    })
}

fn rounds_up(order: Ordering, lower_nonzero: bool, mode: RoundMode) -> bool {
    order == Ordering::Greater
        || (order == Ordering::Equal && (lower_nonzero || mode == RoundMode::HalfAwayFromZero))
}

impl Number {
    pub fn try_trunc_with<A: LimbAllocator>(
        &self,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        self.mag.with_limbs(|data| {
            let integer = data.get(self.scale as usize..).unwrap_or(&[]);
            if integer.len() <= 2 {
                let magnitude = integer
                    .iter()
                    .rev()
                    .fold(0u64, |m, &limb| m * u64::from(BASE) + u64::from(limb));
                return Ok(NumberOutput::Inline(small::canon(self.neg, 0, magnitude)));
            }
            let mut output = alloc
                .allocate_zeroed(integer.len())
                .map_err(NumberOpError::Buffer)?;
            output.as_mut().copy_from_slice(integer);
            Ok(normalize(self.neg, 0, output))
        })
    }

    pub fn try_round_with<A: LimbAllocator>(
        &self,
        digits: i64,
        mode: RoundMode,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        let fraction_digits = 9 * i64::from(self.scale);
        if digits >= fraction_digits {
            return copy(self, alloc);
        }
        let shift = fraction_digits.saturating_sub(digits) as u64;
        self.mag.with_limbs(|data| {
            let Some(&top) = data.last() else {
                return Ok(NumberOutput::Inline(Number::zero()));
            };
            let length = data
                .len()
                .checked_sub(1)
                .and_then(|n| n.checked_mul(9))
                .and_then(|n| n.checked_add(top.ilog10() as usize + 1))
                .ok_or(NumberOpError::Overflow)?;
            if shift > length as u64 {
                return Ok(NumberOutput::Inline(Number::zero()));
            }
            if let Some(magnitude) = self.mag.as_small() {
                let unit = 10u64.pow(shift as u32);
                let up = rounds_up((2 * (magnitude % unit)).cmp(&unit), false, mode);
                let rounded = (magnitude / unit + u64::from(up)) * unit;
                if rounded < small::LIMIT {
                    return Ok(NumberOutput::Inline(small::canon(
                        self.neg, self.scale, rounded,
                    )));
                }
            }
            let groups = usize::try_from(shift / 9).map_err(|_| NumberOpError::Overflow)?;
            let partial = (shift % 9) as u32;
            let unit = 10u32.pow(partial);
            let (order, lower_nonzero) = if partial == 0 {
                (
                    data[groups - 1].cmp(&(BASE / 2)),
                    data[..groups - 1].iter().any(|&limb| limb != 0),
                )
            } else {
                (
                    (data[groups] % unit).cmp(&(unit / 2)),
                    data[..groups].iter().any(|&limb| limb != 0),
                )
            };
            let up = rounds_up(order, lower_nonzero, mode);
            let len = data.len().checked_add(1).ok_or(NumberOpError::Overflow)?;
            let mut output = alloc.allocate_zeroed(len).map_err(NumberOpError::Buffer)?;
            let limbs = output.as_mut();
            limbs[..data.len()].copy_from_slice(data);
            limbs[..groups].fill(0);
            limbs[groups] -= limbs[groups] % unit;
            if up {
                let mut carry = unit;
                for limb in &mut limbs[groups..] {
                    let value = *limb + carry;
                    *limb = value % BASE;
                    carry = u32::from(value >= BASE);
                    if carry == 0 {
                        break;
                    }
                }
            }
            Ok(normalize(self.neg, self.scale, output))
        })
    }
}
