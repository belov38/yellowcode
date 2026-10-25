use super::{Mag, Number, small};
use crate::limbs::{BASE, Limbs};
use std::cmp::Ordering;

#[path = "owned_div.rs"]
mod div;
#[path = "owned_parse.rs"]
mod parse;
#[path = "owned_rem.rs"]
mod rem;
#[path = "owned_round.rs"]
mod round;

pub trait LimbBuffer: AsRef<[u32]> + AsMut<[u32]> {
    fn truncate(&mut self, len: usize);
}

pub trait LimbAllocator {
    type Error;
    type Buffer: LimbBuffer;

    fn allocate_zeroed(&self, len: usize) -> Result<Self::Buffer, Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum NumberOpError<E> {
    Buffer(E),
    DivByZero,
    Overflow,
}

pub enum NumberOutput<B> {
    Inline(Number),
    Buffered { parts: NumberParts, limbs: B },
}

pub struct NumberParts {
    neg: bool,
    scale: u32,
    len: usize,
}

impl NumberParts {
    pub fn into_number(self, limbs: Vec<u32>) -> Number {
        assert_eq!(limbs.len(), self.len, "changed arithmetic output length");
        assert!(limbs.len() >= 3 && limbs.last() != Some(&0));
        assert!(self.scale == 0 || limbs.first() != Some(&0));
        assert!(limbs.iter().all(|&limb| limb < BASE));
        Number {
            neg: self.neg,
            scale: self.scale,
            mag: Mag::Big(Limbs(limbs)),
        }
    }
}

fn normalize<B: LimbBuffer>(neg: bool, mut scale: u32, mut limbs: B) -> NumberOutput<B> {
    let len = limbs
        .as_ref()
        .iter()
        .rposition(|&limb| limb != 0)
        .map_or(0, |i| i + 1);
    limbs.truncate(len);
    let zeros = limbs
        .as_ref()
        .iter()
        .take(scale as usize)
        .take_while(|&&limb| limb == 0)
        .count();
    if zeros > 0 {
        limbs.as_mut().copy_within(zeros.., 0);
        limbs.truncate(len - zeros);
        scale -= zeros as u32;
    }
    let data = limbs.as_ref();
    if data.len() <= 2 {
        let magnitude = data
            .iter()
            .rev()
            .fold(0u64, |m, &limb| m * u64::from(BASE) + u64::from(limb));
        drop(limbs);
        return NumberOutput::Inline(small::canon(neg, scale, magnitude));
    }
    let parts = NumberParts {
        neg,
        scale,
        len: data.len(),
    };
    NumberOutput::Buffered { parts, limbs }
}

#[derive(Clone, Copy)]
struct Decimal<'a> {
    neg: bool,
    scale: u32,
    limbs: &'a [u32],
}

impl Decimal<'_> {
    fn inline(self) -> Option<Number> {
        (self.limbs.len() <= 2).then(|| {
            let magnitude = self
                .limbs
                .iter()
                .rev()
                .fold(0u64, |m, &limb| m * u64::from(BASE) + u64::from(limb));
            small::canon(self.neg, self.scale, magnitude)
        })
    }
}

fn with_native<R>(value: &Number, read: impl FnOnce(Decimal<'_>) -> R) -> R {
    value.mag.with_limbs(|limbs| {
        read(Decimal {
            neg: value.neg,
            scale: value.scale,
            limbs,
        })
    })
}

impl<B: LimbBuffer> NumberOutput<B> {
    fn with_decimal<R>(&self, read: impl FnOnce(Decimal<'_>) -> R) -> R {
        match self {
            Self::Inline(value) => with_native(value, read),
            Self::Buffered { parts, limbs } => read(Decimal {
                neg: parts.neg,
                scale: parts.scale,
                limbs: limbs.as_ref(),
            }),
        }
    }
}

fn add_decimals<A: LimbAllocator>(
    a: Decimal<'_>,
    b: Decimal<'_>,
    alloc: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    if let (Some(left), Some(right)) = (a.inline(), b.inline())
        && let Some(number) = small::try_add(&left, &right, right.neg)
    {
        return Ok(NumberOutput::Inline(number));
    }
    let order = super::arith::cmp_limbs(a.limbs, a.scale, b.limbs, b.scale);
    if a.neg != b.neg && order == Ordering::Equal {
        return Ok(NumberOutput::Inline(Number::zero()));
    }
    let scale = a.scale.max(b.scale);
    let (x, y) = (a.limbs, b.limbs);
    let x_offset = (scale - a.scale) as usize;
    let y_offset = (scale - b.scale) as usize;
    let aligned_len = |data: &[u32], offset: usize| {
        if data.is_empty() {
            Ok(0)
        } else {
            data.len()
                .checked_add(offset)
                .ok_or(NumberOpError::Overflow)
        }
    };
    let len = aligned_len(x, x_offset)?.max(aligned_len(y, y_offset)?);
    let capacity = len.checked_add(1).ok_or(NumberOpError::Overflow)?;
    let mut output = alloc
        .allocate_zeroed(capacity)
        .map_err(NumberOpError::Buffer)?;
    let get = |data: &[u32], offset: usize, index: usize| {
        index
            .checked_sub(offset)
            .and_then(|i| data.get(i))
            .copied()
            .unwrap_or(0)
    };
    let same_sign = a.neg == b.neg;
    let swapped = !same_sign && order == Ordering::Less;
    let mut carry = 0u64;
    let mut borrow = 0i64;
    for (index, limb) in output.as_mut()[..len].iter_mut().enumerate() {
        let (mut left, mut right) = (get(x, x_offset, index), get(y, y_offset, index));
        if swapped {
            std::mem::swap(&mut left, &mut right);
        }
        *limb = if same_sign {
            let value = u64::from(left) + u64::from(right) + carry;
            carry = value / u64::from(BASE);
            (value % u64::from(BASE)) as u32
        } else {
            let value = i64::from(left) - i64::from(right) - borrow;
            borrow = i64::from(value < 0);
            (value + borrow * i64::from(BASE)) as u32
        };
    }
    output.as_mut()[len] = carry as u32;
    Ok(normalize(
        if swapped { b.neg } else { a.neg },
        scale,
        output,
    ))
}

fn add_signed<A: LimbAllocator>(
    a: &Number,
    b: &Number,
    b_neg: bool,
    alloc: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    with_native(a, |left| {
        with_native(b, |mut right| {
            right.neg = b_neg;
            add_decimals(left, right, alloc)
        })
    })
}

fn multiply_decimals<A: LimbAllocator>(
    a: Decimal<'_>,
    b: Decimal<'_>,
    alloc: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    if a.limbs.is_empty() || b.limbs.is_empty() {
        return Ok(NumberOutput::Inline(Number::zero()));
    }
    if let (Some(left), Some(right)) = (a.inline(), b.inline())
        && let Some(number) = small::try_mul(&left, &right)
    {
        return Ok(NumberOutput::Inline(number));
    }
    let (x, y) = (a.limbs, b.limbs);
    let len = x
        .len()
        .checked_add(y.len())
        .ok_or(NumberOpError::Overflow)?;
    let mut output = alloc.allocate_zeroed(len).map_err(NumberOpError::Buffer)?;
    let data = output.as_mut();
    for (i, &left) in x.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &right) in y.iter().enumerate() {
            let value = u64::from(data[i + j]) + u64::from(left) * u64::from(right) + carry;
            data[i + j] = (value % u64::from(BASE)) as u32;
            carry = value / u64::from(BASE);
        }
        data[i + y.len()] = carry as u32;
    }
    Ok(normalize(
        a.neg != b.neg,
        a.scale.saturating_add(b.scale),
        output,
    ))
}

impl Number {
    pub fn try_add_with<A: LimbAllocator>(
        &self,
        other: &Number,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        add_signed(self, other, other.neg, alloc)
    }

    pub fn try_sub_with<A: LimbAllocator>(
        &self,
        other: &Number,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        add_signed(self, other, !other.neg, alloc)
    }

    pub fn try_mul_with<A: LimbAllocator>(
        &self,
        other: &Number,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        with_native(self, |left| {
            with_native(other, |right| multiply_decimals(left, right, alloc))
        })
    }

    pub fn try_neg_with<A: LimbAllocator>(
        &self,
        alloc: &A,
    ) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
        if let Mag::Small(magnitude) = self.mag {
            return Ok(NumberOutput::Inline(small::canon(
                !self.neg, self.scale, magnitude,
            )));
        }
        self.mag.with_limbs(|data| {
            let mut output = alloc
                .allocate_zeroed(data.len())
                .map_err(NumberOpError::Buffer)?;
            output.as_mut().copy_from_slice(data);
            Ok(normalize(!self.neg, self.scale, output))
        })
    }
}
