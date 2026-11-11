pub(crate) fn slice(bytes: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(len)?)
}

fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    slice(bytes, at, N)?.try_into().ok()
}

pub(crate) fn u8_at(bytes: &[u8], at: usize) -> Option<u8> {
    bytes.get(at).copied()
}

pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    array(bytes, at).map(u16::from_le_bytes)
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    array(bytes, at).map(u32::from_le_bytes)
}

pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    array(bytes, at).map(u64::from_le_bytes)
}

pub(crate) fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    array(bytes, at).map(i64::from_le_bytes)
}

pub(crate) fn i16_at(bytes: &[u8], at: usize) -> Option<i16> {
    array(bytes, at).map(i16::from_le_bytes)
}

pub(crate) fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    array(bytes, at).map(i32::from_le_bytes)
}
