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
    #[error("backup media header checksum mismatch: {0}")]
    Checksum(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRegion {
    pub offset: u64,
    pub pages: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeaderChecks {
    pub blocks: u64,
    pub streams: u64,
}

pub const MAX_RETAINED_LOG_REGIONS: usize = 16_384;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn {
    pub vlf: u32,
    pub block: u32,
    pub slot: u16,
}

impl std::fmt::Display for Lsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:08X}:{:08X}:{:04X}", self.vlf, self.block, self.slot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRegion {
    pub offset: u64,
    pub bytes: u64,
    pub checksum: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftwareVersion {
    pub major: u8,
    pub minor: u8,
    pub build: u16,
}

mod flag {
    pub(super) const BULK_LOGGED_DATA: u32 = 0x01;
    pub(super) const READ_ONLY: u32 = 0x04;
    pub(super) const SINGLE_USER: u32 = 0x08;
    pub(super) const CHECKSUMS: u32 = 0x10;
    pub(super) const DAMAGED: u32 = 0x20;
    pub(super) const COPY_ONLY: u32 = 0x400;
    pub(super) const DIFFERENTIAL: u32 = 0x800;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackupHeader {
    pub flags: u32,
    pub software: SoftwareVersion,
    pub first_lsn: Lsn,
    pub last_lsn: Lsn,
    pub checkpoint_lsn: Lsn,
    pub database_backup_lsn: Lsn,
    pub set_attributes: Option<u32>,
}

impl BackupHeader {
    pub fn has_checksums(&self) -> bool {
        self.flags & flag::CHECKSUMS != 0
    }

    pub fn is_damaged(&self) -> bool {
        self.flags & flag::DAMAGED != 0
    }

    pub fn is_differential(&self) -> bool {
        self.flags & flag::DIFFERENTIAL != 0
    }

    pub fn is_copy_only(&self) -> bool {
        self.flags & flag::COPY_ONLY != 0
    }

    pub fn is_read_only(&self) -> bool {
        self.flags & flag::READ_ONLY != 0
    }

    pub fn is_single_user(&self) -> bool {
        self.flags & flag::SINGLE_USER != 0
    }

    pub fn has_bulk_logged_data(&self) -> bool {
        self.flags & flag::BULK_LOGGED_DATA != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    pub file_bytes: u64,
    pub regions: Vec<DataRegion>,
    pub log_bytes: u64,
    pub header_checks: Option<HeaderChecks>,
    pub log_regions: Vec<LogRegion>,
    pub log_regions_complete: bool,
    pub header: Option<BackupHeader>,
    pub checksum_streams: u64,
}

const NO_DATA_STREAM: &str = "no SQL data stream";

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

impl Error {
    pub(crate) fn is_missing_data_stream(&self) -> bool {
        matches!(self, Error::Invalid(message) if message == NO_DATA_STREAM)
    }

    pub(crate) fn is_truncation(&self) -> bool {
        matches!(
            self,
            Error::Invalid(message)
                if message.starts_with("truncated")
                    || message.starts_with("stream past end")
                    || message.starts_with("missing backup terminator")
        )
    }
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

fn u32_at(buf: &[u8], i: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(bytes_at(buf, i)?))
}

fn u64_at(buf: &[u8], i: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(bytes_at(buf, i)?))
}

const MQCI_FACTS: u64 = 72;
const MQCI_MAGIC: &[u8; 4] = b"SCIN";
const MQCI_FLAGS_AT: usize = 8;
const MQCI_VERSION_AT: usize = 24;
const MQCI_LSNS_AT: [usize; 4] = [32, 42, 52, 62];

fn lsn_at(buf: &[u8], i: usize) -> Result<Lsn, Error> {
    Ok(Lsn {
        vlf: u32_at(buf, i)?,
        block: u32_at(buf, i + 4)?,
        slot: u16_at(buf, i + 8)?,
    })
}

fn read_header(
    reader: &mut (impl Read + Seek),
    data_pos: u64,
    length: u64,
    size: u64,
) -> Result<Option<BackupHeader>, Error> {
    if length < MQCI_FACTS {
        return Ok(None);
    }
    let buf = read_at::<{ MQCI_FACTS as usize }>(reader, data_pos, size)?;
    if buf.get(..4) != Some(MQCI_MAGIC.as_slice()) {
        return Ok(None);
    }
    let [build_low, build_high, minor, major] = u32_at(&buf, MQCI_VERSION_AT)?.to_le_bytes();
    let [first, last, checkpoint, database_backup] = MQCI_LSNS_AT;
    Ok(Some(BackupHeader {
        flags: u32_at(&buf, MQCI_FLAGS_AT)?,
        software: SoftwareVersion {
            major,
            minor,
            build: u16::from_le_bytes([build_low, build_high]),
        },
        first_lsn: lsn_at(&buf, first)?,
        last_lsn: lsn_at(&buf, last)?,
        checkpoint_lsn: lsn_at(&buf, checkpoint)?,
        database_backup_lsn: lsn_at(&buf, database_backup)?,
        set_attributes: None,
    }))
}

const BLOCK_CHECKSUM_AT: usize = 50;
const STREAM_CHECKSUM_AT: usize = 20;

fn xor_words(bytes: &[u8]) -> u16 {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0, |sum, word| sum ^ u16::from_le_bytes(*word))
}

fn verify_checksum(header: &[u8], covered: usize, what: &str, pos: u64) -> Result<(), Error> {
    let words = header
        .get(..covered)
        .ok_or_else(|| invalid("short header"))?;
    let stored = u16_at(header, covered)?;
    let computed = xor_words(words);
    if stored == computed {
        Ok(())
    } else {
        Err(Error::Checksum(format!(
            "{what} at offset {pos}: stored {stored:#06x}, computed {computed:#06x}"
        )))
    }
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
    walk(reader, false)
}

pub fn inspect_checked(reader: &mut (impl Read + Seek)) -> Result<MediaInfo, Error> {
    walk(reader, true)
}

fn walk(reader: &mut (impl Read + Seek), verify: bool) -> Result<MediaInfo, Error> {
    let size = reader.seek(SeekFrom::End(0))?;
    let mut result = MediaInfo {
        file_bytes: size,
        regions: Vec::new(),
        log_bytes: 0,
        header_checks: None,
        log_regions: Vec::new(),
        log_regions_complete: true,
        header: None,
        checksum_streams: 0,
    };
    let mut set_attributes = None;
    let mut checks = HeaderChecks::default();
    let mut pos = 0;
    for block_no in 0..MAX_BLOCKS {
        let h = read_at::<60>(reader, pos, size)?;
        let tag = bytes_at::<4>(&h, 0)?;
        if block_no == 0 && &tag != b"TAPE" {
            if h.get(..8) == Some(b"MSSQLBAK".as_slice()) {
                return Err(Error::Unsupported(
                    "compressed or encrypted backup (MSSQLBAK): its structure is not checked without SQL Server; `bak convert` asks the server, accepts compressed copies and refuses encrypted ones"
                        .into(),
                ));
            }
            return Err(invalid(
                "not a SQL Server backup: the file does not start with a TAPE block",
            ));
        }
        if verify {
            verify_checksum(&h, BLOCK_CHECKSUM_AT, "descriptor block header", pos)?;
            checks.blocks += 1;
        }
        match &tag {
            b"ESET" => {
                if result.regions.is_empty() {
                    return Err(invalid(NO_DATA_STREAM));
                }
                result.header_checks = verify.then_some(checks);
                if let Some(header) = result.header.as_mut() {
                    header.set_attributes = set_attributes;
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
        if &tag == b"SSET" && set_attributes.is_none() && first >= BLOCK_HEADER + 4 {
            set_attributes = Some(u32::from_le_bytes(bytes_at::<4>(&h, 52)?));
        }
        let mut stream_pos = add(pos, first)?;
        let mut next = None;
        let mut after_log: Option<usize> = None;
        for _ in 0..MAX_STREAMS_PER_BLOCK {
            let sh = read_at::<22>(reader, stream_pos, size)?;
            if verify {
                verify_checksum(&sh, STREAM_CHECKSUM_AT, "stream header", stream_pos)?;
                checks.streams += 1;
            }
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
                return Err(invalid(format!(
                    "stream past end at {stream_pos}: the file is shorter than the backup it describes (truncated or not fully copied)"
                )));
            }
            let log_before = after_log.take();
            if &id == b"CSUM" {
                result.checksum_streams = add(result.checksum_streams, 1)?;
                if length == 4
                    && let Some(index) = log_before
                {
                    let value = u32::from_le_bytes(read_at::<4>(reader, data_pos, size)?);
                    if let Some(region) = result.log_regions.get_mut(index) {
                        region.checksum = Some(value);
                    }
                }
            }
            if &id == b"MQCI" && &tag == b"MSLS" {
                result.header = read_header(reader, data_pos, length, size)?;
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
                    if result.log_regions.len() < MAX_RETAINED_LOG_REGIONS {
                        after_log = Some(result.log_regions.len());
                        result.log_regions.push(LogRegion {
                            offset: add(data_pos, 2)?,
                            bytes,
                            checksum: None,
                        });
                    } else {
                        result.log_regions_complete = false;
                    }
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
            return Err(invalid(
                "missing backup terminator: the file ends before the backup does (truncated or not fully copied)",
            ));
        }
    }
    Err(invalid("descriptor limit exceeded"))
}
