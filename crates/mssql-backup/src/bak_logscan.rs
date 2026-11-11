use std::io::{self, Read, Seek, SeekFrom};

use crate::{
    bak_bytes::{i16_at, slice, u8_at, u16_at, u32_at},
    media::{LogRegion, Lsn},
};

pub const SECTOR: usize = 512;
pub const MAX_BLOCK_BYTES: usize = 1 << 20;
pub const MAX_WINDOW_BYTES: u64 = 512 << 20;
pub const MAX_RECORDS: u64 = 20_000_000;
const READ_CHUNK: usize = 64 * 1024;
const FIRST_SECTOR: u8 = 0x10;
const LAST_SECTOR: u8 = 0x08;
const PARITY_BITS: u8 = 0xC0;
const FORMAT_2014: u8 = 5;
const HEADER_2014: usize = 48;
const FORMAT_2022: u8 = 6;
const HEADER_2022: usize = 72;
const BLOCK_COUNT_AT: usize = 2;
const BLOCK_USED_AT: usize = 4;
const BLOCK_SEQUENCE_AT: usize = 12;
const BLOCK_OFFSET_AT: usize = 16;
const BLOCK_CHECKSUM_AT: usize = 24;
const RECORD_HEADER: usize = 24;
const PAGE_RECORD_HEADER: usize = 46;
const MIN_EMPTY_BLOCK: usize = 32;
const PAGE_ONLY_HEADER: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_window_bytes: u64,
    pub max_records: u64,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_window_bytes: MAX_WINDOW_BYTES,
        max_records: MAX_RECORDS,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading the log of the backup: {0}")]
    Io(#[from] io::Error),
    #[error("the log of the backup is larger than the direct reader scans: {0}")]
    Limit(&'static str),
    #[error("the log stream at offset {stream} is malformed: {what}")]
    Stream { stream: u64, what: &'static str },
    #[error("the log block at offset {at} of the stream at {stream} is malformed: {what}")]
    Block {
        stream: u64,
        at: u64,
        what: &'static str,
    },
    #[error(
        "the log block at offset {at} of the stream at {stream} fails its checksum (stored {stored:#010x}, computed {computed:#010x})"
    )]
    BlockChecksum {
        stream: u64,
        at: u64,
        stored: u32,
        computed: u32,
    },
    #[error(
        "the log stream at offset {stream} fails its checksum (stored {stored:#010x}, computed {computed:#010x})"
    )]
    StreamChecksum {
        stream: u64,
        stored: u32,
        computed: u32,
    },
    #[error("some blocks of the log carry a checksum and others carry none")]
    MixedChecksums,
    #[error(
        "the log block has format {format}, and only 5 (SQL Server 2014) and 6 (SQL Server 2022) are known"
    )]
    UnknownBlockFormat { format: u8 },
    #[error("the log record {lsn} has the operation byte {byte}, which is not known")]
    UnknownOperation { lsn: Lsn, byte: u8 },
    #[error("the log record {lsn} has the context byte {byte}, which is not known")]
    UnknownContext { lsn: Lsn, byte: u8 },
    #[error("the log record {lsn} is malformed: {what}")]
    Record { lsn: Lsn, what: &'static str },
    #[error("the log window does not fit the backup: {what}")]
    Bounds { what: &'static str },
}

macro_rules! byte_table {
    (
        $(#[$meta:meta])*
        $name:ident, $count:literal, [ $( ($variant:ident, $byte:literal, $text:literal) ),+ $(,)? ]
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $variant, )+
        }

        impl $name {
            pub const ALL: [($name, u8, &str); $count] = [ $( ($name::$variant, $byte, $text), )+ ];

            pub fn from_byte(byte: u8) -> Option<$name> {
                match byte {
                    $( $byte => Some($name::$variant), )+
                    _ => None,
                }
            }

            pub fn byte(self) -> u8 {
                match self {
                    $( $name::$variant => $byte, )+
                }
            }

            pub fn name(self) -> &'static str {
                match self {
                    $( $name::$variant => $text, )+
                }
            }
        }
    };
}

byte_table! {
    Operation, 33, [
        (FormatPage, 1, "LOP_FORMAT_PAGE"),
        (InsertRows, 2, "LOP_INSERT_ROWS"),
        (DeleteRows, 3, "LOP_DELETE_ROWS"),
        (ModifyRow, 4, "LOP_MODIFY_ROW"),
        (ModifyHeader, 5, "LOP_MODIFY_HEADER"),
        (ModifyColumns, 6, "LOP_MODIFY_COLUMNS"),
        (SetBits, 7, "LOP_SET_BITS"),
        (SetFreeSpace, 10, "LOP_SET_FREE_SPACE"),
        (DeleteSplit, 11, "LOP_DELETE_SPLIT"),
        (ExpungeRows, 13, "LOP_EXPUNGE_ROWS"),
        (FileHdrModify, 16, "LOP_FILE_HDR_MODIFY"),
        (ClearGamBits, 17, "LOP_CLEAR_GAM_BITS"),
        (CountDelta, 18, "LOP_COUNT_DELTA"),
        (RootChange, 19, "LOP_ROOT_CHANGE"),
        (InSysXact, 22, "LOP_INSYSXACT"),
        (BeginXact, 128, "LOP_BEGIN_XACT"),
        (CommitXact, 129, "LOP_COMMIT_XACT"),
        (AbortXact, 130, "LOP_ABORT_XACT"),
        (PrepXact, 131, "LOP_PREP_XACT"),
        (HobtDdl, 137, "LOP_HOBT_DDL"),
        (HobtDelta, 140, "LOP_HOBT_DELTA"),
        (LockXact, 141, "LOP_LOCK_XACT"),
        (MigrateLocks, 143, "LOP_MIGRATE_LOCKS"),
        (BeginCkpt, 150, "LOP_BEGIN_CKPT"),
        (XactCkpt, 152, "LOP_XACT_CKPT"),
        (EndCkpt, 153, "LOP_END_CKPT"),
        (BufWrite, 154, "LOP_BUF_WRITE"),
        (CreateAllocChain, 176, "LOP_CREATE_ALLOCCHAIN"),
        (ShrinkNoop, 211, "LOP_SHRINK_NOOP"),
        (BulkExtAllocation, 214, "LOP_BULK_EXT_ALLOCATION"),
        (RangeInsert, 220, "LOP_RANGE_INSERT"),
        (InvalidateCache, 221, "LOP_INVALIDATE_CACHE"),
        (CsiRowgroup, 223, "LOP_CSI_ROWGROUP"),
    ]
}

byte_table! {
    Context, 20, [
        (Null, 0, "LCX_NULL"),
        (Heap, 1, "LCX_HEAP"),
        (Clustered, 2, "LCX_CLUSTERED"),
        (IndexLeaf, 3, "LCX_INDEX_LEAF"),
        (IndexInterior, 4, "LCX_INDEX_INTERIOR"),
        (TextMix, 5, "LCX_TEXT_MIX"),
        (Gam, 8, "LCX_GAM"),
        (Sgam, 9, "LCX_SGAM"),
        (Iam, 10, "LCX_IAM"),
        (Pfs, 11, "LCX_PFS"),
        (ObjectId, 13, "LCX_OBJECT_ID"),
        (FileHeader, 17, "LCX_FILE_HEADER"),
        (SchemaVersion, 18, "LCX_SCHEMA_VERSION"),
        (MarkAsGhost, 19, "LCX_MARK_AS_GHOST"),
        (BootPage, 20, "LCX_BOOT_PAGE"),
        (BootPageCkpt, 23, "LCX_BOOT_PAGE_CKPT"),
        (DiffMap, 24, "LCX_DIFF_MAP"),
        (MlMap, 25, "LCX_ML_MAP"),
        (UnlinkedReorgPage, 28, "LCX_UNLINKED_REORG_PAGE"),
        (BulkOperationPage, 29, "LCX_BULK_OPERATION_PAGE"),
    ]
}

impl Operation {
    pub fn carries_page(self) -> bool {
        use Operation::{
            ClearGamBits, CountDelta, DeleteRows, DeleteSplit, ExpungeRows, FileHdrModify,
            FormatPage, InSysXact, InsertRows, ModifyColumns, ModifyHeader, ModifyRow, RootChange,
            SetBits, SetFreeSpace,
        };
        matches!(
            self,
            FormatPage
                | InsertRows
                | DeleteRows
                | ModifyRow
                | ModifyHeader
                | ModifyColumns
                | SetBits
                | SetFreeSpace
                | DeleteSplit
                | ExpungeRows
                | FileHdrModify
                | ClearGamBits
                | CountDelta
                | RootChange
                | InSysXact
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransactionId {
    pub high: u16,
    pub low: u32,
}

impl TransactionId {
    pub fn is_none(&self) -> bool {
        self.high == 0 && self.low == 0
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04x}:{:08x}", self.high, self.low)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageDetail {
    pub slot: i16,
    pub allocation_unit_word: u32,
    pub previous_page_lsn: Lsn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRef {
    pub file: u16,
    pub page: u32,
    pub detail: Option<PageDetail>,
}

#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub lsn: Lsn,
    pub operation: Operation,
    pub context: Context,
    pub transaction: TransactionId,
    pub tag_bits: u16,
    pub fixed_len: u16,
    pub flag_bits: u16,
    pub previous: Lsn,
    pub page: Option<PageRef>,
    pub bytes: &'a [u8],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Summary {
    pub streams: u64,
    pub blocks: u64,
    pub blocks_2014: u64,
    pub blocks_2022: u64,
    pub empty_blocks: u64,
    pub records: u64,
    pub bytes: u64,
    pub block_checksums_verified: u64,
    pub blocks_without_checksum: u64,
    pub stream_checksums_verified: u64,
    pub streams_without_checksum: u64,
    pub first_block: Option<(u32, u32)>,
    pub end_block: Option<(u32, u32)>,
}

fn sector_fold(sector: &[u8], skip: Option<usize>) -> u32 {
    sector
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != skip)
        .fold(0, |sum, (_, word)| sum ^ u32::from_le_bytes(*word))
}

pub fn block_checksum(block: &[u8]) -> Option<u32> {
    if block.is_empty() || !block.len().is_multiple_of(SECTOR) {
        return None;
    }
    let mut sum = 0_u32;
    for (index, sector) in block.as_chunks::<SECTOR>().0.iter().enumerate() {
        let skip = (index == 0).then_some(BLOCK_CHECKSUM_AT / 4);
        sum = sum.rotate_left(1) ^ sector_fold(sector, skip);
    }
    Some(sum)
}

fn stream_step(sum: u32, sector: &[u8]) -> u32 {
    sum.rotate_left(1) ^ sector_fold(sector, None)
}

pub fn stream_checksum(payload: &[u8]) -> Option<u32> {
    if !payload.len().is_multiple_of(SECTOR) {
        return None;
    }
    Some(
        payload
            .as_chunks::<SECTOR>()
            .0
            .iter()
            .fold(0, |sum, sector| stream_step(sum, sector)),
    )
}

fn lsn_at(bytes: &[u8], at: usize) -> Option<Lsn> {
    Some(Lsn {
        vlf: u32_at(bytes, at)?,
        block: u32_at(bytes, at + 4)?,
        slot: u16_at(bytes, at + 8)?,
    })
}

pub const PAGE_LSN_AT: usize = 40;

pub fn page_lsn(page: &[u8]) -> Option<Lsn> {
    lsn_at(page, PAGE_LSN_AT)
}

struct State {
    summary: Summary,
    expect: Option<(u32, u32)>,
    previous_sequence: Option<u32>,
    stream_start: bool,
}

pub fn scan<R, F>(
    reader: &mut R,
    regions: &[LogRegion],
    first: Lsn,
    last: Lsn,
    limits: Limits,
    mut visit: F,
) -> Result<Summary, Error>
where
    R: Read + Seek,
    F: FnMut(&Record<'_>),
{
    let mut total = 0_u64;
    for region in regions {
        total = total
            .checked_add(region.bytes)
            .ok_or(Error::Limit("the size of the log streams"))?;
    }
    if total > limits.max_window_bytes {
        return Err(Error::Limit("the size of the log streams"));
    }
    let mut state = State {
        summary: Summary::default(),
        expect: None,
        previous_sequence: None,
        stream_start: true,
    };
    let mut chunk = vec![0_u8; READ_CHUNK];
    let mut block = Vec::new();
    for region in regions.iter().filter(|region| region.bytes != 0) {
        scan_stream(
            reader, region, &limits, &mut state, &mut chunk, &mut block, &mut visit,
        )?;
    }
    let (Some(first_block), Some(end_block)) = (state.summary.first_block, state.summary.end_block)
    else {
        return Err(Error::Bounds {
            what: "the log streams hold no block",
        });
    };
    if first_block != (first.vlf, first.block) {
        return Err(Error::Bounds {
            what: "the first block is not the block of FirstLSN",
        });
    }
    if end_block != (last.vlf, last.block) {
        return Err(Error::Bounds {
            what: "the blocks do not end where LastLSN says",
        });
    }
    Ok(state.summary)
}

fn scan_stream<R, F>(
    reader: &mut R,
    region: &LogRegion,
    limits: &Limits,
    state: &mut State,
    chunk: &mut [u8],
    block: &mut Vec<u8>,
    visit: &mut F,
) -> Result<(), Error>
where
    R: Read + Seek,
    F: FnMut(&Record<'_>),
{
    let stream = region.offset;
    if !region.bytes.is_multiple_of(SECTOR as u64) {
        return Err(Error::Stream {
            stream,
            what: "its length is not a whole number of sectors",
        });
    }
    reader.seek(SeekFrom::Start(region.offset))?;
    state.summary.streams += 1;
    state.stream_start = true;
    block.clear();
    let mut sum = 0_u32;
    let mut padding = false;
    let mut parity = 0_u8;
    let mut block_at = 0_u64;
    let mut done = 0_u64;
    while done < region.bytes {
        let want = usize::try_from((region.bytes - done).min(chunk.len() as u64))
            .map_err(|_| Error::Limit("the size of a log stream"))?;
        let Some(buffer) = chunk.get_mut(..want) else {
            return Err(Error::Limit("the size of a log stream"));
        };
        reader.read_exact(buffer)?;
        for sector in buffer.as_chunks::<SECTOR>().0 {
            sum = stream_step(sum, sector);
            let mark = sector.first().copied().unwrap_or(0);
            if padding {
                if sector.iter().any(|byte| *byte != 0) {
                    return Err(Error::Stream {
                        stream,
                        what: "there is data after the zero padding",
                    });
                }
            } else if block.is_empty() {
                if sector.iter().all(|byte| *byte == 0) {
                    padding = true;
                } else {
                    parity = mark & PARITY_BITS;
                    if mark & FIRST_SECTOR == 0 || (parity != 0x40 && parity != 0x80) {
                        return Err(Error::Block {
                            stream,
                            at: done,
                            what: "a sector that does not begin a block",
                        });
                    }
                    block_at = done;
                    block.extend_from_slice(sector);
                }
            } else {
                if mark & PARITY_BITS != parity || mark & FIRST_SECTOR != 0 {
                    return Err(Error::Block {
                        stream,
                        at: block_at,
                        what: "the marks of its sectors do not fit",
                    });
                }
                if block.len() + SECTOR > MAX_BLOCK_BYTES {
                    return Err(Error::Block {
                        stream,
                        at: block_at,
                        what: "it is larger than a log block can be",
                    });
                }
                block.extend_from_slice(sector);
            }
            if !padding && mark & LAST_SECTOR != 0 {
                finish_block(stream, block_at, block, limits, state, visit)?;
                block.clear();
            }
            done += SECTOR as u64;
        }
    }
    if !block.is_empty() {
        return Err(Error::Block {
            stream,
            at: block_at,
            what: "the stream ends inside it",
        });
    }
    match region.checksum {
        Some(stored) if stored == sum => state.summary.stream_checksums_verified += 1,
        Some(stored) => {
            return Err(Error::StreamChecksum {
                stream,
                stored,
                computed: sum,
            });
        }
        None => state.summary.streams_without_checksum += 1,
    }
    state.summary.bytes += region.bytes;
    Ok(())
}

fn finish_block<F>(
    stream: u64,
    at: u64,
    block: &mut [u8],
    limits: &Limits,
    state: &mut State,
    visit: &mut F,
) -> Result<(), Error>
where
    F: FnMut(&Record<'_>),
{
    let bad = |what: &'static str| Error::Block { stream, at, what };
    let stored = u32_at(block, BLOCK_CHECKSUM_AT).ok_or_else(|| bad("it is cut off"))?;
    let computed = block_checksum(block).ok_or_else(|| bad("it is not whole sectors"))?;
    if stored == computed {
        state.summary.block_checksums_verified += 1;
    } else if stored == 0 {
        state.summary.blocks_without_checksum += 1;
    } else {
        return Err(Error::BlockChecksum {
            stream,
            at,
            stored,
            computed,
        });
    }
    if state.summary.block_checksums_verified > 0 && state.summary.blocks_without_checksum > 0 {
        return Err(Error::MixedChecksums);
    }
    let sectors = block.len() / SECTOR;
    let end = block.len();
    for sector in 0..sectors {
        let saved = end
            .checked_sub(1 + sector)
            .and_then(|from| block.get(from).copied())
            .ok_or_else(|| bad("its saved bytes are cut off"))?;
        *block
            .get_mut(sector * SECTOR)
            .ok_or_else(|| bad("its sectors are cut off"))? = saved;
    }
    let header = match u8_at(block, 0) {
        Some(FORMAT_2014) => {
            state.summary.blocks_2014 += 1;
            HEADER_2014
        }
        Some(FORMAT_2022) => {
            state.summary.blocks_2022 += 1;
            HEADER_2022
        }
        Some(format) => return Err(Error::UnknownBlockFormat { format }),
        None => return Err(bad("it is cut off")),
    };
    let count = usize::from(u16_at(block, BLOCK_COUNT_AT).ok_or_else(|| bad("it is cut off"))?);
    let used = usize::from(u16_at(block, BLOCK_USED_AT).ok_or_else(|| bad("it is cut off"))?);
    let sequence = u32_at(block, BLOCK_SEQUENCE_AT).ok_or_else(|| bad("it is cut off"))?;
    let offset = u32_at(block, BLOCK_OFFSET_AT).ok_or_else(|| bad("it is cut off"))?;
    let sectors32 = u32::try_from(sectors).map_err(|_| bad("it has too many sectors"))?;
    let next = offset
        .checked_add(sectors32)
        .ok_or_else(|| bad("its offset overflows"))?;
    match state.expect {
        Some((expected_sequence, expected_offset)) if !state.stream_start => {
            if (sequence, offset) != (expected_sequence, expected_offset) {
                return Err(Error::Bounds {
                    what: "a block does not follow the previous one (a gap or a reordering)",
                });
            }
        }
        _ => {
            if let Some(previous) = state.previous_sequence
                && previous.checked_add(1) != Some(sequence)
            {
                return Err(Error::Bounds {
                    what: "the virtual log files of the streams are not consecutive",
                });
            }
            state.previous_sequence = Some(sequence);
            state.stream_start = false;
        }
    }
    if state.summary.first_block.is_none() {
        state.summary.first_block = Some((sequence, offset));
    }
    state.expect = Some((sequence, next));
    state.summary.end_block = Some((sequence, next));
    state.summary.blocks += 1;
    let array = used
        .checked_sub(
            count
                .checked_mul(2)
                .ok_or_else(|| bad("its count is too large"))?,
        )
        .ok_or_else(|| bad("its record count does not fit its length"))?;
    let usable = end - sectors;
    if used > usable {
        return Err(bad("its offset array reaches into the saved bytes"));
    }
    if count == 0 {
        if array < MIN_EMPTY_BLOCK {
            return Err(bad("an empty block with a short header"));
        }
        state.summary.empty_blocks += 1;
        return Ok(());
    }
    if array < header {
        return Err(bad("its offset array begins inside the header"));
    }
    let start_of = |index: usize| -> Result<usize, Error> {
        let word = array + 2 * (count - 1 - index);
        u16_at(block, word)
            .map(usize::from)
            .ok_or_else(|| bad("its offset array is cut off"))
    };
    let mut previous = header;
    for index in 0..count {
        let start = start_of(index)?;
        if start < previous || start >= array || (index == 0 && start != header) {
            return Err(bad(
                "its record offsets are not increasing and inside the block",
            ));
        }
        previous = start;
    }
    for index in 0..count {
        if state.summary.records >= limits.max_records {
            return Err(Error::Limit("the number of log records"));
        }
        let start = start_of(index)?;
        let limit = if index + 1 < count {
            start_of(index + 1)?
        } else {
            array
        };
        let slot = u16::try_from(index + 1).map_err(|_| bad("it has too many records"))?;
        let lsn = Lsn {
            vlf: sequence,
            block: offset,
            slot,
        };
        let bytes = slice(block, start, limit - start).ok_or_else(|| bad("a record is cut off"))?;
        let record = parse_record(lsn, bytes)?;
        state.summary.records += 1;
        visit(&record);
    }
    Ok(())
}

fn parse_record(lsn: Lsn, bytes: &[u8]) -> Result<Record<'_>, Error> {
    let bad = |what: &'static str| Error::Record { lsn, what };
    if bytes.len() < RECORD_HEADER {
        return Err(bad("it is shorter than its header"));
    }
    let cut = || bad("its header is cut off");
    let tag_bits = u16_at(bytes, 0).ok_or_else(cut)?;
    let fixed_len = u16_at(bytes, 2).ok_or_else(cut)?;
    let previous = lsn_at(bytes, 4).ok_or_else(cut)?;
    let flag_bits = u16_at(bytes, 14).ok_or_else(cut)?;
    let transaction = TransactionId {
        low: u32_at(bytes, 16).ok_or_else(cut)?,
        high: u16_at(bytes, 20).ok_or_else(cut)?,
    };
    let operation_byte = u8_at(bytes, 22).ok_or_else(cut)?;
    let context_byte = u8_at(bytes, 23).ok_or_else(cut)?;
    if usize::from(fixed_len) > bytes.len() {
        return Err(bad("its fixed length is longer than its bytes"));
    }
    let operation = Operation::from_byte(operation_byte).ok_or(Error::UnknownOperation {
        lsn,
        byte: operation_byte,
    })?;
    let context = Context::from_byte(context_byte).ok_or(Error::UnknownContext {
        lsn,
        byte: context_byte,
    })?;
    let page = if operation.carries_page() {
        if usize::from(fixed_len) < PAGE_RECORD_HEADER || bytes.len() < PAGE_RECORD_HEADER {
            return Err(bad("a page operation with a header too short for its page"));
        }
        Some(PageRef {
            page: u32_at(bytes, 24).ok_or_else(cut)?,
            file: u16_at(bytes, 28).ok_or_else(cut)?,
            detail: Some(PageDetail {
                slot: i16_at(bytes, 30).ok_or_else(cut)?,
                allocation_unit_word: u32_at(bytes, 32).ok_or_else(cut)?,
                previous_page_lsn: lsn_at(bytes, 36).ok_or_else(cut)?,
            }),
        })
    } else if operation == Operation::MigrateLocks {
        if bytes.len() < PAGE_ONLY_HEADER {
            return Err(bad("a lock migration with a header too short for its page"));
        }
        Some(PageRef {
            page: u32_at(bytes, 24).ok_or_else(cut)?,
            file: u16_at(bytes, 28).ok_or_else(cut)?,
            detail: None,
        })
    } else {
        None
    };
    Ok(Record {
        lsn,
        operation,
        context,
        transaction,
        tag_bits,
        fixed_len,
        flag_bits,
        previous,
        page,
        bytes,
    })
}
