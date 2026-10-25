pub const UNDEFINED: i32 = 0x7FFF_FFFF;
pub const NULL: i32 = 0x7FFF_FFFE;
pub const TRUE: i32 = 0x7FFF_FFFD;
pub const FALSE: i32 = 0x7FFF_FFFC;

pub fn string(units: &[u16]) -> i32 {
    units
        .iter()
        .fold(0i32, |h, &u| h.wrapping_mul(37).wrapping_add(u as i32))
}

pub fn uuid(bytes: &[u8; 16]) -> i32 {
    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub fn bytes(b: &[u8]) -> i32 {
    b.iter().fold(0i32, |h, &x| h.wrapping_add(x as i8 as i32))
}

pub fn ordinal(n: u32) -> i32 {
    n as i32
}

pub fn mix64(key: u64) -> u64 {
    let mut k = (!key).wrapping_add(key << 21);
    k ^= k >> 24;
    k = k.wrapping_add(k << 3).wrapping_add(k << 8);
    k ^= k >> 14;
    k = k.wrapping_add(k << 2).wrapping_add(k << 4);
    k ^= k >> 28;
    k.wrapping_add(k << 31)
}
