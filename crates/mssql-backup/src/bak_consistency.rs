use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    io::{Read, Seek},
};

use crate::{
    bak_direct::{GateFacts, GateRefusal},
    bak_logscan::{self, Context, Limits, Operation, Record, Summary, TransactionId, page_lsn},
    bak_pagemap::PageMap,
    bak_read::ReadSet,
    media::{BackupHeader, LogRegion, Lsn},
};

pub const KNOWN_MAJOR_VERSIONS: [u8; 2] = [12, 16];
const FIRST_FILE: u16 = 1;
const EXTENT_PAGES: u32 = 8;
pub const MAX_FRAME_PAGES: usize = 1 << 20;
pub const MAX_OPEN_TRANSACTIONS: usize = 1 << 20;
pub const LISTED_OPEN_TRANSACTIONS: usize = 64;
pub const LISTED_PAGES: usize = 64;
const NAMED_PAGES: usize = 6;
const LISTED_KINDS: usize = 6;

#[derive(Debug, Clone, Default)]
pub struct Footprint {
    pages: Vec<(u32, u32)>,
    units: HashSet<u32>,
    frame_extents: BTreeSet<u32>,
    unmarked: usize,
    uncopied: usize,
    structural_unprotected: u32,
}

fn unit_word(auid: i64) -> u32 {
    let [_, _, a, b, c, d, _, _] = auid.to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}

fn runs_touch(pages: &[(u32, u32)], low: u32, high: u32) -> bool {
    let after = pages.partition_point(|(start, _)| *start <= high);
    after
        .checked_sub(1)
        .and_then(|index| pages.get(index))
        .is_some_and(|(_, end)| *end >= low)
}

impl Footprint {
    pub fn new(
        read_set: &ReadSet,
        region2_extents: &[u32],
        unmarked_region2_extents: &[u32],
        uncopied_extents: &[u32],
    ) -> Footprint {
        let pages: Vec<(u32, u32)> = read_set
            .page_ranges()
            .map(|run| (*run.start(), *run.end()))
            .collect();
        let touched = |extent: &u32| {
            extent.checked_mul(EXTENT_PAGES).is_some_and(|first| {
                runs_touch(&pages, first, first.saturating_add(EXTENT_PAGES - 1))
            })
        };
        let mut frame_extents: BTreeSet<u32> = region2_extents
            .iter()
            .chain(unmarked_region2_extents)
            .copied()
            .filter(touched)
            .collect();
        for run in read_set.region2_page_ranges() {
            frame_extents.extend(*run.start() / EXTENT_PAGES..=*run.end() / EXTENT_PAGES);
        }
        let unmarked = unmarked_region2_extents
            .iter()
            .filter(|extent| frame_extents.contains(extent))
            .count();
        Footprint {
            units: read_set
                .allocation_units()
                .iter()
                .map(|auid| unit_word(*auid))
                .collect(),
            pages,
            frame_extents,
            unmarked,
            uncopied: uncopied_extents.len(),
            structural_unprotected: 0,
        }
    }

    pub fn of_map<R: Read + Seek>(read_set: &ReadSet, map: &PageMap<R>) -> Footprint {
        Footprint::new(
            read_set,
            map.region2_extents(),
            map.region2_unmarked_extents(),
            map.uncopied_extents(),
        )
        .with_structure(map.structural_page_checks().unprotected)
    }

    #[must_use]
    pub fn with_structure(mut self, unprotected: u32) -> Footprint {
        self.structural_unprotected = unprotected;
        self
    }

    pub fn read_page(&self, page: u32) -> bool {
        runs_touch(&self.pages, page, page)
    }

    pub fn read_pages(&self, low: u32, high: u32) -> bool {
        runs_touch(&self.pages, low, high)
    }

    pub fn used_unit(&self, word: u32) -> bool {
        self.units.contains(&word)
    }

    pub fn frame_extents(&self) -> impl Iterator<Item = u32> + '_ {
        self.frame_extents.iter().copied()
    }

    pub fn frame_pages(&self) -> impl Iterator<Item = u32> + '_ {
        self.frame_extents
            .iter()
            .flat_map(|extent| {
                let first = extent.saturating_mul(EXTENT_PAGES);
                first..first.saturating_add(EXTENT_PAGES)
            })
            .filter(|page| self.read_page(*page))
    }

    pub fn page_count(&self) -> u64 {
        self.pages
            .iter()
            .map(|(start, end)| u64::from(end - start) + 1)
            .sum()
    }

    pub fn unit_count(&self) -> usize {
        self.units.len()
    }

    pub fn structural_pages_unprotected(&self) -> u32 {
        self.structural_unprotected
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Blocker {
    ReadPage,
    UsedUnit,
    UndecodedOperation(Operation),
    UnknownCounterUpdate,
    OtherFile,
    TransactionBeforeWindow,
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocker::ReadPage => f.write_str("on pages the conversion read"),
            Blocker::UsedUnit => f.write_str("in allocation units the conversion used"),
            Blocker::UndecodedOperation(operation) => {
                write!(
                    f,
                    "of the operation {}, which this reader does not decode",
                    operation.name()
                )
            }
            Blocker::UnknownCounterUpdate => {
                f.write_str("counter updates that this reader does not know")
            }
            Blocker::OtherFile => f.write_str("on a data file other than the first"),
            Blocker::TransactionBeforeWindow => {
                f.write_str("of transactions that began before the window")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocked {
    pub records: u64,
    pub first: Lsn,
    pub first_page: Option<u32>,
}

pub type BlockedKinds = BTreeMap<(Operation, Context), u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CounterAction {
    PageCounts,
    RowCount,
    ColumnModifications,
    LobPageCounts,
    OverflowPageCounts,
}

const COUNTER_ACTION_AT: usize = 48;
const COUNTER_ROWSET_AT: usize = 56;
const SYSALLOCUNITS_WORD: u32 = 7;
const SYSROWSETS_WORD: u32 = 5;
const SYSRSCOLS_WORD: u32 = 3;

pub fn counter_action(record: &Record<'_>) -> Option<(CounterAction, u64)> {
    let action = crate::bak_bytes::u32_at(record.bytes, COUNTER_ACTION_AT)?;
    let rowset = crate::bak_bytes::u64_at(record.bytes, COUNTER_ROWSET_AT)?;
    let word = record.page?.detail?.allocation_unit_word;
    let (action, table) = match action {
        0 => (CounterAction::PageCounts, SYSALLOCUNITS_WORD),
        1 => (CounterAction::RowCount, SYSROWSETS_WORD),
        2 => (CounterAction::ColumnModifications, SYSRSCOLS_WORD),
        3 => (CounterAction::LobPageCounts, SYSALLOCUNITS_WORD),
        4 => (CounterAction::OverflowPageCounts, SYSALLOCUNITS_WORD),
        _ => return None,
    };
    (word == table).then_some((action, rowset))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PfsEffect {
    Allocation { low: u32, high: u32 },
    Other,
}

const OFFSET_IN_ROW_AT: usize = 56;
const MODIFY_SIZE_AT: usize = 58;
const HEADER_ELEMENT_LENGTHS_AT: usize = 64;
const SET_BITS_FIRST_AT: usize = 48;
const SET_BITS_COUNT_AT: usize = 50;
const PFS_ROW_PREFIX: u32 = 4;
const PFS_PAGES: u32 = 8_088;
const FIRST_PFS_PAGE: u32 = 1;
const PFS_ALLOCATED_BIT: u32 = 6;
const HEADER_TYPE_FLAG_BITS: u16 = 2;
const MAX_SET_BITS: u32 = 64;

fn pfs_target(record: &Record<'_>) -> Option<(u32, u32, bool)> {
    use crate::bak_bytes::u16_at;
    if record.context != Context::Pfs {
        return None;
    }
    let pfs_page = record.page?.page;
    let base = if pfs_page == FIRST_PFS_PAGE {
        0
    } else {
        pfs_page
    };
    match record.operation {
        Operation::ModifyRow => {
            let offset = u32::from(u16_at(record.bytes, OFFSET_IN_ROW_AT)?);
            let size = u32::from(u16_at(record.bytes, MODIFY_SIZE_AT)?);
            let first = offset.checked_sub(PFS_ROW_PREFIX)?;
            let last = first.checked_add(size.checked_sub(1)?)?;
            (last < PFS_PAGES).then_some((base.checked_add(first)?, base.checked_add(last)?, true))
        }
        Operation::SetBits => {
            let first = u32::from(u16_at(record.bytes, SET_BITS_FIRST_AT)?);
            let count = u32::from(u16_at(record.bytes, SET_BITS_COUNT_AT)?);
            if count == 0 || count > MAX_SET_BITS {
                return None;
            }
            let last = first.checked_add(count - 1)?;
            let allocated = (first..=last).any(|bit| bit % 8 == PFS_ALLOCATED_BIT);
            let low_byte = (first / 8).checked_sub(PFS_ROW_PREFIX)?;
            let high_byte = (last / 8).checked_sub(PFS_ROW_PREFIX)?;
            (high_byte < PFS_PAGES).then_some((
                base.checked_add(low_byte)?,
                base.checked_add(high_byte)?,
                allocated,
            ))
        }
        _ => None,
    }
}

pub fn pfs_pages(record: &Record<'_>) -> Option<(u32, u32)> {
    pfs_target(record).map(|(low, high, _)| (low, high))
}

pub fn pfs_effect(record: &Record<'_>) -> Option<PfsEffect> {
    use crate::bak_bytes::u16_at;
    if record.context != Context::Pfs {
        return None;
    }
    if record.operation == Operation::ModifyHeader {
        let offset = u16_at(record.bytes, OFFSET_IN_ROW_AT)?;
        let lengths = (
            u16_at(record.bytes, HEADER_ELEMENT_LENGTHS_AT)?,
            u16_at(record.bytes, HEADER_ELEMENT_LENGTHS_AT + 2)?,
        );
        return (offset == HEADER_TYPE_FLAG_BITS && lengths == (1, 1)).then_some(PfsEffect::Other);
    }
    let (low, high, allocated) = pfs_target(record)?;
    Some(if allocated {
        PfsEffect::Allocation { low, high }
    } else {
        PfsEffect::Other
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct WindowFacts {
    pub first: Lsn,
    pub last: Lsn,
    pub scan: Summary,
    pub operations: BTreeMap<Operation, u64>,
    pub counter_updates: BTreeMap<CounterAction, u64>,
    pub blocked: BTreeMap<Blocker, Blocked>,
    pub blocked_kinds: BlockedKinds,
    pub open_transactions: Vec<TransactionId>,
    pub open_transaction_count: u64,
    pub passed_bookkeeping: u64,
    pub passed_differential_map: u64,
    pub passed_counter_updates: u64,
    pub passed_pfs_updates: u64,
    pub passed_outside_footprint: u64,
    pub footprint_pages: u64,
    pub footprint_units: u64,
    pub frame_extents_read: u64,
    pub unmarked_extents_read: u64,
    pub frame_pages_read: u64,
    pub frame_pages_identified: u64,
    pub frame_pages_unchanged: u64,
    pub frame_pages_unproven: Vec<u32>,
    pub frame_pages_unproven_count: u64,
}

impl WindowFacts {
    pub fn blocked_records(&self) -> u64 {
        self.blocked.values().map(|blocked| blocked.records).sum()
    }

    pub fn log_integrity(&self) -> LogIntegrity {
        if self.scan.blocks == 0 {
            LogIntegrity::NotRead
        } else if self.scan.blocks_without_checksum > 0 {
            LogIntegrity::Structure
        } else {
            LogIntegrity::Checksums
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogIntegrity {
    NotRead,
    Checksums,
    Structure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    Idle,
    OutsideTheFootprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Proof {
    pub kind: ProofKind,
    pub window: WindowFacts,
    pub structural_pages_unprotected: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caveat {
    LogBlocksUnprotected,
    NoLogStreamChecksum,
    StructureUnprotected { pages: u32 },
    OpenTransactions { count: u64 },
}

impl Proof {
    pub fn caveats(&self) -> Vec<Caveat> {
        let mut caveats = Vec::new();
        if self.window.scan.blocks_without_checksum > 0 {
            caveats.push(Caveat::LogBlocksUnprotected);
        }
        if self.window.scan.streams_without_checksum > 0 {
            caveats.push(Caveat::NoLogStreamChecksum);
        }
        if self.structural_pages_unprotected > 0 {
            caveats.push(Caveat::StructureUnprotected {
                pages: self.structural_pages_unprotected,
            });
        }
        if self.window.open_transaction_count > 0 {
            caveats.push(Caveat::OpenTransactions {
                count: self.window.open_transaction_count,
            });
        }
        caveats
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    NoHeader,
    UnknownSoftwareVersion { major: u8, minor: u8, build: u16 },
    FlaggedDamaged,
    Differential,
    LogRegionsIncomplete,
    LogNotRead(String),
    UncopiedExtents { count: usize },
    FramesNotShownGenuine { count: u64, pages: Vec<u32> },
    PageHeaderNotRead(String),
    Records,
    BusyWindowWithoutChecksums,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Refusal {
    pub reason: Reason,
    pub window: Option<WindowFacts>,
    pub structural_pages_unprotected: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Proven(Proof),
    Refused(Refusal),
}

impl Verdict {
    pub fn is_proven(&self) -> bool {
        matches!(self, Verdict::Proven(_))
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Reason::NoHeader => f.write_str(
                "the backup carries no header this reader understands, so the window of its transaction log cannot be checked (the log was not read)",
            )?,
            Reason::UnknownSoftwareVersion { major, minor, build } => write!(
                f,
                "the backup was made by SQL Server {major}.{minor} (build {build}); the check of the transaction log was verified on SQL Server 2014 (12) and 2022 (16) only (the log was not read)"
            )?,
            Reason::FlaggedDamaged => f.write_str(
                "the server flagged the backup as made from a damaged database (the log was not read)",
            )?,
            Reason::Differential => f.write_str(
                "this is a differential backup, not a full one (the log was not read)",
            )?,
            Reason::LogRegionsIncomplete => f.write_str(
                "the backup holds more transaction log streams than the reader keeps (the log was not read)",
            )?,
            Reason::LogNotRead(why) => write!(f, "the transaction log of the backup could not be read: {why}")?,
            Reason::UncopiedExtents { count } => write!(
                f,
                "the backup does not hold {count} extents that are allocated, so it cannot be shown to hold what a restore would produce"
            )?,
            Reason::FramesNotShownGenuine { count, pages } => {
                write!(
                    f,
                    "{count} pages that the conversion read from extents the backup copied a second time while it ran cannot be placed by its transaction log: the log has no record of the page with the sequence number that the page carries, or the page carries the number of a change that the log does not show (a copy of an extent shifted to another place and sealed again looks like a real one; the first pages:"
                )?;
                for page in pages.iter().take(NAMED_PAGES) {
                    write!(f, " {page}")?;
                }
                f.write_str(")")?;
            }
            Reason::PageHeaderNotRead(why) => write!(
                f,
                "the header of a page that the conversion read from an extent the backup copied a second time could not be read again to be compared with the transaction log: {why}"
            )?,
            Reason::BusyWindowWithoutChecksums => f.write_str(
                "the transaction log of the backup carries no block checksum (the database was set to PAGE_VERIFY NONE), and it holds records beyond the three checkpoint records; without checksums a damaged block could hide a record, so such a log is accepted only when it holds nothing else",
            )?,
            Reason::Records => f.write_str("the backup was written to while it ran")?,
        }
        let Some(window) = &self.window else {
            if self.structural_pages_unprotected > 0 {
                write!(
                    f,
                    "; {}",
                    structure_caveat_text(self.structural_pages_unprotected)
                )?;
            }
            return Ok(());
        };
        write!(
            f,
            "; the log window ({} records in {} blocks, {} bytes) was read",
            window.scan.records, window.scan.blocks, window.scan.bytes
        )?;
        if matches!(self.reason, Reason::Records) {
            f.write_str(", and")?;
            let mut first = true;
            for (blocker, blocked) in &window.blocked {
                write!(
                    f,
                    "{} {} records {}",
                    if first { "" } else { "," },
                    blocked.records,
                    blocker
                )?;
                first = false;
            }
            write!(
                f,
                " cannot be shown to leave alone what the reader read (the first at {}; the records are",
                window
                    .blocked
                    .values()
                    .map(|blocked| blocked.first)
                    .min()
                    .unwrap_or_default()
            )?;
            let mut kinds: Vec<_> = window.blocked_kinds.iter().collect();
            kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (index, ((operation, context), count)) in
                kinds.iter().take(LISTED_KINDS).enumerate()
            {
                write!(
                    f,
                    "{} {} {} in {}",
                    if index == 0 { "" } else { "," },
                    count,
                    operation.name(),
                    context.name()
                )?;
            }
            if kinds.len() > LISTED_KINDS {
                f.write_str(", and others")?;
            }
            f.write_str(")")?;
        }
        if window.open_transaction_count > 0 {
            write!(
                f,
                "; {} transactions were still open at its end",
                window.open_transaction_count
            )?;
        }
        if self.structural_pages_unprotected > 0 {
            write!(
                f,
                "; {}",
                structure_caveat_text(self.structural_pages_unprotected)
            )?;
        }
        Ok(())
    }
}

pub fn structure_caveat_text(pages: u32) -> String {
    format!(
        "{pages} structural pages of the backup (the file header and the allocation maps) carry no checksum (the database used PAGE_VERIFY NONE), so a damaged byte in them would go unnoticed"
    )
}

#[derive(Debug, Clone, Copy)]
pub struct LogInput<'a> {
    pub header: Option<&'a BackupHeader>,
    pub regions: &'a [LogRegion],
    pub regions_complete: bool,
}

struct Rules<'a> {
    footprint: &'a Footprint,
    facts: WindowFacts,
    open: HashSet<TransactionId>,
    overflowed: bool,
    sequence: Vec<Operation>,
    idle_possible: bool,
    frames: BTreeMap<u32, Frame>,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    header: Lsn,
    named: bool,
    identified: bool,
}

impl<'a> Rules<'a> {
    fn new(footprint: &'a Footprint, frames: BTreeMap<u32, Frame>) -> Rules<'a> {
        Rules {
            footprint,
            facts: WindowFacts {
                footprint_pages: footprint.page_count(),
                footprint_units: footprint.unit_count() as u64,
                ..WindowFacts::default()
            },
            open: HashSet::new(),
            overflowed: false,
            sequence: Vec::new(),
            idle_possible: true,
            frames,
        }
    }

    fn block(&mut self, blocker: Blocker, record: &Record<'_>) {
        *self
            .facts
            .blocked_kinds
            .entry((record.operation, record.context))
            .or_insert(0) += 1;
        let page = record.page.map(|page| page.page);
        self.facts
            .blocked
            .entry(blocker)
            .and_modify(|blocked| blocked.records += 1)
            .or_insert(Blocked {
                records: 1,
                first: record.lsn,
                first_page: page,
            });
    }

    fn record(&mut self, record: &Record<'_>) {
        use Operation::{
            AbortXact, BeginCkpt, BeginXact, ClearGamBits, CommitXact, CountDelta, EndCkpt,
            LockXact, MigrateLocks, PrepXact, SetBits, XactCkpt,
        };
        let operation = record.operation;
        *self.facts.operations.entry(operation).or_insert(0) += 1;
        if self.idle_possible {
            self.sequence.push(operation);
            if self.sequence.len() > 3 {
                self.idle_possible = false;
                self.sequence = Vec::new();
            }
        }
        let transaction = record.transaction;
        match operation {
            BeginXact => {
                if self.open.len() >= MAX_OPEN_TRANSACTIONS {
                    self.overflowed = true;
                } else {
                    self.open.insert(transaction);
                }
            }
            CommitXact | AbortXact => {
                if !self.open.remove(&transaction) && !transaction.is_none() {
                    self.block(Blocker::TransactionBeforeWindow, record);
                    return;
                }
            }
            _ => {
                if !transaction.is_none() && !self.open.contains(&transaction) && !self.overflowed {
                    self.block(Blocker::TransactionBeforeWindow, record);
                    return;
                }
            }
        }
        if let Some(page) = record.page
            && page.file != FIRST_FILE
        {
            self.block(Blocker::OtherFile, record);
            return;
        }
        if let Some(page) = record.page
            && let Some(frame) = self.frames.get_mut(&page.page)
        {
            frame.named = true;
            frame.identified |= record.lsn == frame.header;
        }
        match operation {
            BeginCkpt | XactCkpt | EndCkpt | BeginXact | CommitXact | AbortXact | PrepXact
            | LockXact => {
                self.facts.passed_bookkeeping += 1;
                return;
            }
            CountDelta => {
                match counter_action(record) {
                    Some((action, _)) => {
                        *self.facts.counter_updates.entry(action).or_insert(0) += 1;
                        self.facts.passed_counter_updates += 1;
                    }
                    None => self.block(Blocker::UnknownCounterUpdate, record),
                }
                return;
            }
            SetBits | ClearGamBits if record.context == Context::DiffMap => {
                self.facts.passed_differential_map += 1;
                return;
            }
            _ => {}
        }
        let Some(page) = record.page else {
            self.block(Blocker::UndecodedOperation(operation), record);
            return;
        };
        let footprint = self.footprint;
        if let Some(effect) = pfs_effect(record)
            && !page
                .detail
                .is_some_and(|detail| footprint.used_unit(detail.allocation_unit_word))
        {
            match effect {
                PfsEffect::Allocation { low, high } if footprint.read_pages(low, high) => {
                    self.block(Blocker::ReadPage, record);
                }
                PfsEffect::Allocation { .. } | PfsEffect::Other => {
                    self.facts.passed_pfs_updates += 1;
                }
            }
            return;
        }
        if footprint.read_page(page.page) {
            self.block(Blocker::ReadPage, record);
        } else if page
            .detail
            .is_some_and(|detail| footprint.used_unit(detail.allocation_unit_word))
        {
            self.block(Blocker::UsedUnit, record);
        } else if operation == MigrateLocks || page.detail.is_some() {
            self.facts.passed_outside_footprint += 1;
        } else {
            self.block(Blocker::UndecodedOperation(operation), record);
        }
    }

    fn idle(&self) -> bool {
        self.idle_possible
            && self.sequence
                == [
                    Operation::BeginCkpt,
                    Operation::XactCkpt,
                    Operation::EndCkpt,
                ]
    }
}

pub type PageLsns<'a> = dyn FnMut(u32) -> Result<Option<Lsn>, String> + 'a;

fn no_page_headers(_: u32) -> Result<Option<Lsn>, String> {
    Err("no source of page headers was given to the check".to_owned())
}

pub fn check<R: Read + Seek>(log: &mut R, input: &LogInput<'_>, footprint: &Footprint) -> Verdict {
    check_with_limits(log, input, footprint, Limits::DEFAULT)
}

pub fn check_with_limits<R: Read + Seek>(
    log: &mut R,
    input: &LogInput<'_>,
    footprint: &Footprint,
    limits: Limits,
) -> Verdict {
    check_pages_with_limits(log, input, footprint, limits, &mut no_page_headers)
}

pub fn check_pages<R: Read + Seek>(
    log: &mut R,
    input: &LogInput<'_>,
    footprint: &Footprint,
    header_of: &mut PageLsns<'_>,
) -> Verdict {
    check_pages_with_limits(log, input, footprint, Limits::DEFAULT, header_of)
}

pub fn check_map<R: Read + Seek, L: Read + Seek>(
    map: &mut PageMap<R>,
    read_set: &ReadSet,
    log: &mut L,
    input: &LogInput<'_>,
) -> Verdict {
    let footprint = Footprint::of_map(read_set, map);
    check_pages(log, input, &footprint, &mut |page| {
        let mut bytes = [0_u8; 8192];
        match map.read_page(page, &mut bytes) {
            Ok(Some(_)) => page_lsn(&bytes)
                .map(Some)
                .ok_or_else(|| "the page is cut off".to_owned()),
            Ok(None) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    })
}

fn frame_headers(
    footprint: &Footprint,
    header_of: &mut PageLsns<'_>,
) -> Result<BTreeMap<u32, Frame>, Reason> {
    if footprint.frame_pages().nth(MAX_FRAME_PAGES).is_some() {
        return Err(Reason::PageHeaderNotRead(format!(
            "the conversion read more than {MAX_FRAME_PAGES} pages from extents the backup copied a second time, more than the check reads the headers of"
        )));
    }
    let mut frames = BTreeMap::new();
    for page in footprint.frame_pages() {
        match header_of(page) {
            Ok(Some(header)) => {
                frames.insert(
                    page,
                    Frame {
                        header,
                        named: false,
                        identified: false,
                    },
                );
            }
            Ok(None) => {
                return Err(Reason::PageHeaderNotRead(format!(
                    "page {page} is not in the backup"
                )));
            }
            Err(why) => {
                return Err(Reason::PageHeaderNotRead(format!("page {page}: {why}")));
            }
        }
    }
    Ok(frames)
}

#[derive(Debug, Default)]
struct FrameVerdict {
    identified: u64,
    unchanged: u64,
    unproven: Vec<u32>,
    unproven_count: u64,
}

fn judge_frames(frames: &BTreeMap<u32, Frame>, start: Lsn) -> FrameVerdict {
    let mut verdict = FrameVerdict::default();
    for (page, frame) in frames {
        let shown = if frame.named {
            frame.identified
        } else {
            frame.header < start
        };
        if !shown {
            verdict.unproven_count += 1;
            if verdict.unproven.len() < LISTED_PAGES {
                verdict.unproven.push(*page);
            }
        } else if frame.named {
            verdict.identified += 1;
        } else {
            verdict.unchanged += 1;
        }
    }
    verdict
}

pub fn check_pages_with_limits<R: Read + Seek>(
    log: &mut R,
    input: &LogInput<'_>,
    footprint: &Footprint,
    limits: Limits,
    header_of: &mut PageLsns<'_>,
) -> Verdict {
    let structure = footprint.structural_pages_unprotected();
    let refuse = |reason: Reason| {
        Verdict::Refused(Refusal {
            reason,
            window: None,
            structural_pages_unprotected: structure,
        })
    };
    let Some(header) = input.header else {
        return refuse(Reason::NoHeader);
    };
    if !KNOWN_MAJOR_VERSIONS.contains(&header.software.major) {
        return refuse(Reason::UnknownSoftwareVersion {
            major: header.software.major,
            minor: header.software.minor,
            build: header.software.build,
        });
    }
    if header.is_damaged() {
        return refuse(Reason::FlaggedDamaged);
    }
    if header.is_differential() {
        return refuse(Reason::Differential);
    }
    if !input.regions_complete {
        return refuse(Reason::LogRegionsIncomplete);
    }
    if footprint.uncopied > 0 {
        return refuse(Reason::UncopiedExtents {
            count: footprint.uncopied,
        });
    }
    let frames = match frame_headers(footprint, header_of) {
        Ok(frames) => frames,
        Err(reason) => return refuse(reason),
    };
    let mut rules = Rules::new(footprint, frames);
    let scanned = bak_logscan::scan(
        log,
        input.regions,
        header.first_lsn,
        header.last_lsn,
        limits,
        |record| rules.record(record),
    );
    let summary = match scanned {
        Ok(summary) => summary,
        Err(error) => return refuse(Reason::LogNotRead(error.to_string())),
    };
    let idle = rules.idle();
    let start = summary
        .first_block
        .map_or(header.first_lsn, |(vlf, block)| Lsn {
            vlf,
            block,
            slot: 1,
        })
        .min(header.first_lsn);
    let frame_verdict = judge_frames(&rules.frames, start);
    let mut facts = rules.facts;
    facts.first = header.first_lsn;
    facts.last = header.last_lsn;
    facts.scan = summary;
    facts.open_transaction_count = rules.open.len() as u64;
    let mut open: Vec<TransactionId> = rules.open.iter().copied().collect();
    open.sort_unstable();
    open.truncate(LISTED_OPEN_TRANSACTIONS);
    facts.open_transactions = open;
    facts.frame_extents_read = footprint.frame_extents.len() as u64;
    facts.unmarked_extents_read = footprint.unmarked as u64;
    facts.frame_pages_read = rules.frames.len() as u64;
    facts.frame_pages_identified = frame_verdict.identified;
    facts.frame_pages_unchanged = frame_verdict.unchanged;
    facts.frame_pages_unproven = frame_verdict.unproven.clone();
    facts.frame_pages_unproven_count = frame_verdict.unproven_count;
    let refused = |reason: Reason, facts: WindowFacts| {
        Verdict::Refused(Refusal {
            reason,
            window: Some(facts),
            structural_pages_unprotected: structure,
        })
    };
    if rules.overflowed {
        return refused(
            Reason::LogNotRead(
                "more transactions were open at once than the gate keeps track of".to_owned(),
            ),
            facts,
        );
    }
    if !facts.blocked.is_empty() {
        return refused(Reason::Records, facts);
    }
    if frame_verdict.unproven_count > 0 {
        return refused(
            Reason::FramesNotShownGenuine {
                count: frame_verdict.unproven_count,
                pages: frame_verdict.unproven,
            },
            facts,
        );
    }
    let proven = |kind: ProofKind, facts: WindowFacts| {
        Verdict::Proven(Proof {
            kind,
            window: facts,
            structural_pages_unprotected: structure,
        })
    };
    if idle {
        return proven(ProofKind::Idle, facts);
    }
    if facts.scan.blocks_without_checksum > 0 {
        return refused(Reason::BusyWindowWithoutChecksums, facts);
    }
    proven(ProofKind::OutsideTheFootprint, facts)
}

pub fn consistency_gate() -> impl FnOnce(&GateFacts<'_>) -> Result<(), GateRefusal> {
    |facts| {
        let footprint = Footprint::new(
            facts.read_set,
            &facts.map.region2_extents,
            &facts.map.unmarked_region2_extents,
            &facts.map.uncopied_extents,
        )
        .with_structure(facts.map.structural_page_checks.unprotected);
        let input = LogInput {
            header: facts.backup.header.as_ref(),
            regions: &facts.backup.log_regions,
            regions_complete: facts.backup.log_regions_complete,
        };
        let mut container = facts.container.borrow_mut();
        let mut log: &mut dyn crate::bak_direct::ReadSeek = &mut *container;
        match check_pages(&mut log, &input, &footprint, &mut |page| {
            (facts.page_lsn)(page)
        }) {
            Verdict::Proven(proof) => {
                facts.outcome.record(proof);
                Ok(())
            }
            Verdict::Refused(refusal) => Err(GateRefusal {
                reason: refusal.to_string(),
            }),
        }
    }
}
