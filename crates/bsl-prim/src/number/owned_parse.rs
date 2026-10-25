use super::{LimbAllocator, NumberOpError, NumberOutput, normalize};
use crate::Number;
use crate::number::small;

fn decimal<A: LimbAllocator>(
    negative: bool,
    len: usize,
    digit: impl Fn(usize) -> u8,
    groups: u32,
    padding: usize,
    allocator: &A,
) -> Result<NumberOutput<A::Buffer>, NumberOpError<A::Error>> {
    let Some(start) = (0..len).find(|&index| digit(index) != 0) else {
        return Ok(NumberOutput::Inline(Number::zero()));
    };
    let end = len.checked_add(padding).ok_or(NumberOpError::Overflow)?;
    let count = end.checked_sub(start).ok_or(NumberOpError::Overflow)?;
    let digit_at = |index| {
        if index < len {
            u32::from(digit(index))
        } else {
            0
        }
    };
    if count <= 18 {
        let magnitude =
            (start..end).fold(0u64, |value, index| value * 10 + u64::from(digit_at(index)));
        return Ok(NumberOutput::Inline(small::canon(
            negative, groups, magnitude,
        )));
    }
    let mut limbs = allocator
        .allocate_zeroed(count.div_ceil(9))
        .map_err(NumberOpError::Buffer)?;
    let mut cursor = end;
    for limb in limbs.as_mut() {
        let begin = cursor.saturating_sub(9).max(start);
        *limb = (begin..cursor).fold(0u32, |value, index| value * 10 + digit_at(index));
        cursor = begin;
    }
    Ok(normalize(negative, groups, limbs))
}

impl Number {
    pub fn try_from_decimal_digits_with<A: LimbAllocator>(
        negative: bool,
        digits: &[u8],
        scale: u32,
        allocator: &A,
    ) -> Result<Option<NumberOutput<A::Buffer>>, NumberOpError<A::Error>> {
        if digits.is_empty() || digits.iter().any(|&digit| digit > 9) {
            return Ok(None);
        }
        decimal(
            negative,
            digits.len(),
            |index| digits[index],
            scale.div_ceil(9),
            ((9 - scale % 9) % 9) as usize,
            allocator,
        )
        .map(Some)
    }

    pub fn try_parse_owned_with<A: LimbAllocator>(
        text: &str,
        allocator: &A,
    ) -> Result<Option<NumberOutput<A::Buffer>>, NumberOpError<A::Error>> {
        let bytes = text.as_bytes();
        let (negative, bytes) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        let (int, frac) = match bytes.iter().position(|&byte| byte == b'.') {
            Some(index) => (&bytes[..index], &bytes[index + 1..]),
            None => (bytes, &bytes[..0]),
        };
        if (int.is_empty() && frac.is_empty()) || !int.iter().chain(frac).all(u8::is_ascii_digit) {
            return Ok(None);
        }
        let Ok(groups) = u32::try_from(frac.len().div_ceil(9)) else {
            return Ok(None);
        };
        let padding = (9 - frac.len() % 9) % 9;
        let len = int
            .len()
            .checked_add(frac.len())
            .ok_or(NumberOpError::Overflow)?;
        decimal(
            negative,
            len,
            |index| {
                if index < int.len() {
                    int[index] - b'0'
                } else {
                    frac[index - int.len()] - b'0'
                }
            },
            groups,
            padding,
            allocator,
        )
        .map(Some)
    }
}
