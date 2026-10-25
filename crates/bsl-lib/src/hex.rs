use crate::error::{LibError, msg};

const DIGITS: &[u8; 16] = b"0123456789ABCDEF";

pub fn hex_string(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(DIGITS[(x >> 4) as usize] as char);
        s.push(DIGITS[(x & 15) as usize] as char);
    }
    s
}

fn digit(u: u16) -> Option<u8> {
    match u {
        0x30..=0x39 => Some((u - 0x30) as u8),
        0x41..=0x46 => Some((u - 0x41 + 10) as u8),
        0x61..=0x66 => Some((u - 0x61 + 10) as u8),
        _ => None,
    }
}

pub fn hex_bytes(s: &[u16]) -> Result<Vec<u8>, LibError> {
    let bad = || LibError::new(msg::HEX_FORMAT);
    if !s.len().is_multiple_of(2) {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for p in s.as_chunks::<2>().0.iter() {
        let hi = digit(p[0]).ok_or_else(bad)?;
        let lo = digit(p[1]).ok_or_else(bad)?;
        out.push(hi << 4 | lo);
    }
    Ok(out)
}

pub fn hex_spaced(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 3);
    for (i, &x) in b.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push(DIGITS[(x >> 4) as usize] as char);
        s.push(DIGITS[(x & 15) as usize] as char);
    }
    s
}
