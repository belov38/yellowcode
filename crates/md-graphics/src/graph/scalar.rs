use crate::templates::allocation::Allocation;
use memory_budget::BudgetError;
use std::fmt::{self, Write};

#[derive(Debug)]
pub(crate) enum ScalarError {
    Memory(BudgetError),
    Invalid,
}
impl From<BudgetError> for ScalarError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}

pub(crate) struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}
impl<const N: usize> Text<N> {
    pub(crate) fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(self.bytes.get(..self.len).unwrap_or_default()).unwrap_or_default()
    }
    fn zeros(&mut self, count: usize) -> Result<(), ScalarError> {
        for _ in 0..count {
            self.write_str("0").map_err(|_| ScalarError::Invalid)?;
        }
        Ok(())
    }
}
impl<const N: usize> Write for Text<N> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub(crate) fn integer(text: &str) -> Option<i64> {
    let text = text.trim_matches(py_space);
    let (negative, body) = if let Some(body) = text.strip_prefix('-') {
        (true, body)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    let mut value = 0i64;
    let mut after_digit = false;
    for byte in body.bytes() {
        if byte.is_ascii_digit() {
            let digit = i64::from(byte - b'0');
            value = if negative {
                value.checked_mul(10)?.checked_sub(digit)?
            } else {
                value.checked_mul(10)?.checked_add(digit)?
            };
            after_digit = true;
        } else if byte == b'_' && after_digit {
            after_digit = false;
        } else {
            return None;
        }
    }
    after_digit.then_some(value)
}

pub(crate) fn number(text: &str, allocation: Allocation<'_>) -> Result<f64, ScalarError> {
    let text = text.trim_matches(py_space);
    if !text.as_bytes().contains(&b'_') {
        return text.parse().map_err(|_| ScalarError::Invalid);
    }
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'_'
            && !(at
                .checked_sub(1)
                .and_then(|at| text.as_bytes().get(at))
                .is_some_and(u8::is_ascii_digit)
                && text.as_bytes().get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            return Err(ScalarError::Invalid);
        }
    }
    let mut clean = allocation.vector(text.len())?;
    for byte in text.bytes().filter(|byte| *byte != b'_') {
        clean.push(byte)?;
    }
    let text = std::str::from_utf8(&clean).map_err(|_| ScalarError::Invalid)?;
    text.parse().map_err(|_| ScalarError::Invalid)
}

pub(crate) fn xml_double(value: f64) -> Result<Text<384>, ScalarError> {
    let mut output = Text::new();
    if !value.is_finite() {
        return Err(ScalarError::Invalid);
    }
    if value == 0.0 {
        output.write_str("0").map_err(|_| ScalarError::Invalid)?;
        return Ok(output);
    }
    let mut scientific = Text::<64>::new();
    write!(&mut scientific, "{:.14e}", value.abs()).map_err(|_| ScalarError::Invalid)?;
    let (mantissa, exponent) = scientific
        .as_str()
        .split_once('e')
        .ok_or(ScalarError::Invalid)?;
    let exponent: i32 = exponent.parse().map_err(|_| ScalarError::Invalid)?;
    let mut digits = Text::<32>::new();
    for part in mantissa.split('.') {
        digits.write_str(part).map_err(|_| ScalarError::Invalid)?;
    }
    let digits = digits.as_str().trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let point = exponent + 1;
    if value < 0.0 {
        output.write_str("-").map_err(|_| ScalarError::Invalid)?;
    }
    if point <= 0 {
        output.write_str("0.").map_err(|_| ScalarError::Invalid)?;
        output.zeros(usize::try_from(-point).map_err(|_| ScalarError::Invalid)?)?;
        output.write_str(digits).map_err(|_| ScalarError::Invalid)?;
    } else {
        let point = usize::try_from(point).map_err(|_| ScalarError::Invalid)?;
        if point >= digits.len() {
            output.write_str(digits).map_err(|_| ScalarError::Invalid)?;
            output.zeros(point - digits.len())?;
        } else {
            let (left, right) = digits.split_at(point);
            write!(&mut output, "{left}.{right}").map_err(|_| ScalarError::Invalid)?;
        }
    }
    Ok(output)
}
