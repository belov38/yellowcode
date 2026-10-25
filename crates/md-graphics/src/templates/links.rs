use super::{
    allocation::{Allocation, Owned},
    context::{self, Lookup},
};
use memory_budget::BudgetError;
#[derive(Clone, Copy)]
enum Pass {
    Files,
    Picture,
    Standard,
    Help,
    Lines,
}
#[derive(Clone, Copy)]
struct Replacement<'a> {
    consumed: usize,
    parts: [&'a [u8]; 3],
}
fn uuid(bytes: &[u8]) -> bool {
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_digit() || matches!(b, b'a'..=b'f')
            }
        })
}
fn named<'a>(lookup: &'a dyn Lookup, id: &[u8]) -> Option<&'a str> {
    let text = std::str::from_utf8(id).ok()?;
    lookup
        .name(&brace::Guid::parse(text)?)
        .filter(|n| !n.is_empty())
}
fn standard_digits(digits: &[u8]) -> Option<&'static str> {
    for file in ["templates/tables.json", "ui.json"] {
        for (key, value) in mdschema::static_schema()
            .table(file)
            .and_then(|t| t.get("std_pictures"))
            .and_then(|v| v.as_object())
            .unwrap_or_default()
        {
            if key
                .strip_prefix('-')
                .is_some_and(|key| key.as_bytes() == digits)
            {
                return value.as_str();
            }
        }
    }
    None
}
fn found<'a>(input: &'a [u8], pass: Pass, lookup: &'a dyn Lookup) -> Option<Replacement<'a>> {
    let (consumed, parts) = match pass {
        Pass::Files => {
            if !uuid(input.get(..36)?) || input.get(36..43)? != b"_files/" {
                return None;
            }
            (43, [&b"_files/"[..], &b""[..], &b""[..]])
        }
        Pass::Picture | Pass::Help => {
            let prefix = if matches!(pass, Pass::Picture) {
                &b"../../mdpicture/id"[..]
            } else {
                &b"../id"[..]
            };
            let body = input.strip_prefix(prefix)?;
            let first = body.get(..36)?;
            if !uuid(first) || body.get(36) != Some(&b'/') || !uuid(body.get(37..73)?) {
                return None;
            }
            let consumed = prefix.len().checked_add(73)?;
            let parts = if let Some(name) = named(lookup, first) {
                [
                    name.as_bytes(),
                    if matches!(pass, Pass::Help) {
                        &b"/Help"[..]
                    } else {
                        &b""[..]
                    },
                    &b""[..],
                ]
            } else if matches!(pass, Pass::Picture) {
                match context::merged("std_pictures", std::str::from_utf8(first).ok()?) {
                    Some(name) => [&b"StdPicture."[..], name.as_bytes(), &b""[..]],
                    None => [input.get(..consumed)?, &b""[..], &b""[..]],
                }
            } else {
                [input.get(..consumed)?, &b""[..], &b""[..]]
            };
            (consumed, parts)
        }
        Pass::Standard => {
            let prefix = b"../../mdpicture/idn-";
            let body = input.strip_prefix(prefix)?;
            let length = body.iter().take_while(|b| b.is_ascii_digit()).count();
            if length == 0 {
                return None;
            }
            let consumed = prefix.len().checked_add(length)?;
            let parts = match standard_digits(body.get(..length)?) {
                Some(name) => [&b"StdPicture."[..], name.as_bytes(), &b""[..]],
                None => [input.get(..consumed)?, &b""[..], &b""[..]],
            };
            (consumed, parts)
        }
        Pass::Lines => {
            if !input.starts_with(b"\r\n") {
                return None;
            }
            (2, [&b"\n"[..], &b""[..], &b""[..]])
        }
    };
    Some(Replacement { consumed, parts })
}
fn pass(
    allocation: Allocation<'_>,
    input: Owned<Vec<u8>>,
    mode: Pass,
    lookup: &dyn Lookup,
) -> Result<Owned<Vec<u8>>, BudgetError> {
    let mut length = 0usize;
    let mut at = 0usize;
    let mut changed = false;
    while let Some(tail) = input.get(at..).filter(|tail| !tail.is_empty()) {
        if let Some(replacement) = found(tail, mode, lookup) {
            let count = replacement.parts.iter().try_fold(0usize, |n, p| {
                n.checked_add(p.len()).ok_or(BudgetError::Overflow)
            })?;
            length = length.checked_add(count).ok_or(BudgetError::Overflow)?;
            changed |= count != replacement.consumed
                || !replacement.parts.iter().flat_map(|p| p.iter()).eq(tail
                    .get(..replacement.consumed)
                    .ok_or(BudgetError::Overflow)?
                    .iter());
            at = at
                .checked_add(replacement.consumed)
                .ok_or(BudgetError::Overflow)?;
        } else {
            length = length.checked_add(1).ok_or(BudgetError::Overflow)?;
            at = at.checked_add(1).ok_or(BudgetError::Overflow)?;
        }
    }
    if !changed {
        return Ok(input);
    }
    let mut output = allocation.vector(length)?;
    at = 0;
    while let Some(tail) = input.get(at..).filter(|tail| !tail.is_empty()) {
        if let Some(replacement) = found(tail, mode, lookup) {
            for part in replacement.parts {
                output.extend(part)?;
            }
            at = at
                .checked_add(replacement.consumed)
                .ok_or(BudgetError::Overflow)?;
        } else {
            output.push(*tail.first().ok_or(BudgetError::Overflow)?)?;
            at = at.checked_add(1).ok_or(BudgetError::Overflow)?;
        }
    }
    Ok(output.finish())
}
pub(crate) fn rewrite(
    allocation: Allocation<'_>,
    mut input: Owned<Vec<u8>>,
    lookup: &dyn Lookup,
) -> Result<Owned<Vec<u8>>, BudgetError> {
    for mode in [
        Pass::Files,
        Pass::Picture,
        Pass::Standard,
        Pass::Help,
        Pass::Lines,
    ] {
        input = pass(allocation, input, mode, lookup)?;
    }
    Ok(input)
}
