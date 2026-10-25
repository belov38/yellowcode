pub fn is_space(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0A | 0x0D | 0x0C | 0x0B | 0xA0 | 0x3000)
}

pub fn trim_left(s: &[u16]) -> &[u16] {
    let n = s.iter().take_while(|&&c| is_space(c)).count();
    s.get(n..).unwrap_or(&[])
}

pub fn trim_right(s: &[u16]) -> &[u16] {
    let n = s.iter().rev().take_while(|&&c| is_space(c)).count();
    s.get(..s.len() - n).unwrap_or(&[])
}

pub fn trim(s: &[u16]) -> &[u16] {
    trim_right(trim_left(s))
}

pub fn is_blank(s: &[u16]) -> bool {
    s.iter().all(|&c| is_space(c))
}
