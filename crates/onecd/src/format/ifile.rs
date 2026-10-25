use super::bytes::{self, OrCorrupt};
use super::{Format, FormatInfo, PAGE_SIZES_838};
use crate::error::{Error, Location, Result};

const SIG_838: [u8; 2] = [0x1C, 0xFD];
const SIG_8214: &[u8; 8] = b"1CDBOBV8";
const SLOTS_OFFSET: usize = 24;
const ALLOC_ENTRIES_8214: usize = 1023;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IFileHeader {
    V838 {
        fatlevel: u16,
        versions: [u32; 3],
        length: u64,
        slots: Vec<u32>,
    },
    V8214 {
        length: u32,
        versions: [u32; 3],
        slots: Vec<u32>,
    },
}

pub fn parse_ifile_header(fi: FormatInfo, page: &[u8], page_no: u32) -> Result<IFileHeader> {
    let loc = || Location::InternalFile(page_no);
    let trunc = "file header page is truncated";
    let nslots = fi.file_header_slots() as usize;
    match fi.format {
        Format::V838 => {
            if page.get(..2) != Some(&SIG_838[..]) {
                return Err(Error::corrupt(loc(), "bad internal file signature"));
            }
            let fatlevel = bytes::u16le(page, 2).or_corrupt(loc, trunc)?;
            let versions = [4, 8, 12].map(|o| bytes::u32le(page, o));
            let [Some(v1), Some(v2), Some(v3)] = versions else {
                return Err(Error::corrupt(loc(), trunc));
            };
            let length = bytes::u64le(page, 16).or_corrupt(loc, trunc)?;
            let slots = bytes::u32s_le(page, SLOTS_OFFSET, nslots).or_corrupt(loc, trunc)?;
            Ok(IFileHeader::V838 {
                fatlevel,
                versions: [v1, v2, v3],
                length,
                slots,
            })
        }
        Format::V8214 => {
            if page.get(..8) != Some(&SIG_8214[..]) {
                return Err(Error::corrupt(loc(), "bad internal file signature"));
            }
            let length = bytes::u32le(page, 8).or_corrupt(loc, trunc)?;
            let versions = [12, 16, 20].map(|o| bytes::u32le(page, o));
            let [Some(v1), Some(v2), Some(v3)] = versions else {
                return Err(Error::corrupt(loc(), trunc));
            };
            let slots = bytes::u32s_le(page, SLOTS_OFFSET, nslots).or_corrupt(loc, trunc)?;
            Ok(IFileHeader::V8214 {
                length,
                versions: [v1, v2, v3],
                slots,
            })
        }
    }
}

pub fn encode_ifile_header_838(
    fatlevel: u16,
    versions: [u32; 3],
    length: u64,
    slots: &[u32],
    page: &mut [u8],
) -> Result<()> {
    let bad = |what: &str| Error::corrupt(Location::File, what);
    if fatlevel > 1 {
        return Err(bad("unknown fatlevel"));
    }
    if !u32::try_from(page.len()).is_ok_and(|n| PAGE_SIZES_838.contains(&n)) {
        return Err(bad("page buffer is not a valid 8.3.8 page size"));
    }
    if slots.len() > (page.len() / 4).saturating_sub(6) {
        return Err(bad("too many slots for the file header page"));
    }
    page.fill(0);
    let written = (|| {
        page.get_mut(..2)?.copy_from_slice(&SIG_838);
        bytes::put_u16le(page, 2, fatlevel)?;
        for (i, v) in versions.iter().enumerate() {
            bytes::put_u32le(page, 4 + 4 * i, *v)?;
        }
        bytes::put_u64le(page, 16, length)?;
        for (i, s) in slots.iter().enumerate() {
            bytes::put_u32le(page, SLOTS_OFFSET + 4 * i, *s)?;
        }
        Some(())
    })();
    written.ok_or_else(|| bad("page buffer is too small for the file header"))
}

const VERSIONS_838: [usize; 3] = [4, 8, 12];

fn check_sig_838(page: &[u8], page_no: u32) -> Result<()> {
    if page.get(..2) != Some(&SIG_838[..]) {
        return Err(Error::corrupt(
            Location::InternalFile(page_no),
            "bad internal file signature",
        ));
    }
    Ok(())
}

pub fn ifile_versions_838(page: &[u8], page_no: u32) -> Result<[u32; 3]> {
    check_sig_838(page, page_no)?;
    let [Some(v1), Some(v2), Some(v3)] = VERSIONS_838.map(|o| bytes::u32le(page, o)) else {
        return Err(Error::corrupt(
            Location::InternalFile(page_no),
            "file header page is truncated",
        ));
    };
    Ok([v1, v2, v3])
}

pub fn put_ifile_versions_838(page: &mut [u8], page_no: u32, versions: [u32; 3]) -> Result<()> {
    check_sig_838(page, page_no)?;
    for (o, v) in VERSIONS_838.iter().zip(versions) {
        bytes::put_u32le(page, *o, v).or_corrupt(
            || Location::InternalFile(page_no),
            "file header page is truncated",
        )?;
    }
    Ok(())
}

pub fn parse_alloc_page(fi: FormatInfo, page: &[u8], page_no: u32) -> Result<Vec<u32>> {
    let loc = || Location::Page(page_no);
    let trunc = "allocation page is truncated";
    match fi.format {
        Format::V838 => bytes::u32s_le(page, 0, fi.alloc_entries() as usize).or_corrupt(loc, trunc),
        Format::V8214 => {
            let raw = bytes::array::<4>(page, 0).or_corrupt(loc, trunc)?;
            let count = usize::try_from(i32::from_le_bytes(raw))
                .ok()
                .filter(|&c| c <= ALLOC_ENTRIES_8214)
                .ok_or_else(|| Error::corrupt(loc(), "allocation page count is out of range"))?;
            bytes::u32s_le(page, 4, count).or_corrupt(loc, trunc)
        }
    }
}
