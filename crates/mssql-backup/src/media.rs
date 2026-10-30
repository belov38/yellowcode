use std::io::{self, Read, Seek, SeekFrom};

const BLOCK_HEADER: u64 = 52;
const STREAM_HEADER: u64 = 22;
const PAGE: u64 = 8192;
const MAX_BLOCKS: usize = 1_000_000;
const MAX_STREAMS_PER_BLOCK: usize = 1_000_000;
pub const MAX_RETAINED_REGIONS: usize = 65_536;
pub const MAX_RETAINED_REGION_BYTES: usize = 1_048_576;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backup media I/O: {0}")]
    Io(#[from] io::Error),
    #[error("invalid backup media: {0}")]
    Invalid(String),
    #[error("unsupported backup media: {0}")]
    Unsupported(String),
    #[error("backup media resource limit: {0}")]
    ResourceLimit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRegion {
    pub offset: u64,
    pub pages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    pub file_bytes: u64,
    pub regions: Vec<DataRegion>,
    pub log_bytes: u64,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b).ok_or_else(|| invalid("offset overflow"))
}

fn read_at<const N: usize>(
    reader: &mut (impl Read + Seek),
    pos: u64,
    size: u64,
) -> Result<[u8; N], Error> {
    if add(pos, N as u64)? > size {
        return Err(invalid(format!("truncated at offset {pos}")));
    }
    reader.seek(SeekFrom::Start(pos))?;
    let mut buf = [0; N];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn bytes_at<const N: usize>(buf: &[u8], i: usize) -> Result<[u8; N], Error> {
    let end = i
        .checked_add(N)
        .ok_or_else(|| invalid("header offset overflow"))?;
    buf.get(i..end)
        .ok_or_else(|| invalid("short header"))?
        .try_into()
        .map_err(|_| invalid("short header"))
}

fn u16_at(buf: &[u8], i: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(bytes_at(buf, i)?))
}

fn u64_at(buf: &[u8], i: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(bytes_at(buf, i)?))
}

fn retain_region(regions: &mut Vec<DataRegion>, region: DataRegion) -> Result<(), Error> {
    let limit = MAX_RETAINED_REGIONS.min(MAX_RETAINED_REGION_BYTES / size_of::<DataRegion>());
    if regions.len() >= limit {
        return Err(Error::ResourceLimit(format!(
            "retained region budget exceeded ({MAX_RETAINED_REGIONS} regions / {MAX_RETAINED_REGION_BYTES} bytes)"
        )));
    }
    if regions.len() == regions.capacity() {
        let capacity = regions.capacity().max(16).saturating_mul(2).min(limit);
        regions
            .try_reserve_exact(capacity - regions.len())
            .map_err(|error| {
                Error::ResourceLimit(format!("cannot reserve retained region budget: {error}"))
            })?;
        if regions.capacity() > limit {
            return Err(Error::ResourceLimit(
                "allocator exceeded retained region budget".into(),
            ));
        }
    }
    regions.push(region);
    Ok(())
}

pub fn inspect(reader: &mut (impl Read + Seek)) -> Result<MediaInfo, Error> {
    let size = reader.seek(SeekFrom::End(0))?;
    let mut result = MediaInfo {
        file_bytes: size,
        regions: Vec::new(),
        log_bytes: 0,
    };
    let mut pos = 0;
    for block_no in 0..MAX_BLOCKS {
        let h = read_at::<60>(reader, pos, size)?;
        let tag = bytes_at::<4>(&h, 0)?;
        if block_no == 0 && &tag != b"TAPE" {
            return Err(invalid("missing TAPE signature"));
        }
        match &tag {
            b"ESET" => {
                if result.regions.is_empty() {
                    return Err(invalid("no SQL data stream"));
                }
                return Ok(result);
            }
            b"EOTM" => {
                return Err(Error::Unsupported(
                    "incomplete or multi-volume media".into(),
                ));
            }
            b"TAPE" | b"SSET" | b"VOLB" | b"SFMB" | b"MSCI" | b"MSDA" | b"MSTL" | b"MSLS" => {}
            _ => {
                return Err(Error::Unsupported(format!(
                    "unknown descriptor at offset {pos}"
                )));
            }
        }
        let first = u64::from(u16_at(&h, 8)?);
        if first < BLOCK_HEADER {
            return Err(invalid(format!("invalid first stream offset at {pos}")));
        }
        if &tag == b"SFMB" {
            pos = add(pos, first)?;
            if pos >= size {
                return Err(invalid("truncated after filemark"));
            }
            continue;
        }
        let mut stream_pos = add(pos, first)?;
        let mut next = None;
        for _ in 0..MAX_STREAMS_PER_BLOCK {
            let sh = read_at::<22>(reader, stream_pos, size)?;
            let id = bytes_at::<4>(&sh, 0)?;
            if !id.iter().all(u8::is_ascii_graphic) {
                return Err(invalid(format!("invalid stream ID at offset {stream_pos}")));
            }
            let length = u64_at(&sh, 8)?;
            let encryption = u16_at(&sh, 16)?;
            let compression = u16_at(&sh, 18)?;
            if encryption != 0 || compression != 0 {
                return Err(Error::Unsupported(format!(
                    "encrypted or compressed stream at offset {stream_pos}"
                )));
            }
            let data_pos = add(stream_pos, STREAM_HEADER)?;
            let end = add(data_pos, length)?;
            if end > size {
                return Err(invalid(format!("stream past end at {stream_pos}")));
            }
            if (&id == b"MQDA" || &id == b"MQTL") && length != 0 {
                if length < 2 || !(length - 2).is_multiple_of(PAGE) {
                    return Err(invalid(format!(
                        "invalid SQL page stream length at {stream_pos}"
                    )));
                }
                let prefix = read_at::<2>(reader, data_pos, size)?;
                if prefix != [0, 0] {
                    return Err(Error::Unsupported(format!(
                        "unknown SQL stream prefix at {data_pos}"
                    )));
                }
                let bytes = length - 2;
                if &id == b"MQDA" {
                    if &tag != b"MSDA" {
                        return Err(invalid("data stream outside MSDA"));
                    }
                    retain_region(
                        &mut result.regions,
                        DataRegion {
                            offset: add(data_pos, 2)?,
                            pages: bytes / PAGE,
                        },
                    )?;
                } else {
                    if &tag != b"MSTL" {
                        return Err(invalid("log stream outside MSTL"));
                    }
                    result.log_bytes = add(result.log_bytes, bytes)?;
                }
            }
            if &id == b"SPAD" {
                next = Some(end);
                break;
            }
            stream_pos = add(end, 3)? & !3;
        }
        pos = next.ok_or_else(|| invalid("missing padding stream or stream limit exceeded"))?;
        if pos >= size {
            return Err(invalid("missing backup terminator"));
        }
    }
    Err(invalid("descriptor limit exceeded"))
}
