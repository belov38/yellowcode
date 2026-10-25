use super::bytes::{self, OrCorrupt};
use super::{Format, FormatInfo, PAGE_SIZES_838};
use crate::error::{Error, Location, Result};

pub const FREELIST_PAGE: u32 = 1;

const SIG_838: [u8; 2] = [0x1C, 0xFF];
const SIG_8214: &[u8; 8] = b"1CDBOBV8";
const SLOTS_OFFSET_838: usize = 12;
const SLOTS_OFFSET_8214: usize = 24;
const LIST_ENTRIES_8214: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeListHeader {
    pub count: u32,
    pub counter: u32,
    pub slots: Vec<u32>,
}

fn corrupt(what: &str) -> Error {
    Error::corrupt(Location::Page(FREELIST_PAGE), what)
}

pub fn entries_per_list_page(fi: FormatInfo) -> u32 {
    match fi.format {
        Format::V838 => fi.page_size / 4,
        Format::V8214 => LIST_ENTRIES_8214,
    }
}

fn header_fields(fi: FormatInfo, page: &[u8]) -> Result<(u32, u32, usize)> {
    let trunc = "free-list header page is truncated";
    let loc = || Location::Page(FREELIST_PAGE);
    let (count, counter, slots_off) = match fi.format {
        Format::V838 => {
            if page.get(..2) != Some(&SIG_838[..]) {
                return Err(corrupt("bad free-list signature"));
            }
            let count = bytes::u32le(page, 4).or_corrupt(loc, trunc)?;
            let counter = bytes::u32le(page, 8).or_corrupt(loc, trunc)?;
            (count, counter, SLOTS_OFFSET_838)
        }
        Format::V8214 => {
            if page.get(..8) != Some(&SIG_8214[..]) {
                return Err(corrupt("bad free-list signature"));
            }
            let count = bytes::u32le(page, 8).or_corrupt(loc, trunc)?;
            let counter = bytes::u32le(page, 12).or_corrupt(loc, trunc)?;
            (count, counter, SLOTS_OFFSET_8214)
        }
    };
    Ok((count, counter, slots_off))
}
pub(crate) fn parse_freelist_count(fi: FormatInfo, page: &[u8]) -> Result<u32> {
    let (count, _, offset) = header_fields(fi, page)?;
    let n = (fi.freelist_slots() as usize)
        .checked_mul(4)
        .ok_or_else(|| corrupt("free-list slots overflow"))?;
    bytes::sub(page, offset, n).or_corrupt(
        || Location::Page(FREELIST_PAGE),
        "free-list header page is truncated",
    )?;
    Ok(count)
}
pub fn parse_freelist_header(fi: FormatInfo, page: &[u8]) -> Result<FreeListHeader> {
    let (count, counter, slots_off) = header_fields(fi, page)?;
    let loc = || Location::Page(FREELIST_PAGE);
    let trunc = "free-list header page is truncated";
    let slots =
        bytes::u32s_le(page, slots_off, fi.freelist_slots() as usize).or_corrupt(loc, trunc)?;
    Ok(FreeListHeader {
        count,
        counter,
        slots,
    })
}

pub fn encode_freelist_header_838(h: &FreeListHeader, page: &mut [u8]) -> Result<()> {
    if !u32::try_from(page.len()).is_ok_and(|n| PAGE_SIZES_838.contains(&n)) {
        return Err(corrupt("page buffer is not a valid 8.3.8 page size"));
    }
    if h.slots.len() > page.len().saturating_sub(SLOTS_OFFSET_838) / 4 {
        return Err(corrupt("too many slots for the free-list header page"));
    }
    page.fill(0);
    let written = (|| {
        page.get_mut(..2)?.copy_from_slice(&SIG_838);
        bytes::put_u32le(page, 4, h.count)?;
        bytes::put_u32le(page, 8, h.counter)?;
        for (i, s) in h.slots.iter().enumerate() {
            bytes::put_u32le(page, SLOTS_OFFSET_838 + 4 * i, *s)?;
        }
        Some(())
    })();
    written.ok_or_else(|| corrupt("page buffer is too small for the free-list header"))
}
