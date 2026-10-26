use crate::{BslDate, Error};
use brace::Node;

pub const MAX_DATA_BYTES: usize = 1_048_576;
const MAX_DEPTH: usize = 32;
pub(crate) const MAX_COLLECTION: usize = 4096;

pub(crate) fn bound_text(text: &str) -> Result<(), Error> {
    if text.len() > MAX_DATA_BYTES {
        return Err(Error::Limit);
    }
    let mut depth = 0usize;
    let mut quoted = false;
    for b in text.bytes() {
        match b {
            b'"' => quoted = !quoted,
            b'{' if !quoted => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Error::Limit);
                }
            }
            b'}' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn parse_date(s: &str) -> Option<BslDate> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |a, b| s.get(a..b)?.parse::<u32>().ok();
    BslDate::from_parts(
        n(0, 4)?,
        n(4, 6)?,
        n(6, 8)?,
        n(8, 10)?,
        n(10, 12)?,
        n(12, 14)?,
    )
}

pub(crate) struct Layout {
    positions: [Option<usize>; 38],
}

impl Layout {
    pub(crate) fn at<'a>(&self, fields: &'a [Node], logical: usize) -> Option<&'a Node> {
        fields.get((*self.positions.get(logical)?)?)
    }

    pub(crate) fn read(fields: &[Node]) -> Result<Self, Error> {
        let mut out = Self {
            positions: [None; 38],
        };
        let mut actual = 0;
        for (group, mandatory) in [
            ((0, 5), true),
            ((6, 6), false),
            ((7, 9), false),
            ((10, 10), false),
            ((11, 11), false),
            ((12, 13), false),
            ((14, 14), false),
            ((15, 15), false),
            ((16, 16), false),
            ((17, 17), false),
            ((18, 18), false),
            ((19, 19), false),
            ((20, 20), false),
            ((21, 22), false),
            ((23, 23), false),
            ((24, 25), false),
            ((26, 26), false),
            ((27, 27), false),
            ((28, 30), false),
            ((32, 33), false),
            ((34, 34), false),
            ((35, 35), false),
            ((36, 36), false),
            ((37, 37), false),
        ] {
            if actual == fields.len() && !mandatory {
                break;
            }
            for logical in group.0..=group.1 {
                let node = fields.get(actual).ok_or(Error::BadField(logical))?;
                check(logical, node)?;
                if let Some(slot) = out.positions.get_mut(logical) {
                    *slot = Some(actual);
                }
                actual += 1;
                if matches!(logical, 17 | 27 | 30) {
                    let count = node.as_u32().ok_or(Error::BadField(logical))? as usize;
                    if count > MAX_COLLECTION || (logical == 30 && count > 99) {
                        return Err(Error::BadField(logical));
                    }
                    let width = if logical == 27 { 2 } else { 1 };
                    let size = count.checked_mul(width).ok_or(Error::BadField(logical))?;
                    let end = actual.checked_add(size).ok_or(Error::BadField(logical))?;
                    let items = fields.get(actual..end).ok_or(Error::BadField(logical))?;
                    for item in items {
                        let valid = if logical == 17 {
                            item.as_list().is_some_and(|pair| {
                                pair.len() == 2 && pair.iter().all(|n| n.as_str().is_some())
                            })
                        } else {
                            item.as_str().is_some()
                        };
                        if !valid {
                            return Err(Error::BadField(logical));
                        }
                    }
                    if logical == 30
                        && count != 0
                        && let Some(slot) = out.positions.get_mut(31)
                    {
                        *slot = Some(actual);
                    }
                    actual = end;
                }
            }
        }
        if actual != fields.len() {
            return Err(Error::FieldCount(fields.len()));
        }
        Ok(out)
    }
}

fn check(i: usize, n: &Node) -> Result<(), Error> {
    let valid = match i {
        0 => n.as_guid().is_some(),
        4 | 6 => n.as_guid().is_some() || n.text() == Some(""),
        1 | 2 | 3 | 12 | 13 | 25 | 28 | 33 | 37 => n.as_str().is_some(),
        5 => counted_guids(n, 1),
        19 => counted_guids(n, 2),
        7 | 8 | 10 | 11 | 18 | 20 | 23 | 24 | 26 | 34 | 35 | 36 => n.as_bool().is_some(),
        9 => n
            .text()
            .is_some_and(|s| s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit())),
        14 => n.as_u32().is_some_and(|v| v <= 2),
        15 | 17 | 22 | 27 | 30 | 32 => n.as_u32().is_some(),
        16 | 29 => n.as_atom().and_then(parse_date).is_some(),
        21 => n.is_list(),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::BadField(i))
    }
}

fn counted_guids(n: &Node, width: usize) -> bool {
    let Some((count, values)) = n.as_list().and_then(|n| n.split_first()) else {
        return false;
    };
    count
        .as_u32()
        .filter(|n| *n as usize <= MAX_COLLECTION)
        .and_then(|n| (n as usize).checked_mul(width))
        == Some(values.len())
        && values.iter().all(|n| n.as_guid().is_some())
}
