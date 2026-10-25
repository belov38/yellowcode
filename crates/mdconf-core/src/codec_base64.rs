use super::allocation::{Allocation, Owned};
use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use brace::Node;
use memory_budget::BudgetError;
const B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
pub(crate) fn picture_bytes(
    allocation: Allocation<'_>,
    source: &str,
) -> Result<Option<Owned<Vec<u8>>>, BudgetError> {
    bytes(
        allocation,
        source.strip_prefix("#base64:").unwrap_or(source),
        true,
    )
}
pub fn node_bytes(
    allocation: Allocation<'_>,
    source: &Node,
) -> Result<Option<Owned<Vec<u8>>>, BudgetError> {
    let Some(body) = source
        .as_atom()
        .and_then(|body| body.strip_prefix("#base64:"))
    else {
        return Ok(None);
    };
    bytes(allocation, body, false)
}
fn bytes(
    allocation: Allocation<'_>,
    source: &str,
    picture: bool,
) -> Result<Option<Owned<Vec<u8>>>, BudgetError> {
    let whitespace = |c: &char| {
        if picture {
            c.is_whitespace()
        } else {
            c.is_ascii_whitespace()
        }
    };
    let length = source
        .chars()
        .filter(|c| !whitespace(c))
        .try_fold(0usize, |length, c| {
            length
                .checked_add(c.len_utf8())
                .ok_or(BudgetError::Overflow)
        })?;
    let remainder = length % 4;
    if (picture && remainder != 0) || remainder == 1 {
        return Ok(None);
    }
    let mut compact = allocation.vector(length)?;
    for c in source.chars().filter(|c| !whitespace(c)) {
        let mut encoded = [0u8; 4];
        compact.extend(c.encode_utf8(&mut encoded).as_bytes())?;
    }
    let padding = compact
        .iter()
        .rev()
        .take(2)
        .take_while(|&&byte| byte == b'=')
        .count();
    let length = (length / 4)
        .checked_mul(3)
        .and_then(|length| length.checked_add(remainder.saturating_sub(1)))
        .ok_or(BudgetError::Overflow)?;
    let Some(length) = length.checked_sub(padding) else {
        return Ok(None);
    };
    let mut output = allocation.vector(length)?;
    for _ in 0..length {
        output.push(0)?;
    }
    let Ok(actual) = B64.decode_slice(&*compact, &mut output) else {
        return Ok(None);
    };
    output.truncate(actual);
    Ok(Some(output.finish()))
}
