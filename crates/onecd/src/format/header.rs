use super::bytes::{self, OrCorrupt};
use super::{Format, PAGE_SIZES_838};
use crate::error::{Error, Location, Result, Unsupported};

const SIGNATURE: &[u8; 8] = b"1CDBMSV8";
const HEADER_LEN: usize = 0x18;
const VERSION_838: [u8; 4] = [8, 3, 8, 0];
const VERSIONS_LEGACY: [[u8; 4]; 6] = [
    [8, 0, 0, 0],
    [8, 0, 3, 0],
    [8, 0, 5, 0],
    [8, 1, 0, 0],
    [8, 2, 0, 0],
    [8, 2, 14, 0],
];
const PAGE_SIZE_LEGACY: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbHeader {
    pub version: [u8; 4],
    pub format: Format,
    pub length_pages: u32,
    pub freelist_page: u32,
    pub page_size: u32,
}

fn format_of_version(version: [u8; 4]) -> Option<Format> {
    if version == VERSION_838 {
        Some(Format::V838)
    } else if VERSIONS_LEGACY.contains(&version) {
        Some(Format::V8214)
    } else {
        None
    }
}

fn header_corrupt(what: &str) -> Error {
    Error::corrupt(Location::Page(0), what)
}

pub fn parse_header(b: &[u8]) -> Result<DbHeader> {
    let page0 = || Location::Page(0);
    let head = bytes::sub(b, 0, HEADER_LEN).or_corrupt(page0, "file header is truncated")?;
    if !head.starts_with(SIGNATURE) {
        return Err(header_corrupt("bad file signature"));
    }
    let version: [u8; 4] = bytes::array(head, 8).or_corrupt(page0, "file header is truncated")?;
    let format = format_of_version(version)
        .ok_or(Error::Unsupported(Unsupported::FormatVersion(version)))?;
    let length_pages = bytes::u32le(head, 0x0C).or_corrupt(page0, "file header is truncated")?;
    let freelist_page = bytes::u32le(head, 0x10).or_corrupt(page0, "file header is truncated")?;
    let page_size = match format {
        Format::V8214 => PAGE_SIZE_LEGACY,
        Format::V838 => {
            let ps = bytes::u32le(head, 0x14).or_corrupt(page0, "file header is truncated")?;
            if !PAGE_SIZES_838.contains(&ps) {
                return Err(header_corrupt(&format!("invalid page size {ps}")));
            }
            ps
        }
    };
    Ok(DbHeader {
        version,
        format,
        length_pages,
        freelist_page,
        page_size,
    })
}

pub fn encode_header(h: &DbHeader, page: &mut [u8]) -> Result<()> {
    if h.format != Format::V838 {
        return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
    }
    if format_of_version(h.version) != Some(Format::V838) {
        return Err(header_corrupt("version does not match the 8.3.8 format"));
    }
    if !PAGE_SIZES_838.contains(&h.page_size) {
        return Err(header_corrupt(&format!(
            "invalid page size {}",
            h.page_size
        )));
    }
    let too_small = || header_corrupt("page buffer is smaller than the file header");
    let (head, tail) = page
        .split_at_mut_checked(HEADER_LEN)
        .ok_or_else(too_small)?;
    tail.fill(0);
    head.fill(0);
    let written = (|| {
        head.get_mut(..8)?.copy_from_slice(SIGNATURE);
        head.get_mut(8..12)?.copy_from_slice(&h.version);
        bytes::put_u32le(head, 0x0C, h.length_pages)?;
        bytes::put_u32le(head, 0x10, h.freelist_page)?;
        bytes::put_u32le(head, 0x14, h.page_size)
    })();
    written.ok_or_else(too_small)
}
