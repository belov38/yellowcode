// Derived from bcdb 0.2.1, src/BcDb.Core/PageFile.cs and src/BcDb.Core/Mtf.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use std::{
    collections::BTreeSet,
    io::{self, Read, Seek, SeekFrom},
};

use crate::{
    bak_checksum::{Integrity, page_integrity},
    media::{self, DataRegion, HeaderChecks},
    records::FixedVarRecord,
};

const PAGE_SIZE: usize = 8192;
const PAGE_BYTES: u64 = 8192;
const PAGES_PER_EXTENT: u64 = 8;
const INTERVAL_EXTENTS: u64 = 63_904;
const INTERVAL_PAGES: u64 = INTERVAL_EXTENTS * PAGES_PER_EXTENT;
const PFS_INTERVAL: u64 = 8_088;
const PFS_EXTENT_STEP: u64 = PFS_INTERVAL / PAGES_PER_EXTENT;
const PFS_ALLOCATED: u8 = 0x40;

const HEADER_SIZE: usize = 96;
const BITMAP_RECORD: usize = 7_992;
const BITMAP_BYTES: usize = BITMAP_RECORD - 4;
const INTERVAL_WORDS: usize = 999;
const PFS_RECORD: usize = 4 + PFS_INTERVAL as usize;

const TYPE_FILE_HEADER: u8 = 15;
const TYPE_GAM: u8 = 8;
const TYPE_SGAM: u8 = 9;
const TYPE_DCM: u8 = 16;
const TYPE_PFS: u8 = 11;
const TYPE_ML_MAP: u8 = 17;
const TYPE_FILLER: u8 = 0x65;
const KNOWN_PAGE_TYPES: [u8; 16] = [1, 2, 3, 4, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19, 20];

const HEAD_BYTES: usize = 40;
const FRAME_BLOCKS: u64 = PAGES_PER_EXTENT;
const FRAME_BYTES: usize = PAGE_SIZE * 8;
const MAX_TAIL_BLOCKS: u64 = 1_024;
const VERIFIED_SLOTS: usize = 256;
const PFS_CACHE: usize = 4;
const COMPRESSED_MAGIC: &[u8; 8] = b"MSSQLBAK";
const PAGE_NUMBERS: u64 = 1 << 32;
const MAX_INTERVALS: u64 = 4_208;
const MAX_ABSENT: usize = 1 << 20;
const VOTE_MARGIN: u64 = 1_024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("page map I/O: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported backup layout: {0}")]
    Unsupported(&'static str),
    #[error("unsupported page protection: {0}")]
    TornPage(&'static str),
    #[error("inconsistent backup: {0}")]
    Inconsistent(&'static str),
    #[error("page map limit exceeded: {0}")]
    Limit(&'static str),
    #[error("page {page} ({what}) fails its integrity check")]
    Corrupt { page: u32, what: &'static str },
    #[error(
        "region 2 frame for extent {extent} rests on its page headers alone (no lead extent, no differential map bit)"
    )]
    UnprovenFrame { extent: u32 },
    #[error("extent {extent} is allocated but neither data region holds it")]
    UncopiedExtent { extent: u32 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenOptions {
    pub accept_unmarked_frames: bool,
    pub accept_uncopied_extents: bool,
}

impl OpenOptions {
    #[must_use]
    pub fn accepting_unmarked_frames(mut self) -> OpenOptions {
        self.accept_unmarked_frames = true;
        self
    }

    #[must_use]
    pub fn accepting_uncopied_extents(mut self) -> OpenOptions {
        self.accept_uncopied_extents = true;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageChecks {
    pub verified: u32,
    pub unprotected: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_pages: u64,
    pub max_regions: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_pages: 1 << 29,
        max_regions: 64,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageOrigin {
    Region1,
    Region2,
}

fn from_media(error: media::Error) -> Error {
    if error.is_missing_data_stream() {
        return Error::Unsupported(
            "the backup holds no data copy (a transaction log backup, or a backup of nothing)",
        );
    }
    if error.is_truncation() {
        return Error::Inconsistent("the backup file is truncated: it ends inside its structure");
    }
    match error {
        media::Error::Io(error) => Error::Io(error),
        media::Error::Checksum(_) => Error::Inconsistent("a backup header checksum does not match"),
        media::Error::Invalid(_) => {
            Error::Inconsistent("the backup media framing is invalid (not a backup, or damaged)")
        }
        media::Error::Unsupported(reason) => Error::Unsupported(unsupported_media(&reason)),
        media::Error::ResourceLimit(_) => Error::Limit("backup media regions"),
    }
}

fn unsupported_media(reason: &str) -> &'static str {
    if reason.starts_with("unknown descriptor") {
        "the backup holds a block of a kind this reader does not know (the backup of a database with a memory-optimized filegroup has one)"
    } else if reason.starts_with("encrypted or compressed stream") {
        "a stream of the backup is encrypted or compressed"
    } else if reason.starts_with("incomplete or multi-volume") {
        "the backup is incomplete, or one volume of several"
    } else {
        "the backup media is not understood (an unknown stream prefix, or a kind of media this reader does not know)"
    }
}

fn short() -> Error {
    Error::Inconsistent("a page is shorter than its structure")
}

fn le_u16(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let end = at.checked_add(2).ok_or_else(short)?;
    let pair: [u8; 2] = bytes
        .get(at..end)
        .and_then(|part| part.try_into().ok())
        .ok_or_else(short)?;
    Ok(u16::from_le_bytes(pair))
}

fn le_u32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let end = at.checked_add(4).ok_or_else(short)?;
    let quad: [u8; 4] = bytes
        .get(at..end)
        .and_then(|part| part.try_into().ok())
        .ok_or_else(short)?;
    Ok(u32::from_le_bytes(quad))
}

#[derive(Debug, Clone, Copy)]
struct Head {
    version: u8,
    kind: u8,
    page_id: u32,
    file_id: u16,
}

fn head(bytes: &[u8]) -> Result<Head, Error> {
    Ok(Head {
        version: *bytes.first().ok_or_else(short)?,
        kind: *bytes.get(1).ok_or_else(short)?,
        page_id: le_u32(bytes, 32)?,
        file_id: le_u16(bytes, 36)?,
    })
}

fn is_filler(bytes: &[u8]) -> bool {
    bytes.first() == Some(&1) && bytes.get(1) == Some(&TYPE_FILLER)
}

fn expect_page(bytes: &[u8], id: u64, kind: Option<u8>, what: &'static str) -> Result<(), Error> {
    let found = head(bytes)?;
    let good = found.version == 1
        && found.file_id == 1
        && u64::from(found.page_id) == id
        && kind.is_none_or(|k| found.kind == k);
    if good {
        Ok(())
    } else {
        Err(Error::Inconsistent(what))
    }
}

fn torn_page_refusal(what: &str) -> Error {
    macro_rules! torn {
        ($page:literal) => {
            Error::TornPage(concat!(
                "the ",
                $page,
                " carries torn-page protection (TORN_PAGE_DETECTION): its header holds bits of its content that this reader cannot put back (the database may use CHECKSUM now: a page keeps the protection it was written with until it is written again)"
            ))
        };
    }
    match what {
        "file header" => torn!("file header page"),
        "GAM" => torn!("GAM page"),
        "SGAM" => torn!("SGAM page"),
        "DCM" => torn!("DCM page"),
        "final GAM" => torn!("final GAM page of region 2"),
        "final DCM" => torn!("final DCM page of region 2"),
        "PFS" => torn!("PFS page"),
        _ => torn!("page"),
    }
}

fn check_integrity(
    page: &[u8],
    number: u64,
    what: &'static str,
    checks: &mut PageChecks,
) -> Result<(), Error> {
    let page: &[u8; PAGE_SIZE] = page.try_into().map_err(|_| short())?;
    let verdict = page_integrity(page);
    if matches!(verdict, Integrity::TornPage { .. }) {
        return Err(torn_page_refusal(what));
    }
    if verdict.is_corrupt() {
        let page = u32::try_from(number).map_err(|_| Error::Limit("pages in the file"))?;
        return Err(Error::Corrupt { page, what });
    }
    if verdict.is_verified() {
        checks.verified += 1;
    } else {
        checks.unprotected += 1;
    }
    Ok(())
}

fn extent_bitmap(page: &[u8]) -> Result<&[u8], Error> {
    if le_u16(page, 22)? != 2 {
        return Err(Error::Inconsistent(
            "an allocation page does not have two records",
        ));
    }
    let at = usize::from(le_u16(page, PAGE_SIZE - 4)?);
    let length = usize::from(le_u16(page, at.checked_add(2).ok_or_else(short)?)?);
    let end = at.checked_add(length).ok_or_else(short)?;
    if at < HEADER_SIZE || length != BITMAP_RECORD || end > PAGE_SIZE - 4 {
        return Err(Error::Inconsistent(
            "an allocation page's bitmap record is not where or what it should be",
        ));
    }
    page.get(at + 4..end).ok_or_else(short)
}

fn pfs_bytes(page: &[u8]) -> Result<&[u8], Error> {
    if le_u16(page, 22)? != 1 {
        return Err(Error::Inconsistent("a PFS page does not have one record"));
    }
    let at = usize::from(le_u16(page, PAGE_SIZE - 2)?);
    let length = usize::from(le_u16(page, at.checked_add(2).ok_or_else(short)?)?);
    let end = at.checked_add(length).ok_or_else(short)?;
    if at < HEADER_SIZE || length != PFS_RECORD || end > PAGE_SIZE - 2 {
        return Err(Error::Inconsistent(
            "a PFS page's record is not where or what it should be",
        ));
    }
    page.get(at + 4..end).ok_or_else(short)
}

fn file_header_size(page: &[u8]) -> Result<u32, Error> {
    let record = page.get(HEADER_SIZE..PAGE_SIZE - 2).ok_or_else(short)?;
    let record = FixedVarRecord::parse(record)
        .map_err(|_| Error::Inconsistent("the file header record cannot be parsed"))?;
    let size = record
        .variables
        .get(4)
        .map(|column| column.data)
        .filter(|data| data.len() == 4)
        .ok_or(Error::Inconsistent(
            "the file header record has no size column where every known layout puts it",
        ))?;
    le_u32(size, 0)
}

fn bit(bits: &[u8], index: u64) -> bool {
    usize::try_from(index / 8)
        .ok()
        .and_then(|byte| bits.get(byte))
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

#[derive(Debug, Clone, Copy)]
struct Region {
    offset: u64,
    blocks: u64,
}

impl Region {
    fn block_offset(&self, block: u64) -> Result<u64, Error> {
        if block >= self.blocks {
            return Err(Error::Inconsistent(
                "a block lies beyond the end of its data region",
            ));
        }
        block
            .checked_mul(PAGE_BYTES)
            .and_then(|bytes| self.offset.checked_add(bytes))
            .ok_or(Error::Inconsistent("a block offset overflows"))
    }
}

impl From<&DataRegion> for Region {
    fn from(region: &DataRegion) -> Self {
        Region {
            offset: region.offset,
            blocks: region.pages,
        }
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(buf)?;
    Ok(())
}

fn read_block<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    block: u64,
    buf: &mut [u8],
) -> Result<(), Error> {
    read_at(reader, region.block_offset(block)?, buf)
}

fn read_head<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    block: u64,
) -> Result<[u8; HEAD_BYTES], Error> {
    let mut bytes = [0; HEAD_BYTES];
    read_block(reader, region, block, &mut bytes)?;
    Ok(bytes)
}

fn block_is_filler<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    block: u64,
) -> Result<bool, Error> {
    let mut bytes = [0; 2];
    read_block(reader, region, block, &mut bytes)?;
    Ok(is_filler(&bytes))
}

fn names_page(found: &Head) -> bool {
    found.file_id == 1
        && match (found.version, found.kind) {
            (1, kind) => kind != TYPE_FILLER,
            (0, 0) => found.page_id != 0,
            _ => false,
        }
}

fn lead_matches(bytes: &[u8], first_page: u64) -> bool {
    head(bytes).is_ok_and(|found| names_page(&found) && u64::from(found.page_id) == first_page)
}

const LEAD_MISMATCH: Error = Error::Inconsistent(
    "an extent does not lead with its own first page (a damaged backup, or one with more than one data file)",
);

const MISPLACED_ALLOCATION_PAGE: Error = Error::Inconsistent(
    "the first page of an extent is an allocation page that cannot sit where its page id says",
);

fn allocation_page_fits(found: &Head) -> bool {
    let id = u64::from(found.page_id);
    match found.kind {
        TYPE_PFS => id != 0 && id.is_multiple_of(PFS_INTERVAL),
        TYPE_GAM => id != 0 && id.is_multiple_of(INTERVAL_PAGES),
        TYPE_SGAM | TYPE_DCM | TYPE_ML_MAP => false,
        _ => true,
    }
}

const OTHER_FILE: Error = Error::Unsupported(
    "the backup holds pages of a data file other than file 1 (a database with more than one data file, or a backup of a file or filegroup)",
);

fn page_of_another_file(found: &Head) -> bool {
    found.version == 1 && KNOWN_PAGE_TYPES.contains(&found.kind) && !matches!(found.file_id, 0 | 1)
}

fn frame_of_another_file<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    frame: u64,
    first: &Head,
) -> Result<bool, Error> {
    if !page_of_another_file(first) {
        return Ok(false);
    }
    for slot in 1..FRAME_BLOCKS {
        let block = frame * FRAME_BLOCKS + slot;
        if block >= region.blocks {
            break;
        }
        let sibling = head(&read_head(reader, region, block)?)?;
        if page_of_another_file(&sibling) && sibling.file_id == first.file_id {
            return Ok(true);
        }
    }
    Ok(false)
}

const FRAME_DISAGREES: Error =
    Error::Inconsistent("the first page of an extent disagrees with the other pages of its frame");

fn check_frame<R: Read + Seek>(reader: &mut R, region: Region, frame: u64) -> Result<(), Error> {
    let mut first = [0_u8; PAGE_SIZE];
    read_block(reader, region, frame * FRAME_BLOCKS, &mut first)?;
    let lead = head(&first)?;
    if page_integrity(&first).is_corrupt() {
        return Err(Error::Corrupt {
            page: lead.page_id,
            what: "first page of an extent of region 1",
        });
    }
    let named = u64::from(lead.page_id) / PAGES_PER_EXTENT;
    let mut for_the_lead = 0_u32;
    let mut for_others: Vec<(u64, u32)> = Vec::new();
    for slot in 1..FRAME_BLOCKS {
        let other = head(&read_head(reader, region, frame * FRAME_BLOCKS + slot)?)?;
        if !names_page(&other) || u64::from(other.page_id) % PAGES_PER_EXTENT != slot {
            continue;
        }
        let extent = u64::from(other.page_id) / PAGES_PER_EXTENT;
        if extent == named {
            for_the_lead += 1;
        } else if let Some((_, votes)) = for_others.iter_mut().find(|(e, _)| *e == extent) {
            *votes += 1;
        } else {
            for_others.push((extent, 1));
        }
    }
    if for_the_lead == 0 && for_others.iter().any(|(_, votes)| *votes >= 2) {
        return Err(FRAME_DISAGREES);
    }
    Ok(())
}

fn check_padding<R: Read + Seek>(reader: &mut R, region: Region, from: u64) -> Result<(), Error> {
    let rest = region.blocks.saturating_sub(from);
    if rest > MAX_TAIL_BLOCKS {
        return Err(Error::Inconsistent(
            "the data region holds more blocks than the allocation maps account for",
        ));
    }
    for block in from..region.blocks {
        if !block_is_filler(reader, region, block)? {
            return Err(Error::Inconsistent(
                "the data region holds blocks that no allocation map accounts for",
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Interval {
    base: u64,
    present: Box<[u64]>,
    ranks: Box<[u32]>,
}

impl Interval {
    fn new(base: u64, present: Vec<u64>) -> Interval {
        let mut ranks = Vec::with_capacity(present.len());
        let mut seen = 0_u32;
        for word in &present {
            ranks.push(seen);
            seen += word.count_ones();
        }
        Interval {
            base,
            present: present.into_boxed_slice(),
            ranks: ranks.into_boxed_slice(),
        }
    }

    fn rank(&self, extent: u64) -> Option<u64> {
        let word = usize::try_from(extent / 64).ok()?;
        let bits = *self.present.get(word)?;
        if bits & (1_u64 << (extent % 64)) == 0 {
            return None;
        }
        let below = bits & ((1_u64 << (extent % 64)) - 1);
        Some(u64::from(*self.ranks.get(word)?) + u64::from(below.count_ones()))
    }

    fn neighbours(&self, extent: u64) -> (Option<u64>, Option<u64>) {
        let word = usize::try_from(extent / 64).unwrap_or(usize::MAX);
        let shift = extent % 64;
        let below_mask = (1_u64 << shift) - 1;
        let above_mask = u64::MAX
            .checked_shl(u32::try_from(shift + 1).unwrap_or(64))
            .unwrap_or(0);
        let below = self
            .present
            .iter()
            .enumerate()
            .take(word.saturating_add(1))
            .rev()
            .find_map(|(index, bits)| {
                let bits = if index == word {
                    bits & below_mask
                } else {
                    *bits
                };
                (bits != 0).then(|| index as u64 * 64 + 63 - u64::from(bits.leading_zeros()))
            });
        let above = self
            .present
            .iter()
            .enumerate()
            .skip(word)
            .find_map(|(index, bits)| {
                let bits = if index == word {
                    bits & above_mask
                } else {
                    *bits
                };
                (bits != 0).then(|| index as u64 * 64 + u64::from(bits.trailing_zeros()))
            });
        (below, above)
    }

    fn block_of(&self, extent: u64) -> Option<u64> {
        self.base
            .checked_add(self.rank(extent)?.checked_mul(PAGES_PER_EXTENT)?)
    }

    fn index_bytes(&self) -> usize {
        self.present.len() * size_of::<u64>() + self.ranks.len() * size_of::<u32>()
    }
}

fn nth_set(bits: &[u64], n: u64) -> Option<u64> {
    let mut left = n;
    for (index, word) in bits.iter().enumerate() {
        let count = u64::from(word.count_ones());
        if left < count {
            let mut rest = *word;
            for _ in 0..left {
                rest &= rest.wrapping_sub(1);
            }
            return Some(index as u64 * 64 + u64::from(rest.trailing_zeros()));
        }
        left -= count;
    }
    None
}

fn count_set(bits: &[u64]) -> u64 {
    bits.iter().map(|word| u64::from(word.count_ones())).sum()
}

fn word_bit(bits: &[u64], index: u64) -> bool {
    usize::try_from(index / 64)
        .ok()
        .and_then(|word| bits.get(word))
        .is_some_and(|word| word & (1_u64 << (index % 64)) != 0)
}

fn set_word_bit(bits: &mut [u64], index: u64, on: bool) {
    let word = usize::try_from(index / 64)
        .ok()
        .and_then(|word| bits.get_mut(word));
    if let Some(word) = word {
        if on {
            *word |= 1_u64 << (index % 64);
        } else {
            *word &= !(1_u64 << (index % 64));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lead {
    Filler,
    Page(u64),
}

fn lead_of<R: Read + Seek>(reader: &mut R, region: Region, frame: u64) -> Result<Lead, Error> {
    let bytes = read_head(reader, region, frame * FRAME_BLOCKS)?;
    if is_filler(&bytes) {
        return Ok(Lead::Filler);
    }
    let found = head(&bytes)?;
    if names_page(&found) && u64::from(found.page_id).is_multiple_of(PAGES_PER_EXTENT) {
        if !allocation_page_fits(&found) {
            return Err(MISPLACED_ALLOCATION_PAGE);
        }
        Ok(Lead::Page(u64::from(found.page_id)))
    } else if frame_of_another_file(reader, region, frame, &found)? {
        Err(OTHER_FILE)
    } else {
        Err(LEAD_MISMATCH)
    }
}

fn frame_extent<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    frame: u64,
    first_page: u64,
) -> Result<u64, Error> {
    match lead_of(reader, region, frame)? {
        Lead::Page(id) => {
            let extent = id.checked_sub(first_page).ok_or(LEAD_MISMATCH)? / PAGES_PER_EXTENT;
            if extent < INTERVAL_EXTENTS {
                Ok(extent)
            } else {
                Err(LEAD_MISMATCH)
            }
        }
        Lead::Filler => Err(LEAD_MISMATCH),
    }
}

fn interval_end<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    from: u64,
    total: u64,
    boundary: u64,
) -> Result<u64, Error> {
    let (mut low, mut high) = (from, total);
    while low < high {
        let middle = low + (high - low) / 2;
        let beyond = match lead_of(reader, region, middle)? {
            Lead::Filler => true,
            Lead::Page(id) => id >= boundary,
        };
        if beyond {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    Ok(low)
}

struct Absent {
    extent: u64,
    allocated: bool,
    after_last: bool,
}

struct Placed {
    present: Vec<u64>,
    absent: Vec<Absent>,
}

fn skew<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    named: &Interval,
    from: u64,
    frame: u64,
    first_page: u64,
) -> Result<Option<u64>, Error> {
    let extent = frame_extent(reader, region, from + frame, first_page)?;
    Ok(named.rank(extent).and_then(|rank| rank.checked_sub(frame)))
}

fn without_skipped<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    named: &[u64],
    from: u64,
    frames: u64,
    first_page: u64,
) -> Result<Option<Vec<u64>>, Error> {
    let index = Interval::new(0, named.to_vec());
    let last = frames.checked_sub(1).ok_or(LEAD_MISMATCH)?;
    let Some(last_skew) = skew(reader, region, &index, from, last, first_page)? else {
        return Ok(None);
    };
    if skew(reader, region, &index, from, 0, first_page)? != Some(0) {
        return Ok(None);
    }
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut pending = vec![(0_u64, 0_u64, last, last_skew)];
    while let Some((low, low_skew, high, high_skew)) = pending.pop() {
        if low_skew == high_skew {
            continue;
        }
        if high == low + 1 {
            runs.push((low + low_skew + 1, high_skew - low_skew));
            continue;
        }
        let middle = low + (high - low) / 2;
        let Some(middle_skew) = skew(reader, region, &index, from, middle, first_page)? else {
            return Ok(None);
        };
        if middle_skew < low_skew || middle_skew > high_skew {
            return Ok(None);
        }
        pending.push((middle, middle_skew, high, high_skew));
        pending.push((low, low_skew, middle, middle_skew));
    }
    runs.sort_unstable();
    let last_rank = last + last_skew;
    let mut present = named.to_vec();
    let (mut rank, mut next_run) = (0_u64, 0_usize);
    for (word_index, word) in named.iter().enumerate() {
        let mut rest = *word;
        while rest != 0 {
            let extent = word_index as u64 * 64 + u64::from(rest.trailing_zeros());
            rest &= rest.wrapping_sub(1);
            while runs
                .get(next_run)
                .is_some_and(|(start, len)| rank >= start + len)
            {
                next_run += 1;
            }
            let inside = runs.get(next_run).is_some_and(|(start, _)| rank >= *start);
            if inside || rank > last_rank {
                set_word_bit(&mut present, extent, false);
            }
            rank += 1;
        }
    }
    Ok(Some(present))
}

fn scan_extents<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    from: u64,
    frames: u64,
    first_page: u64,
) -> Result<Vec<u64>, Error> {
    let mut present = vec![0_u64; INTERVAL_WORDS];
    let mut previous: Option<u64> = None;
    for frame in 0..frames {
        let extent = frame_extent(reader, region, from + frame, first_page)?;
        if previous.is_some_and(|p| extent <= p) || (frame == 0 && extent != 0) {
            return Err(Error::Inconsistent(
                "the extents of the data region do not ascend from the lead extent of their interval",
            ));
        }
        previous = Some(extent);
        set_word_bit(&mut present, extent, true);
    }
    Ok(present)
}

fn place_extents<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    interval: u64,
    from: u64,
    frames: u64,
    named: Vec<u64>,
    maps: (&[u8], &[u8]),
) -> Result<Placed, Error> {
    let first_page = interval * INTERVAL_PAGES;
    let count = count_set(&named);
    let (Some(last_frame), Some(last_named)) = (frames.checked_sub(1), count.checked_sub(1)) else {
        return Err(LEAD_MISMATCH);
    };
    let as_named = frames == count
        && nth_set(&named, last_named)
            == Some(frame_extent(reader, region, from + last_frame, first_page)?);
    let present = if as_named {
        named.clone()
    } else if frames < count {
        match without_skipped(reader, region, &named, from, frames, first_page)? {
            Some(present) => present,
            None => scan_extents(reader, region, from, frames, first_page)?,
        }
    } else {
        scan_extents(reader, region, from, frames, first_page)?
    };
    let last = count_set(&present)
        .checked_sub(1)
        .and_then(|n| nth_set(&present, n))
        .ok_or(LEAD_MISMATCH)?;
    let (gam, sgam) = maps;
    let mut absent = Vec::new();
    for extent in 0..INTERVAL_EXTENTS {
        if !as_named && word_bit(&named, extent) && !word_bit(&present, extent) {
            absent.push(Absent {
                extent: interval * INTERVAL_EXTENTS + extent,
                allocated: !bit(gam, extent) || bit(sgam, extent),
                after_last: extent > last,
            });
        }
    }
    Ok(Placed { present, absent })
}

struct Walk1 {
    intervals: Vec<Interval>,
    checks: PageChecks,
    header_size: u32,
    derived_end: u64,
    absent: Vec<Absent>,
    highest: Vec<u64>,
    last_frames: Vec<u64>,
}

fn highest_allocated(gam: &[u8]) -> u64 {
    (0..INTERVAL_EXTENTS)
        .rev()
        .find(|extent| !bit(gam, *extent))
        .unwrap_or(0)
}

fn walk_region1<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    limits: &Limits,
) -> Result<Walk1, Error> {
    let mut page = [0_u8; PAGE_SIZE];
    read_block(reader, region, 0, &mut page)?;
    if frame_of_another_file(reader, region, 0, &head(&page)?)? {
        return Err(OTHER_FILE);
    }
    let mut checks = PageChecks::default();
    check_integrity(&page, 0, "file header", &mut checks)?;
    expect_page(
        &page,
        0,
        Some(TYPE_FILE_HEADER),
        "block 0 of the data region is not the file header page",
    )?;
    let header_size = file_header_size(&page)?;
    if header_size == 0 || !u64::from(header_size).is_multiple_of(PAGES_PER_EXTENT) {
        return Err(Error::Inconsistent(
            "the file header records a size that is not a positive multiple of 8 pages",
        ));
    }
    if u64::from(header_size) > limits.max_pages {
        return Err(Error::Limit("pages in the file"));
    }
    if u64::from(header_size).div_ceil(INTERVAL_PAGES) > MAX_INTERVALS {
        return Err(Error::Limit("GAM intervals in the file"));
    }

    let mut walk = Walk1 {
        intervals: Vec::new(),
        checks,
        header_size,
        derived_end: 0,
        absent: Vec::new(),
        highest: Vec::new(),
        last_frames: Vec::new(),
    };
    let total = region.blocks / FRAME_BLOCKS;
    let mut cursor = 0_u64;
    let mut gam_page = [0_u8; PAGE_SIZE];
    let mut sgam_page = [0_u8; PAGE_SIZE];
    for interval in 0_u64.. {
        let first_page = interval * INTERVAL_PAGES;
        if interval > 0 {
            if cursor >= total || lead_of(reader, region, cursor)? == Lead::Filler {
                break;
            }
            if first_page >= limits.max_pages.min(PAGE_NUMBERS) {
                return Err(Error::Limit("pages in the file"));
            }
            if interval >= MAX_INTERVALS {
                return Err(Error::Limit("GAM intervals in the file"));
            }
        }
        let block = cursor * FRAME_BLOCKS;
        let (gam_at, gam_id) = if interval == 0 {
            (2, 2)
        } else {
            (0, first_page)
        };
        read_block(reader, region, block + gam_at, &mut gam_page)?;
        check_integrity(&gam_page, gam_id, "GAM", &mut walk.checks)?;
        expect_page(
            &gam_page,
            gam_id,
            Some(TYPE_GAM),
            "the GAM page of an interval is not where the stream puts it",
        )?;
        read_block(reader, region, block + gam_at + 1, &mut sgam_page)?;
        check_integrity(&sgam_page, gam_id + 1, "SGAM", &mut walk.checks)?;
        expect_page(
            &sgam_page,
            gam_id + 1,
            Some(TYPE_SGAM),
            "the SGAM page of an interval is not where the stream puts it",
        )?;
        read_block(reader, region, block + 6, &mut page)?;
        check_integrity(&page, first_page + 6, "DCM", &mut walk.checks)?;
        expect_page(
            &page,
            first_page + 6,
            Some(TYPE_DCM),
            "the DCM page of an interval is not where the stream puts it",
        )?;
        let gam = extent_bitmap(&gam_page)?;
        let sgam = extent_bitmap(&sgam_page)?;
        if gam.len() != BITMAP_BYTES || sgam.len() != BITMAP_BYTES {
            return Err(Error::Inconsistent("an extent bitmap has the wrong size"));
        }

        let mut named = vec![0_u64; INTERVAL_WORDS];
        for extent in 0..INTERVAL_EXTENTS {
            let global = interval * INTERVAL_EXTENTS + extent;
            let wanted = extent == 0
                || !bit(gam, extent)
                || bit(sgam, extent)
                || global.is_multiple_of(PFS_EXTENT_STEP);
            if wanted {
                set_word_bit(&mut named, extent, true);
            }
        }

        let end = interval_end(reader, region, cursor, total, first_page + INTERVAL_PAGES)?;
        let placed = place_extents(
            reader,
            region,
            interval,
            cursor,
            end - cursor,
            named,
            (gam, sgam),
        )?;
        let last = count_set(&placed.present)
            .checked_sub(1)
            .and_then(|n| nth_set(&placed.present, n))
            .ok_or(LEAD_MISMATCH)?;
        walk.last_frames.push(end - 1);
        walk.derived_end = walk
            .derived_end
            .max(first_page + (last + 1) * PAGES_PER_EXTENT);
        if walk.absent.len().saturating_add(placed.absent.len()) > MAX_ABSENT {
            return Err(Error::Limit(
                "extents that the allocation maps name and that are absent from region 1",
            ));
        }
        walk.absent.extend(placed.absent);
        walk.highest.push(highest_allocated(gam));
        walk.intervals
            .push(Interval::new(cursor * FRAME_BLOCKS, placed.present));
        cursor = end;
    }

    let last_interval = (walk.intervals.len() as u64).saturating_sub(1);
    walk.absent.retain(|absent| {
        !(absent.extent / INTERVAL_EXTENTS == last_interval
            && absent.after_last
            && !absent.allocated)
    });
    check_padding(reader, region, cursor * FRAME_BLOCKS)?;
    let intervals_by_size = u64::from(header_size).div_ceil(INTERVAL_PAGES);
    if intervals_by_size > walk.intervals.len() as u64 {
        return Err(Error::Inconsistent(
            "the file size reaches into a GAM interval that the data region does not hold",
        ));
    }
    if walk.derived_end > limits.max_pages {
        return Err(Error::Limit("pages in the file"));
    }
    Ok(walk)
}

fn marked(extent: u64, current: Option<u64>, final_dcm: &[u8]) -> bool {
    let interval = extent / INTERVAL_EXTENTS;
    (interval == 0 && extent <= 1)
        || extent == interval * INTERVAL_EXTENTS
        || (current == Some(interval) && bit(final_dcm, extent - interval * INTERVAL_EXTENTS))
}

#[derive(Default)]
struct Walk2 {
    frames: Vec<u32>,
    unmarked: Vec<u32>,
    final_gam: Vec<(u64, Vec<u8>)>,
    checks: PageChecks,
}

fn walk_region2<R: Read + Seek>(
    reader: &mut R,
    region: Region,
    vote_limit: u64,
    highest: &[u64],
    options: &OpenOptions,
) -> Result<Walk2, Error> {
    let mut walk = Walk2::default();
    let mut frame = vec![0_u8; FRAME_BYTES];
    let mut block = 0_u64;
    let mut previous: Option<u64> = None;
    let mut current: Option<u64> = None;
    let mut final_dcm: Vec<u8> = Vec::new();
    let mut final_highest = 0_u64;
    while block + FRAME_BLOCKS <= region.blocks {
        read_block(reader, region, block, &mut frame)?;
        let mut candidates: Vec<(u64, u32)> = Vec::with_capacity(8);
        let mut padding = true;
        let mut beyond = 0_u32;
        for slot in 0..8_usize {
            let bytes = frame
                .get(slot * PAGE_SIZE..slot * PAGE_SIZE + HEAD_BYTES)
                .ok_or_else(short)?;
            padding &= is_filler(bytes);
            let found = head(bytes)?;
            let identifies = match (found.version, found.kind) {
                (1, kind) => KNOWN_PAGE_TYPES.contains(&kind),
                (0, 0) => frame
                    .get(slot * PAGE_SIZE + HEADER_SIZE..(slot + 1) * PAGE_SIZE)
                    .is_some_and(|body| body.iter().all(|b| *b == 0)),
                _ => false,
            };
            if !identifies
                || !names_page(&found)
                || u64::from(found.page_id) >= vote_limit
                || u64::from(found.page_id) % PAGES_PER_EXTENT != slot as u64
            {
                continue;
            }
            let extent = u64::from(found.page_id) / PAGES_PER_EXTENT;
            let interval = extent / INTERVAL_EXTENTS;
            let mut top = usize::try_from(interval)
                .ok()
                .and_then(|index| highest.get(index))
                .copied();
            if current == Some(interval) {
                top = top.map(|region1| region1.max(final_highest));
            }
            if top.is_none_or(|top| extent % INTERVAL_EXTENTS > top.saturating_add(VOTE_MARGIN)) {
                beyond += 1;
                continue;
            }
            match candidates.iter_mut().find(|(named, _)| *named == extent) {
                Some((_, votes)) => *votes += 1,
                None => candidates.push((extent, 1)),
            }
        }
        if candidates.is_empty() {
            if padding {
                break;
            }
            if beyond > 0 {
                return Err(Error::Inconsistent(
                    "a region 2 frame names only extents far beyond the highest allocated one",
                ));
            }
            return Err(Error::Inconsistent(
                "a region 2 frame has no frame-aligned page id and is not padding",
            ));
        }
        let later: Vec<(u64, u32)> = candidates
            .into_iter()
            .filter(|(extent, _)| previous.is_none_or(|p| *extent > p))
            .collect();
        let most = later.iter().map(|(_, votes)| *votes).max();
        let leaders: Vec<u64> = later
            .iter()
            .filter(|(_, votes)| Some(*votes) == most)
            .map(|(extent, _)| *extent)
            .collect();
        let extent = match leaders.as_slice() {
            [] => {
                return Err(Error::Inconsistent(
                    "a region 2 frame fits no extent after the previous frame",
                ));
            }
            [only] => *only,
            several => {
                let mut allowed = several
                    .iter()
                    .copied()
                    .filter(|extent| marked(*extent, current, &final_dcm));
                match (allowed.next(), allowed.next()) {
                    (Some(one), None) => one,
                    _ => {
                        return Err(Error::Inconsistent(
                            "a region 2 frame's page headers name more than one extent equally often and the final differential map does not decide",
                        ));
                    }
                }
            }
        };
        let interval = extent / INTERVAL_EXTENTS;
        if current != Some(interval) {
            if extent != interval * INTERVAL_EXTENTS {
                return Err(Error::Inconsistent(
                    "a region 2 section does not start with the lead extent of its interval",
                ));
            }
            let dcm = frame.get(6 * PAGE_SIZE..7 * PAGE_SIZE).ok_or_else(short)?;
            check_integrity(
                dcm,
                interval * INTERVAL_PAGES + 6,
                "final DCM",
                &mut walk.checks,
            )?;
            expect_page(
                dcm,
                interval * INTERVAL_PAGES + 6,
                Some(TYPE_DCM),
                "the final DCM page of an interval is not where region 2 puts it",
            )?;
            final_dcm = extent_bitmap(dcm)?.to_vec();
            let gam_slot = if interval == 0 { 2 } else { 0 };
            let gam_id = if interval == 0 {
                2
            } else {
                interval * INTERVAL_PAGES
            };
            let gam = frame
                .get(gam_slot * PAGE_SIZE..(gam_slot + 1) * PAGE_SIZE)
                .ok_or_else(short)?;
            check_integrity(gam, gam_id, "final GAM", &mut walk.checks)?;
            expect_page(
                gam,
                gam_id,
                Some(TYPE_GAM),
                "the final GAM page of an interval is not where region 2 puts it",
            )?;
            let final_bitmap = extent_bitmap(gam)?;
            final_highest = highest_allocated(final_bitmap);
            walk.final_gam.push((interval, final_bitmap.to_vec()));
            current = Some(interval);
        }
        let number = u32::try_from(extent).map_err(|_| Error::Limit("pages in the file"))?;
        for slot in 0..PAGES_PER_EXTENT as usize {
            let page: &[u8; PAGE_SIZE] = frame
                .get(slot * PAGE_SIZE..(slot + 1) * PAGE_SIZE)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(short)?;
            if page_integrity(page).is_corrupt() {
                return Err(Error::Corrupt {
                    page: number.saturating_mul(8).saturating_add(slot as u32),
                    what: "page of a frame of region 2",
                });
            }
        }
        if !marked(extent, current, &final_dcm) {
            if !options.accept_unmarked_frames {
                return Err(Error::UnprovenFrame { extent: number });
            }
            walk.unmarked.push(number);
        }
        walk.frames.push(number);
        previous = Some(extent);
        block += FRAME_BLOCKS;
    }
    check_padding(reader, region, block)?;
    Ok(walk)
}

pub struct PageMap<R: Read + Seek> {
    reader: R,
    region1: Region,
    region2: Option<Region>,
    intervals: Vec<Interval>,
    frames: Vec<u32>,
    unmarked: Vec<u32>,
    uncopied: Vec<u32>,
    header_size: u32,
    file_pages: u64,
    data_end: u64,
    checks: HeaderChecks,
    page_checks: PageChecks,
    verified: [u32; VERIFIED_SLOTS],
    pfs: Vec<(u32, Box<[u8]>)>,
}

impl<R: Read + Seek> std::fmt::Debug for PageMap<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageMap")
            .field("file_pages", &self.file_pages)
            .field("header_size", &self.header_size)
            .field("intervals", &self.intervals.len())
            .field("region2_frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> PageMap<R> {
    pub fn open(reader: R, limits: Limits) -> Result<Self, Error> {
        PageMap::open_with(reader, limits, OpenOptions::default())
    }

    pub fn open_with(mut reader: R, limits: Limits, options: OpenOptions) -> Result<Self, Error> {
        let mut magic = [0_u8; 8];
        reader.seek(SeekFrom::Start(0))?;
        if reader.read_exact(&mut magic).is_ok() && &magic == COMPRESSED_MAGIC {
            return Err(Error::Unsupported(
                "a compressed or encrypted backup (the file starts with MSSQLBAK)",
            ));
        }
        let info = media::inspect_checked(&mut reader).map_err(from_media)?;
        let regions: Vec<Region> = info
            .regions
            .iter()
            .filter(|region| region.pages != 0)
            .map(Region::from)
            .collect();
        if regions.len() > limits.max_regions {
            return Err(Error::Limit("data regions"));
        }
        if regions.len() > 2 {
            return Err(Error::Unsupported(
                "more than two data regions: only a full copy and a re-dump of changed extents are known",
            ));
        }
        if regions
            .iter()
            .any(|region| region.blocks > limits.max_pages)
        {
            return Err(Error::Limit("pages in a data region"));
        }
        let region1 = *regions
            .first()
            .ok_or(Error::Inconsistent("the backup holds no data region"))?;
        let walk = walk_region1(&mut reader, region1, &limits)?;
        let region2 = regions.get(1).copied();
        let vote_limit = (walk.intervals.len() as u64) * INTERVAL_PAGES;
        let walk2 = match region2 {
            Some(region) => walk_region2(&mut reader, region, vote_limit, &walk.highest, &options)?,
            None => Walk2::default(),
        };
        let region2_end = walk2
            .frames
            .last()
            .map_or(0, |extent| (u64::from(*extent) + 1) * PAGES_PER_EXTENT);
        let data_end = walk.derived_end.max(region2_end);
        let file_pages = u64::from(walk.header_size).max(data_end);
        if file_pages > limits.max_pages {
            return Err(Error::Limit("pages in the file"));
        }
        let mut uncopied: Vec<u32> = Vec::new();
        let mut gaps: Vec<u64> = Vec::new();
        for absent in &walk.absent {
            let in_region2 = u32::try_from(absent.extent)
                .is_ok_and(|extent| walk2.frames.binary_search(&extent).is_ok());
            let freed = absent.allocated
                && walk2
                    .final_gam
                    .binary_search_by_key(&(absent.extent / INTERVAL_EXTENTS), |(interval, _)| {
                        *interval
                    })
                    .ok()
                    .and_then(|index| walk2.final_gam.get(index))
                    .is_some_and(|(_, gam)| bit(gam, absent.extent % INTERVAL_EXTENTS));
            if !in_region2 && !freed {
                let extent =
                    u32::try_from(absent.extent).map_err(|_| Error::Limit("pages in the file"))?;
                if !options.accept_uncopied_extents {
                    return Err(Error::UncopiedExtent { extent });
                }
                uncopied.push(extent);
            } else if !in_region2 {
                gaps.push(absent.extent);
            }
        }
        let mut adopted: BTreeSet<u64> = walk.last_frames.iter().copied().collect();
        for gap in gaps {
            let Some(interval) = usize::try_from(gap / INTERVAL_EXTENTS)
                .ok()
                .and_then(|index| walk.intervals.get(index))
            else {
                continue;
            };
            let (below, above) = interval.neighbours(gap % INTERVAL_EXTENTS);
            for neighbour in [below, above].into_iter().flatten() {
                if let Some(block) = interval.block_of(neighbour) {
                    adopted.insert(block / FRAME_BLOCKS);
                }
            }
        }
        for frame in adopted {
            check_frame(&mut reader, region1, frame)?;
        }
        Ok(PageMap {
            reader,
            region1,
            region2,
            intervals: walk.intervals,
            frames: walk2.frames,
            unmarked: walk2.unmarked,
            uncopied,
            header_size: walk.header_size,
            file_pages,
            data_end,
            checks: info.header_checks.unwrap_or_default(),
            page_checks: PageChecks {
                verified: walk.checks.verified + walk2.checks.verified,
                unprotected: walk.checks.unprotected + walk2.checks.unprotected,
            },
            verified: [0; VERIFIED_SLOTS],
            pfs: Vec::new(),
        })
    }

    pub fn file_pages(&self) -> u64 {
        self.file_pages
    }

    pub fn header_size_pages(&self) -> u32 {
        self.header_size
    }

    pub fn region2_extents(&self) -> &[u32] {
        &self.frames
    }

    pub fn region2_unmarked_extents(&self) -> &[u32] {
        &self.unmarked
    }

    pub fn uncopied_extents(&self) -> &[u32] {
        &self.uncopied
    }

    pub fn mtf_header_checks(&self) -> HeaderChecks {
        self.checks
    }

    pub fn structural_page_checks(&self) -> PageChecks {
        self.page_checks
    }

    pub fn index_bytes(&self) -> usize {
        self.intervals
            .iter()
            .map(Interval::index_bytes)
            .sum::<usize>()
            + self.frames.len() * size_of::<u32>()
            + self.intervals.len() * size_of::<Interval>()
    }

    pub fn read_page(
        &mut self,
        page: u32,
        out: &mut [u8; PAGE_SIZE],
    ) -> Result<Option<PageOrigin>, Error> {
        let extent = page / PAGES_PER_EXTENT as u32;
        let slot = u64::from(page) % PAGES_PER_EXTENT;
        if let Ok(frame) = self.frames.binary_search(&extent) {
            let region = self.region2.ok_or(Error::Inconsistent(
                "a region 2 frame exists without a region 2",
            ))?;
            read_block(
                &mut self.reader,
                region,
                frame as u64 * FRAME_BLOCKS + slot,
                out,
            )?;
            return Ok(Some(PageOrigin::Region2));
        }
        let Some(lead) = self.lead_block(extent) else {
            out.fill(0);
            return Ok(None);
        };
        self.ensure_lead(extent, lead)?;
        read_block(&mut self.reader, self.region1, lead + slot, out)?;
        Ok(Some(PageOrigin::Region1))
    }

    fn lead_block(&self, extent: u32) -> Option<u64> {
        let extent = u64::from(extent);
        self.intervals
            .get(usize::try_from(extent / INTERVAL_EXTENTS).ok()?)?
            .block_of(extent % INTERVAL_EXTENTS)
    }

    fn ensure_lead(&mut self, extent: u32, lead: u64) -> Result<(), Error> {
        let slot = extent as usize % VERIFIED_SLOTS;
        if self.verified.get(slot) == Some(&(extent + 1)) {
            return Ok(());
        }
        let bytes = read_head(&mut self.reader, self.region1, lead)?;
        if !lead_matches(&bytes, u64::from(extent) * PAGES_PER_EXTENT) {
            return Err(LEAD_MISMATCH);
        }
        if let Some(entry) = self.verified.get_mut(slot) {
            *entry = extent + 1;
        }
        Ok(())
    }

    pub fn is_allocated(&mut self, page: u32) -> Result<bool, Error> {
        if u64::from(page) >= self.file_pages {
            return Ok(false);
        }
        let base = page / PFS_INTERVAL as u32 * PFS_INTERVAL as u32;
        let offset = (page - base) as usize;
        let cached = self
            .pfs
            .iter()
            .find(|(first, _)| *first == base)
            .and_then(|(_, bytes)| bytes.get(offset).copied());
        let byte = match cached {
            Some(byte) => byte,
            None => self.load_pfs(base, offset)?,
        };
        Ok(byte & PFS_ALLOCATED != 0)
    }

    fn load_pfs(&mut self, base: u32, offset: usize) -> Result<u8, Error> {
        let pfs_page = if base == 0 { 1 } else { base };
        let mut page = [0_u8; PAGE_SIZE];
        if self.read_page(pfs_page, &mut page)?.is_none() {
            if u64::from(base) >= self.data_end {
                return Ok(0);
            }
            return Err(Error::Inconsistent("a PFS page is not in the backup"));
        }
        check_integrity(&page, u64::from(pfs_page), "PFS", &mut self.page_checks)?;
        expect_page(
            &page,
            u64::from(pfs_page),
            Some(TYPE_PFS),
            "a page that should be a PFS page is not",
        )?;
        let bytes: Box<[u8]> = pfs_bytes(&page)?.into();
        let byte = bytes
            .get(offset)
            .copied()
            .ok_or(Error::Inconsistent("a PFS record is too short"))?;
        if self.pfs.len() >= PFS_CACHE {
            self.pfs.remove(0);
        }
        self.pfs.push((base, bytes));
        Ok(byte)
    }

    pub fn verify_layout(&mut self) -> Result<u64, Error> {
        let mut checked = 0_u64;
        for (index, interval) in self.intervals.iter().enumerate() {
            let first_page = index as u64 * INTERVAL_PAGES;
            let mut position = 0_u64;
            for (word_index, word) in interval.present.iter().enumerate() {
                let mut rest = *word;
                while rest != 0 {
                    let extent = word_index as u64 * 64 + u64::from(rest.trailing_zeros());
                    rest &= rest.wrapping_sub(1);
                    let bytes = read_head(
                        &mut self.reader,
                        self.region1,
                        interval.base + position * PAGES_PER_EXTENT,
                    )?;
                    if !lead_matches(&bytes, first_page + extent * PAGES_PER_EXTENT) {
                        return Err(LEAD_MISMATCH);
                    }
                    position += 1;
                    checked += 1;
                }
            }
        }
        Ok(checked)
    }
}
