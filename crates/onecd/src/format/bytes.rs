use crate::error::{Error, Location, Result};

pub(crate) fn sub(b: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    b.get(off..off.checked_add(len)?)
}

pub(crate) fn array<const N: usize>(b: &[u8], off: usize) -> Option<[u8; N]> {
    sub(b, off, N)?.try_into().ok()
}

fn put<const N: usize>(b: &mut [u8], off: usize, v: [u8; N]) -> Option<()> {
    let dst: &mut [u8; N] = b.get_mut(off..off.checked_add(N)?)?.try_into().ok()?;
    *dst = v;
    Some(())
}

pub(crate) fn u16le(b: &[u8], off: usize) -> Option<u16> {
    array(b, off).map(u16::from_le_bytes)
}

pub(crate) fn u32le(b: &[u8], off: usize) -> Option<u32> {
    array(b, off).map(u32::from_le_bytes)
}

pub(crate) fn u64le(b: &[u8], off: usize) -> Option<u64> {
    array(b, off).map(u64::from_le_bytes)
}

pub(crate) fn u32be(b: &[u8], off: usize) -> Option<u32> {
    array(b, off).map(u32::from_be_bytes)
}

pub(crate) fn u32s_le(b: &[u8], off: usize, n: usize) -> Option<Vec<u32>> {
    let raw = sub(b, off, n.checked_mul(4)?)?;
    Some(
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect(),
    )
}

pub(crate) fn put_u16le(b: &mut [u8], off: usize, v: u16) -> Option<()> {
    put(b, off, v.to_le_bytes())
}

pub(crate) fn put_u32le(b: &mut [u8], off: usize, v: u32) -> Option<()> {
    put(b, off, v.to_le_bytes())
}

pub(crate) fn put_u64le(b: &mut [u8], off: usize, v: u64) -> Option<()> {
    put(b, off, v.to_le_bytes())
}

pub(crate) fn put_u32be(b: &mut [u8], off: usize, v: u32) -> Option<()> {
    put(b, off, v.to_be_bytes())
}

pub(crate) trait OrCorrupt<T> {
    fn or_corrupt(self, loc: impl FnOnce() -> Location, what: &str) -> Result<T>;
    fn or_corrupt_static(self, loc: impl FnOnce() -> Location, what: &'static str) -> Result<T>;
}

impl<T> OrCorrupt<T> for Option<T> {
    fn or_corrupt_static(self, loc: impl FnOnce() -> Location, what: &'static str) -> Result<T> {
        self.ok_or_else(|| Error::CorruptStatic {
            location: loc(),
            what,
        })
    }
    fn or_corrupt(self, loc: impl FnOnce() -> Location, what: &str) -> Result<T> {
        self.ok_or_else(|| Error::corrupt(loc(), what))
    }
}
