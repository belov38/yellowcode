use crate::codec::{self, Encoding};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlMode {
    Url,
    UrlInUrl,
}

fn keep(b: u8, mode: UrlMode) -> bool {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
        return true;
    }
    mode == UrlMode::UrlInUrl && b"/?&=:#@!$'()*+,;[]".contains(&b)
}

pub fn url_encode(s: &[u16], mode: UrlMode) -> String {
    const D: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len() * 3);
    let mut buf = [0u8; 4];
    for &u in s {
        let bytes: &[u8] = match char::from_u32(u32::from(u)) {
            Some(c) => c.encode_utf8(&mut buf).as_bytes(),
            None => b"%3F",
        };
        if bytes == b"%3F" {
            out.push_str("%3F");
            continue;
        }
        for &b in bytes {
            if keep(b, mode) {
                out.push(b as char);
            } else {
                out.push('%');
                out.push(D[(b >> 4) as usize] as char);
                out.push(D[(b & 15) as usize] as char);
            }
        }
    }
    out
}

fn hexv(u: u16) -> Option<u8> {
    match u {
        0x30..=0x39 => Some((u - 0x30) as u8),
        0x41..=0x46 => Some((u - 0x41 + 10) as u8),
        0x61..=0x66 => Some((u - 0x61 + 10) as u8),
        _ => None,
    }
}

pub fn url_decode(s: &[u16], _mode: UrlMode) -> Vec<u16> {
    let mut out = Vec::with_capacity(s.len());
    let mut run: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] == 0x25
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hexv(s[i + 1]), hexv(s[i + 2]))
        {
            run.push(h << 4 | l);
            i += 3;
            continue;
        }
        if !run.is_empty() {
            out.extend(codec::decode(&run, Encoding::Utf8));
            run.clear();
        }
        out.push(s[i]);
        i += 1;
    }
    if !run.is_empty() {
        out.extend(codec::decode(&run, Encoding::Utf8));
    }
    out
}
