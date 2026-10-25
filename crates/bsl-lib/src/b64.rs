use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub fn base64_string(b: &[u8]) -> String {
    let plain = STANDARD.encode(b);
    let mut out = String::with_capacity(plain.len() + plain.len() / 32);
    let mut n = 0usize;
    for c in plain.chars() {
        if n == 64 {
            out.push_str("\r\n");
            n = 0;
        }
        out.push(c);
        n += 1;
    }
    out
}

pub fn base64_plain(b: &[u8]) -> String {
    STANDARD.encode(b)
}

fn sextet(u: u16) -> Option<u8> {
    match u {
        0x41..=0x5A => Some((u - 0x41) as u8),
        0x61..=0x7A => Some((u - 0x61 + 26) as u8),
        0x30..=0x39 => Some((u - 0x30 + 52) as u8),
        0x2B => Some(62),
        0x2F => Some(63),
        _ => None,
    }
}

pub fn base64_value(s: &[u16]) -> Option<Vec<u8>> {
    let mut sym: Vec<u16> = Vec::with_capacity(s.len());
    for &u in s {
        if matches!(u, 0x20 | 0x09 | 0x0A | 0x0D) {
            continue;
        }
        sym.push(u);
    }
    let pad = sym.iter().rev().take_while(|&&u| u == 0x3D).count();
    let body = &sym[..sym.len() - pad];
    if pad > 2 || body.iter().any(|&u| sextet(u).is_none()) {
        return Some(Vec::new());
    }
    if !sym.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    for chunk in body.chunks(4) {
        let mut acc = 0u32;
        for &u in chunk {
            acc = (acc << 6) | u32::from(sextet(u).unwrap_or(0));
        }
        acc <<= 6 * (4 - chunk.len()) as u32;
        let bytes = acc.to_be_bytes();
        let n = chunk.len() * 6 / 8;
        out.extend_from_slice(&bytes[1..1 + n]);
    }
    Some(out)
}
