// Derived from bcdb 0.2.1, src/BcDb.Core/TableReader.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use std::collections::HashSet;

use crate::{
    bak_bytes::{i64_at, u8_at, u16_at},
    bak_checksum::PAGE_SIZE,
    bak_read::{
        Error, PageBytes, PageReader, PageSource, TYPE_BCM, TYPE_DATA, TYPE_DCM, TYPE_GAM,
        TYPE_IAM, TYPE_INDEX, TYPE_PFS, TYPE_SGAM,
    },
    bak_record::{
        HEADER_SIZE, PageHeader, PagePtr, PhysSlot, RecordKind, RecordSpan, carries_version_info,
        decode_cells, has_unreleased_version, parse_fixed_var, parse_forwarded, record_room,
        record_spans, records, stub_target,
    },
    bak_values::{ColumnDef, SqlType, Value, decode_value},
    records::FixedVarRecord,
};

pub const MAX_IAM_PAGES: usize = 1024;
pub const MAX_CHAIN_PAGES: usize = 1 << 18;
const INTERVAL_EXTENTS: u32 = 63_904;
const INTERVAL_PAGES: u32 = INTERVAL_EXTENTS * 8;
const PFS_INTERVAL: u32 = 8_088;
pub const MAX_LEAF_PAGES: usize = 1 << 21;
const IAM_HEADER_LEN: usize = 94;
const IAM_START_AT: usize = 40;
const IAM_SINGLES_AT: usize = 46;
const IAM_SINGLES: usize = 8;
const IAM_BITMAP_RECORD: usize = 7_992;
const IAM_BITMAP_HEAD: usize = 4;
const EXTENT_TYPES: &[u8] = &[
    TYPE_DATA, TYPE_INDEX, TYPE_IAM, TYPE_GAM, TYPE_SGAM, TYPE_PFS, TYPE_DCM, TYPE_BCM,
];
const FLAG_COMPRESSED_PAGE: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    InRow,
    Lob,
    RowOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocUnit {
    pub id: i64,
    pub kind: UnitKind,
    pub owner: i64,
    pub first_page: PagePtr,
    pub root_page: PagePtr,
    pub first_iam: PagePtr,
}

impl AllocUnit {
    pub fn identity(&self) -> i64 {
        self.id & !0xFFFF
    }

    pub fn from_fixed(fixed: &[u8]) -> Result<AllocUnit, Error> {
        let cut = || Error::Inconsistent("an allocation unit record is cut off");
        let id = i64_at(fixed, 0).ok_or_else(cut)?;
        let code = u8_at(fixed, 8).ok_or_else(cut)?;
        let owner = i64_at(fixed, 9).ok_or_else(cut)?;
        let first_page = PagePtr::from_bytes(fixed, 23).ok_or_else(cut)?;
        let root_page = PagePtr::from_bytes(fixed, 29).ok_or_else(cut)?;
        let first_iam = PagePtr::from_bytes(fixed, 35).ok_or_else(cut)?;
        let kind = match code {
            1 => UnitKind::InRow,
            2 => UnitKind::Lob,
            3 => UnitKind::RowOverflow,
            _ => return Err(Error::Unsupported("an allocation unit of an unknown type")),
        };
        Ok(AllocUnit {
            id,
            kind,
            owner,
            first_page,
            root_page,
            first_iam,
        })
    }
}

#[derive(Debug)]
struct Interval {
    base: u32,
    singles: Vec<u32>,
    bitmap: Vec<u8>,
}

impl Interval {
    fn has_extent(&self, extent: u32) -> bool {
        extent < INTERVAL_EXTENTS
            && self
                .bitmap
                .get(usize::try_from(extent / 8).unwrap_or(usize::MAX))
                .is_some_and(|byte| byte & (1 << (extent % 8)) != 0)
    }

    fn last_extent(&self) -> Option<u32> {
        let at = self.bitmap.iter().rposition(|byte| *byte != 0)?;
        let byte = *self.bitmap.get(at)?;
        let bit = 7_u32.checked_sub(byte.leading_zeros())?;
        u32::try_from(at).ok()?.checked_mul(8)?.checked_add(bit)
    }

    fn next_extent(&self, from: u32) -> Option<u32> {
        let mut extent = from;
        while extent < INTERVAL_EXTENTS {
            let byte = *self
                .bitmap
                .get(usize::try_from(extent / 8).unwrap_or(usize::MAX))?;
            if byte == 0 {
                extent = (extent / 8 + 1) * 8;
            } else if byte & (1 << (extent % 8)) != 0 {
                return Some(extent);
            } else {
                extent += 1;
            }
        }
        None
    }
}

#[derive(Debug)]
pub struct UnitMap {
    intervals: Vec<Interval>,
    iam_pages: Vec<u32>,
    identity: i64,
}

enum Candidate {
    Data,
    Skip,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PageCursor {
    interval: usize,
    single: usize,
    extent: u32,
    sub: u32,
}

impl UnitMap {
    pub fn load<S: PageSource>(
        reader: &mut PageReader<S>,
        unit: &AllocUnit,
    ) -> Result<UnitMap, Error> {
        let mut map = UnitMap {
            intervals: Vec::new(),
            iam_pages: Vec::new(),
            identity: unit.identity(),
        };
        let mut page = Box::new([0_u8; PAGE_SIZE]);
        let mut next = unit.first_iam;
        while !next.is_null() {
            if next.file != 1 {
                return Err(Error::Unsupported(
                    "an IAM page in a file other than file 1",
                ));
            }
            if map.iam_pages.contains(&next.page) {
                return Err(Error::Inconsistent("the IAM chain of a unit loops"));
            }
            if map.iam_pages.len() >= MAX_IAM_PAGES {
                return Err(Error::Limit(
                    "IAM pages in the chain of one allocation unit",
                ));
            }
            if reader.read(next.page, &[TYPE_IAM], &mut page)?.is_none() {
                return Err(Error::Inconsistent("an IAM page is not in the backup"));
            }
            let header = PageHeader::parse(&page, next.page)?;
            if header.unit_identity() != map.identity {
                return Err(Error::Malformed {
                    page: next.page,
                    what: "an IAM page of another allocation unit (its header names another unit than the one that leads to it)",
                });
            }
            let interval = parse_iam(&page, &header, next.page)?;
            if map
                .intervals
                .iter()
                .any(|known| known.base == interval.base)
            {
                return Err(Error::Inconsistent(
                    "two IAM pages of one unit cover one interval",
                ));
            }
            map.iam_pages.push(next.page);
            map.intervals.push(interval);
            next = header.next;
        }
        map.check_named_pages(reader.file_pages())?;
        Ok(map)
    }

    pub fn iam_pages(&self) -> &[u32] {
        &self.iam_pages
    }

    pub fn identity(&self) -> i64 {
        self.identity
    }

    pub fn contains(&self, page: u32) -> bool {
        self.intervals.iter().any(|i| i.singles.contains(&page)) || self.extent_holds(page)
    }

    fn extent_holds(&self, page: u32) -> bool {
        let base = page / INTERVAL_PAGES * INTERVAL_PAGES;
        self.intervals
            .iter()
            .find(|interval| interval.base == base)
            .is_some_and(|interval| interval.has_extent((page - base) / 8))
    }

    fn check_named_pages(&self, file_pages: u64) -> Result<(), Error> {
        const BEYOND: Error = Error::Inconsistent(
            "an IAM page names a page beyond the end of the database file (the end the backup knows)",
        );
        let mut singles: Vec<u32> = self
            .intervals
            .iter()
            .flat_map(|interval| interval.singles.iter().copied())
            .collect();
        singles.sort_unstable();
        if singles.windows(2).any(|pair| pair.first() == pair.get(1)) {
            return Err(Error::Inconsistent(
                "an IAM page names one page twice (two single-page slots)",
            ));
        }
        for &single in &singles {
            if u64::from(single) >= file_pages {
                return Err(BEYOND);
            }
            if self.extent_holds(single) {
                return Err(Error::Inconsistent(
                    "an IAM page names a page twice (a single-page slot inside an extent of the unit)",
                ));
            }
        }
        for interval in &self.intervals {
            let Some(last) = interval.last_extent() else {
                continue;
            };
            let first_page = u64::from(interval.base) + u64::from(last) * 8;
            if first_page >= file_pages {
                return Err(BEYOND);
            }
        }
        Ok(())
    }

    fn classify(
        &self,
        page: u32,
        header: &PageHeader,
        bytes: &PageBytes,
    ) -> Result<Candidate, Error> {
        let own = |header: &PageHeader| header.unit_identity() == self.identity;
        match header.page_type {
            TYPE_DATA if own(header) => Ok(Candidate::Data),
            TYPE_INDEX if own(header) => {
                let only_index_records = record_spans(bytes, header, page)?
                    .iter()
                    .all(|span| matches!(span.kind, RecordKind::Index | RecordKind::GhostIndex));
                if only_index_records {
                    Ok(Candidate::Skip)
                } else {
                    Err(Error::Malformed {
                        page,
                        what: "an index page that holds records of rows (a data page of another type?)",
                    })
                }
            }
            TYPE_DATA | TYPE_INDEX => Err(Error::Malformed {
                page,
                what: "a page of another allocation unit among the pages of this one (the unit in its header is not this one)",
            }),
            TYPE_IAM if self.iam_pages.contains(&page) => Ok(Candidate::Skip),
            TYPE_IAM => Err(Error::Malformed {
                page,
                what: "an IAM page that the IAM chain of the unit does not lead to",
            }),
            TYPE_PFS | TYPE_GAM | TYPE_SGAM | TYPE_DCM | TYPE_BCM
                if allocation_page_is_in_place(header.page_type, page) =>
            {
                Ok(Candidate::Skip)
            }
            TYPE_PFS | TYPE_GAM | TYPE_SGAM | TYPE_DCM | TYPE_BCM => Err(Error::Malformed {
                page,
                what: "an allocation map page away from the place the engine puts it",
            }),
            _ => Err(Error::Malformed {
                page,
                what: "a page of a type that does not belong to the extents of a table",
            }),
        }
    }

    pub fn next_candidate(&self, cursor: &mut PageCursor) -> Result<Option<u32>, Error> {
        loop {
            let Some(interval) = self.intervals.get(cursor.interval) else {
                return Ok(None);
            };
            if let Some(&single) = interval.singles.get(cursor.single) {
                cursor.single += 1;
                return Ok(Some(single));
            }
            if cursor.sub == 0 {
                match interval.next_extent(cursor.extent) {
                    Some(extent) => cursor.extent = extent,
                    None => {
                        *cursor = PageCursor {
                            interval: cursor.interval + 1,
                            ..PageCursor::default()
                        };
                        continue;
                    }
                }
            }
            let page = cursor
                .extent
                .checked_mul(8)
                .and_then(|first| first.checked_add(cursor.sub))
                .and_then(|offset| interval.base.checked_add(offset))
                .ok_or(Error::Inconsistent("a page number beyond 2^32"))?;
            cursor.sub += 1;
            if cursor.sub == 8 {
                cursor.sub = 0;
                cursor.extent += 1;
            }
            return Ok(Some(page));
        }
    }

    pub fn candidates(&self) -> Candidates<'_> {
        Candidates {
            map: self,
            cursor: PageCursor::default(),
            failed: false,
        }
    }
}

fn allocation_page_is_in_place(kind: u8, page: u32) -> bool {
    let within = page % INTERVAL_PAGES;
    let first_interval = page < INTERVAL_PAGES;
    match kind {
        TYPE_PFS => page == 1 || (page != 0 && page.is_multiple_of(PFS_INTERVAL)),
        TYPE_GAM => within == if first_interval { 2 } else { 0 },
        TYPE_SGAM => within == if first_interval { 3 } else { 1 },
        TYPE_DCM => within == 6,
        TYPE_BCM => within == 7,
        _ => false,
    }
}

pub struct Candidates<'a> {
    map: &'a UnitMap,
    cursor: PageCursor,
    failed: bool,
}

impl Iterator for Candidates<'_> {
    type Item = Result<u32, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.map.next_candidate(&mut self.cursor) {
            Ok(page) => page.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

fn parse_iam(page: &PageBytes, header: &PageHeader, page_no: u32) -> Result<Interval, Error> {
    let list = records(page, header, page_no)?;
    let [head, bitmap] = list.as_slice() else {
        return Err(Error::Inconsistent("an IAM page does not have two records"));
    };
    let cut = || Error::Inconsistent("a record of an IAM page is cut off");
    if head.bytes.len() < IAM_HEADER_LEN {
        return Err(cut());
    }
    let start = PagePtr::from_bytes(head.bytes, IAM_START_AT).ok_or_else(cut)?;
    if start.file > 1 {
        return Err(Error::Unsupported(
            "an IAM page for an interval in a file other than file 1",
        ));
    }
    if start.page % INTERVAL_PAGES != 0 {
        return Err(Error::Inconsistent(
            "an IAM page does not start on the boundary of a GAM interval",
        ));
    }
    let mut singles = Vec::new();
    for slot in 0..IAM_SINGLES {
        let single = PagePtr::from_bytes(head.bytes, IAM_SINGLES_AT + 6 * slot).ok_or_else(cut)?;
        if single.is_null() {
            continue;
        }
        if single.file != 1 {
            return Err(Error::Unsupported(
                "a single-page slot of an IAM page names a page in another file",
            ));
        }
        singles.push(single.page);
    }
    let length = usize::from(u16_at(bitmap.bytes, 2).ok_or_else(cut)?);
    if length != IAM_BITMAP_RECORD || length > bitmap.bytes.len() {
        return Err(Error::Inconsistent(
            "the bitmap record of an IAM page is not the size of a bitmap",
        ));
    }
    let bits = bitmap
        .bytes
        .get(IAM_BITMAP_HEAD..length)
        .ok_or_else(cut)?
        .to_vec();
    Ok(Interval {
        base: start.page,
        singles,
        bitmap: bits,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct LiveRecord<'a> {
    pub page: u32,
    pub slot: u16,
    pub kind: RecordKind,
    pub bytes: &'a [u8],
}

impl<'a> LiveRecord<'a> {
    pub fn frame(&self) -> Result<FixedVarRecord<'a>, Error> {
        match self.kind {
            RecordKind::Forwarded => {
                parse_forwarded(self.page, self.bytes).map(|(record, _)| record)
            }
            _ => parse_fixed_var(self.page, self.bytes),
        }
    }
}

enum Order {
    Map {
        map: UnitMap,
        cursor: PageCursor,
    },
    Chain {
        next: PagePtr,
        seen: HashSet<u32>,
        identity: Option<i64>,
        rules: Option<ChainRules>,
    },
}

#[derive(Debug)]
struct ChainRules {
    expected_prev: PagePtr,
    pages: u64,
    first_empty: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Leaf {
    page: u32,
    prev: PagePtr,
    next: PagePtr,
    slots: u16,
}

#[derive(Debug)]
struct LeafLevel {
    pages: Vec<Leaf>,
    limit: usize,
}

impl LeafLevel {
    fn add(&mut self, page: u32, header: &PageHeader) -> Result<(), Error> {
        if self.pages.len() >= self.limit {
            return Err(Error::Limit("leaf pages of a clustered index"));
        }
        self.pages.push(Leaf {
            page,
            prev: header.prev,
            next: header.next,
            slots: header.slot_count,
        });
        Ok(())
    }

    fn check(mut self) -> Result<(), Error> {
        let mut pages = std::mem::take(&mut self.pages);
        pages.sort_unstable_by_key(|leaf| leaf.page);
        let total = pages.len();
        if total == 0 {
            return Ok(());
        }
        if let Some(twice) = pages.windows(2).find_map(|pair| match pair {
            [first, second] if first.page == second.page => Some(first.page),
            _ => None,
        }) {
            return Err(leaf_chain(
                twice,
                format!("page {twice} was read twice by one scan"),
            ));
        }
        let mut heads: Vec<u32> = Vec::new();
        for leaf in &pages {
            let here = PagePtr {
                page: leaf.page,
                file: 1,
            };
            if leaf.prev.is_null() {
                heads.push(leaf.page);
            } else {
                match find_leaf(&pages, leaf.prev) {
                    None => return Err(unknown_link(leaf.page, "previous", leaf.prev)),
                    Some(prev) if prev.next != here => {
                        return Err(unanswered_link(leaf.page, "previous", leaf.prev, prev.next));
                    }
                    Some(_) => {}
                }
            }
            if !leaf.next.is_null() {
                match find_leaf(&pages, leaf.next) {
                    None => return Err(unknown_link(leaf.page, "next", leaf.next)),
                    Some(next) if next.prev != here => {
                        return Err(unanswered_link(leaf.page, "next", leaf.next, next.prev));
                    }
                    Some(_) => {}
                }
            }
        }
        let head = match heads.as_slice() {
            [head] => *head,
            _ => return Err(ends_of_chain(&pages, &heads, total)),
        };
        let mut reached = vec![false; total];
        let mut visited = 0_usize;
        let mut at = PagePtr {
            page: head,
            file: 1,
        };
        for _ in 0..total {
            let Some(index) = position_of_leaf(&pages, at) else {
                break;
            };
            match reached.get_mut(index) {
                Some(seen) if !*seen => *seen = true,
                _ => break,
            }
            visited += 1;
            let Some(leaf) = pages.get(index) else { break };
            at = leaf.next;
        }
        if visited != total {
            let missed = reached
                .iter()
                .position(|seen| !seen)
                .and_then(|index| pages.get(index))
                .map_or(head, |leaf| leaf.page);
            return Err(leaf_chain(
                missed,
                format!(
                    "the walk from the first leaf page (page {head}) by the next pointers reaches {visited} of the {total} leaf pages that were read; page {missed} is not on it (a ring of pages that closes on itself, or a second chain)"
                ),
            ));
        }
        match pages.iter().find(|leaf| leaf.slots == 0) {
            Some(empty) if total > 1 => Err(empty_leaf(empty.page, total)),
            _ => Ok(()),
        }
    }
}

impl ChainRules {
    fn read(&mut self, page: u32, header: &PageHeader) -> Result<(), Error> {
        if header.prev != self.expected_prev {
            let what = if self.pages == 0 {
                format!(
                    "page {page} is where the chain starts, and says it has a previous page, {}",
                    show(header.prev)
                )
            } else {
                format!(
                    "page {page} says its previous page is {}, but the chain came to it from {}",
                    show(header.prev),
                    show(self.expected_prev)
                )
            };
            return Err(leaf_chain(page, what));
        }
        self.expected_prev = PagePtr { page, file: 1 };
        self.pages = self.pages.saturating_add(1);
        if header.slot_count == 0 && self.first_empty.is_none() {
            self.first_empty = Some(page);
        }
        Ok(())
    }

    fn check_end(&self) -> Result<(), Error> {
        match self.first_empty {
            Some(empty) if self.pages > 1 => Err(empty_leaf(empty, self.pages)),
            _ => Ok(()),
        }
    }
}

const CHAIN_CAUSES: &str = " (a backup written to while it ran can look like this, and so can an idle database for about a minute after the engine's own ghost cleanup, for example after a purge job: back up again after a minute, or use --server to let SQL Server read the backup)";

fn leaf_chain(page: u32, what: String) -> Error {
    Error::LeafChain {
        page,
        what: what + CHAIN_CAUSES,
    }
}

fn show(pointer: PagePtr) -> String {
    format!("({}:{})", pointer.file, pointer.page)
}

fn position_of_leaf(pages: &[Leaf], to: PagePtr) -> Option<usize> {
    if to.file != 1 {
        return None;
    }
    pages.binary_search_by_key(&to.page, |leaf| leaf.page).ok()
}

fn find_leaf(pages: &[Leaf], to: PagePtr) -> Option<&Leaf> {
    position_of_leaf(pages, to).and_then(|index| pages.get(index))
}

fn unknown_link(page: u32, which: &str, to: PagePtr) -> Error {
    leaf_chain(
        page,
        format!(
            "page {page} says its {which} page is {}, which is not a leaf page of this unit that the scan read (a page that the allocation maps do not give to it, in another file, or of another kind)",
            show(to)
        ),
    )
}

fn unanswered_link(page: u32, which: &str, to: PagePtr, answer: PagePtr) -> Error {
    let back = if which == "next" { "previous" } else { "next" };
    leaf_chain(
        page,
        format!(
            "page {page} says its {which} page is {}, but the {back} page of that page is {}",
            show(to),
            show(answer)
        ),
    )
}

fn ends_of_chain(pages: &[Leaf], heads: &[u32], total: usize) -> Error {
    let first = heads
        .first()
        .copied()
        .or_else(|| pages.first().map(|leaf| leaf.page))
        .unwrap_or_default();
    let text = if heads.is_empty() {
        format!(
            "none of the {total} leaf pages is the first one: every page has a previous page (the chain has no head and closes on itself)"
        )
    } else {
        let named: Vec<String> = heads.iter().take(4).map(u32::to_string).collect();
        format!(
            "{} leaf pages have no previous page, so there are {} chains, or the head of one is missing (pages {}{})",
            heads.len(),
            heads.len(),
            named.join(", "),
            if heads.len() > named.len() {
                ", ..."
            } else {
                ""
            }
        )
    };
    leaf_chain(first, text)
}

fn empty_leaf(page: u32, total: impl std::fmt::Display) -> Error {
    leaf_chain(
        page,
        format!(
            "leaf page {page} has no record, among {total} leaf pages (the engine does not leave an empty leaf page in a table of several; its rows may have been hidden)"
        ),
    )
}

pub struct RecordScan {
    order: Order,
    page: Box<PageBytes>,
    page_no: u32,
    spans: Vec<RecordSpan>,
    next_span: usize,
    stubs: u64,
    forwarded: u64,
    finished: bool,
    leaves: Option<LeafLevel>,
    accept_unreleased: bool,
    unreleased: u64,
}

impl RecordScan {
    fn with_order(order: Order) -> RecordScan {
        RecordScan {
            order,
            page: Box::new([0_u8; PAGE_SIZE]),
            page_no: 0,
            spans: Vec::new(),
            next_span: 0,
            stubs: 0,
            forwarded: 0,
            finished: false,
            leaves: None,
            accept_unreleased: false,
            unreleased: 0,
        }
    }

    pub fn accepting_unreleased_versions(mut self) -> RecordScan {
        self.accept_unreleased = true;
        self
    }

    pub fn unreleased_records(&self) -> u64 {
        self.unreleased
    }

    pub fn checking_leaf_chain(mut self) -> RecordScan {
        self.leaves = Some(LeafLevel {
            pages: Vec::new(),
            limit: MAX_LEAF_PAGES,
        });
        self
    }

    pub fn with_leaf_limit(mut self, pages: usize) -> RecordScan {
        if let Some(level) = self.leaves.as_mut() {
            level.limit = pages;
        }
        self
    }

    pub fn new(map: UnitMap) -> RecordScan {
        RecordScan::with_order(Order::Map {
            map,
            cursor: PageCursor::default(),
        })
    }

    pub fn chain(first: PagePtr) -> RecordScan {
        RecordScan::with_order(Order::Chain {
            next: first,
            seen: HashSet::new(),
            identity: None,
            rules: None,
        })
    }

    pub fn chain_of(first: PagePtr, unit_id: i64) -> RecordScan {
        RecordScan::with_order(Order::Chain {
            next: first,
            seen: HashSet::new(),
            identity: Some(unit_id & !0xFFFF),
            rules: Some(ChainRules {
                expected_prev: PagePtr::NULL,
                pages: 0,
                first_empty: None,
            }),
        })
    }

    fn load_next_page<S: PageSource>(&mut self, reader: &mut PageReader<S>) -> Result<bool, Error> {
        loop {
            let (candidate, expected): (u32, &'static [u8]) = match &mut self.order {
                Order::Map { map, cursor } => {
                    let Some(candidate) = map.next_candidate(cursor)? else {
                        return Ok(false);
                    };
                    if !reader.is_allocated(candidate)? {
                        continue;
                    }
                    (candidate, EXTENT_TYPES)
                }
                Order::Chain { next, seen, .. } => {
                    if next.is_null() {
                        return Ok(false);
                    }
                    if next.file != 1 {
                        return Err(Error::Unsupported("a page chain that leaves file 1"));
                    }
                    if !seen.insert(next.page) {
                        return Err(Error::Inconsistent("a page chain loops"));
                    }
                    if seen.len() > MAX_CHAIN_PAGES {
                        return Err(Error::Limit("pages in a chain"));
                    }
                    if !reader.is_allocated(next.page)? {
                        return Err(Error::Inconsistent("a page of a chain is not allocated"));
                    }
                    (next.page, &[TYPE_DATA])
                }
            };
            if reader.read(candidate, expected, &mut self.page)?.is_none() {
                return Err(Error::Inconsistent(
                    "a page that the allocation maps give to the unit is not in the backup",
                ));
            }
            let header = PageHeader::parse(&self.page, candidate)?;
            match &mut self.order {
                Order::Chain {
                    next,
                    identity,
                    rules,
                    ..
                } => {
                    *next = header.next;
                    if *identity.get_or_insert(header.unit_identity()) != header.unit_identity() {
                        return Err(Error::Malformed {
                            page: candidate,
                            what: "a page of a chain that says it belongs to another allocation unit than the chain",
                        });
                    }
                    if let Some(rules) = rules.as_mut() {
                        rules.read(candidate, &header)?;
                    }
                }
                Order::Map { map, .. } => {
                    if matches!(
                        map.classify(candidate, &header, &self.page)?,
                        Candidate::Skip
                    ) {
                        continue;
                    }
                }
            }
            if header.level != 0 {
                return Err(Error::Malformed {
                    page: candidate,
                    what: "a data page above the leaf level",
                });
            }
            if header.type_flags & FLAG_COMPRESSED_PAGE != 0 {
                return Err(Error::Unsupported("page or row compression"));
            }
            if let Some(level) = self.leaves.as_mut() {
                level.add(candidate, &header)?;
            }
            self.spans = record_spans(&self.page, &header, candidate)?;
            check_page_accounting(&self.page, &header, candidate, &self.spans)?;
            self.next_span = 0;
            self.page_no = candidate;
            return Ok(true);
        }
    }

    pub fn next<S: PageSource>(
        &mut self,
        reader: &mut PageReader<S>,
    ) -> Result<Option<LiveRecord<'_>>, Error> {
        let span = loop {
            let Some(span) = self.spans.get(self.next_span).copied() else {
                if self.finished {
                    return Ok(None);
                }
                if self.load_next_page(reader)? {
                    continue;
                }
                self.finished = true;
                if let Some(level) = self.leaves.take() {
                    level.check()?;
                }
                if let Order::Chain {
                    rules: Some(rules), ..
                } = &self.order
                {
                    rules.check_end()?;
                }
                if self.stubs != self.forwarded {
                    return Err(Error::Inconsistent(
                        "the forwarding stubs of a heap and its forwarded records are not equally many",
                    ));
                }
                return Ok(None);
            };
            self.next_span += 1;
            let bytes = span.bytes(&self.page).ok_or(Error::Malformed {
                page: self.page_no,
                what: "a record runs past the page",
            })?;
            if u8_at(bytes, 0).is_some_and(|status| status & 1 != 0) {
                return Err(Error::Unsupported("page or row compression"));
            }
            if has_unreleased_version(span.kind, bytes) {
                if !self.accept_unreleased {
                    return Err(Error::Unsupported(UNRELEASED_VERSION));
                }
                self.unreleased = self.unreleased.saturating_add(1);
            }
            match span.kind {
                RecordKind::ForwardingStub => {
                    let target = stub_target(self.page_no, bytes)?;
                    check_in_file(reader.file_pages(), target.file, target.page)?;
                    self.stubs += 1;
                }
                RecordKind::Forwarded => {
                    let (_, back) = parse_forwarded(self.page_no, bytes)?;
                    check_in_file(reader.file_pages(), back.file, back.page)?;
                    self.forwarded += 1;
                    break span;
                }
                RecordKind::Primary => break span,
                RecordKind::Index | RecordKind::BlobFragment => {
                    return Err(Error::Malformed {
                        page: self.page_no,
                        what: "a record of a kind that does not belong on a data page",
                    });
                }
                RecordKind::GhostIndex | RecordKind::GhostData | RecordKind::GhostVersion => {}
            }
        };
        let bytes = span.bytes(&self.page).ok_or(Error::Malformed {
            page: self.page_no,
            what: "a record runs past the page",
        })?;
        Ok(Some(LiveRecord {
            page: self.page_no,
            slot: span.slot,
            kind: span.kind,
            bytes,
        }))
    }
}

const UNRELEASED_VERSION: &str = "a row carries version information that has not been released: a database with accelerated database recovery keeps it until its persistent version store cleaner has run, often for good, and nothing in a page says whether the row belongs to a transaction that was rolled back (a changed row can be a rolled back one, a deleted row can be live, a row can be one that never was)";

const MIN_RECORD_ROOM: usize = 9;

fn check_page_accounting(
    page: &PageBytes,
    header: &PageHeader,
    page_no: u32,
    spans: &[RecordSpan],
) -> Result<PageFigures, Error> {
    let versioned = spans
        .iter()
        .any(|span| span.bytes(page).is_some_and(carries_version_info));
    let refuse = |plain: &'static str, with_versions: &'static str| Error::PageAccounting {
        page: page_no,
        what: if versioned { with_versions } else { plain },
    };
    let mut ghosts = 0_usize;
    let mut compressed = false;
    let mut room = Some(0_usize);
    for span in spans {
        let bytes = span.bytes(page).ok_or(Error::Malformed {
            page: page_no,
            what: "a record runs past the page",
        })?;
        compressed |= u8_at(bytes, 0).is_some_and(|status| status & 1 != 0);
        let ghost_forwarded =
            span.kind == RecordKind::Forwarded && u8_at(bytes, 1).is_some_and(|b| b & 1 != 0);
        if span.kind.is_ghost() || ghost_forwarded {
            ghosts += 1;
        }
        let record = record_room(span.kind, bytes);
        if record.is_some_and(|record| record > bytes.len()) {
            return Err(refuse(
                "a record is longer than the room between it and the next record on the page (a record length was changed, or the page has a form this reader does not know)",
                "a page with row-version information holds a record that is longer than the room between it and the next record (a changed page, or row versioning in a form this reader does not know)",
            ));
        }
        room = room.and_then(|total| total.checked_add(record?.max(MIN_RECORD_ROOM)));
    }
    if compressed {
        return Ok(PageFigures::NotMeasured);
    }
    if usize::from(header.ghost_records) != ghosts {
        return Err(refuse(
            "the number of ghost records in the page header is not the number of records marked as deleted on the page (a ghost bit or the count was changed, or the page has a form this reader does not know)",
            "a page with row-version information whose count of ghost records is not the number of records marked as deleted (a changed page, or row versioning in a form this reader does not know)",
        ));
    }
    let Some(room) = room else {
        return Ok(PageFigures::NotMeasured);
    };
    let accounted = HEADER_SIZE + 2 * usize::from(header.slot_count) + room;
    if accounted.checked_add(usize::from(header.free_count)) != Some(PAGE_SIZE) {
        return Err(refuse(
            "the header, the slots, the records and the free space of the page do not add up to its size (a slot count, a record length or a free-space figure was changed, or the page has a form this reader does not know)",
            "a page with row-version information whose header, slots, records and free space do not add up to its size (a changed page, or row versioning or accelerated database recovery in a form this reader does not know)",
        ));
    }
    Ok(PageFigures::Measured)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFigures {
    Measured,
    NotMeasured,
}

pub fn check_page_figures(page: &PageBytes, page_no: u32) -> Result<PageFigures, Error> {
    let header = PageHeader::parse(page, page_no)?;
    let spans = record_spans(page, &header, page_no)?;
    check_page_accounting(page, &header, page_no, &spans)
}

fn check_in_file(file_pages: u64, file: u16, page: u32) -> Result<(), Error> {
    if file != 1 || u64::from(page) >= file_pages {
        return Err(Error::Inconsistent(
            "a forwarding pointer names a page outside the database file",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub values: Vec<Value>,
    pub page: u32,
    pub slot: u16,
}

#[derive(Debug, Clone)]
pub struct TableDef {
    pub object_id: i32,
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub compressed: bool,
    pub units: Vec<AllocUnit>,
    pub layout: Vec<PhysSlot>,
    pub clustered: bool,
    catalog_rows: Option<u64>,
}

impl TableDef {
    pub fn new(
        object_id: i32,
        name: String,
        columns: Vec<ColumnDef>,
        layout: Vec<PhysSlot>,
        compressed: bool,
        units: Vec<AllocUnit>,
    ) -> TableDef {
        TableDef {
            object_id,
            name,
            columns,
            compressed,
            units,
            layout,
            clustered: false,
            catalog_rows: None,
        }
    }

    pub fn catalog_row_count(&self) -> Option<u64> {
        self.catalog_rows
    }

    pub fn with_catalog_row_count(mut self, rows: Option<u64>) -> TableDef {
        self.catalog_rows = rows;
        self
    }

    pub fn count_rows<S: PageSource>(&self, reader: &mut PageReader<S>) -> Result<RowCount, Error> {
        let mut rows = self.rows(reader)?;
        for row in rows.by_ref() {
            row?;
        }
        Ok(RowCount {
            read: rows.rows_read(),
            catalog: self.catalog_rows,
        })
    }

    pub fn unit(&self, kind: UnitKind) -> Option<&AllocUnit> {
        self.units.iter().find(|unit| unit.kind == kind)
    }

    pub fn rows<'a, S: PageSource>(
        &'a self,
        reader: &'a mut PageReader<S>,
    ) -> Result<Rows<'a, S>, Error> {
        self.rows_with(reader, RowOptions::default())
    }

    pub fn rows_with<'a, S: PageSource>(
        &'a self,
        reader: &'a mut PageReader<S>,
        options: RowOptions,
    ) -> Result<Rows<'a, S>, Error> {
        if self.compressed {
            return Err(Error::Unsupported("page or row compression"));
        }
        if let Some((column, what)) = self
            .columns
            .iter()
            .find_map(|column| column.refused.map(|what| (&column.name, what)))
        {
            return Err(Error::UnsupportedColumn {
                column: column.clone(),
                what,
            });
        }
        if let Some(column) = self.columns.iter().find_map(|column| match column.ty {
            SqlType::Unsupported(name) => Some((&column.name, name)),
            _ => None,
        }) {
            return Err(Error::UnsupportedType {
                column: column.0.clone(),
                type_name: column.1.to_owned(),
            });
        }
        let unit = self.unit(UnitKind::InRow).ok_or(Error::Inconsistent(
            "a table without an in-row allocation unit",
        ))?;
        reader.note_object(self.object_id)?;
        reader.note_allocation_unit(unit.id)?;
        let map = UnitMap::load(reader, unit)?;
        let mut scan = RecordScan::new(map);
        if options.accept_unreleased_versions {
            scan = scan.accepting_unreleased_versions();
        }
        Ok(Rows {
            table: self,
            reader,
            scan: if self.clustered {
                scan.checking_leaf_chain()
            } else {
                scan
            },
            ended: false,
            read: 0,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RowOptions {
    pub accept_unreleased_versions: bool,
}

impl RowOptions {
    pub fn accepting_unreleased_versions() -> RowOptions {
        RowOptions {
            accept_unreleased_versions: true,
        }
    }
}

pub struct Rows<'a, S: PageSource> {
    table: &'a TableDef,
    reader: &'a mut PageReader<S>,
    scan: RecordScan,
    ended: bool,
    read: u64,
}

impl<S: PageSource> Rows<'_, S> {
    pub fn rows_read(&self) -> u64 {
        self.read
    }

    pub fn unreleased_records(&self) -> u64 {
        self.scan.unreleased_records()
    }

    fn advance(&mut self) -> Result<Option<Row>, Error> {
        let Some(live) = self.scan.next(self.reader)? else {
            return Ok(None);
        };
        let record = live.frame()?;
        let cells = decode_cells(&record, &self.table.layout, self.table.columns.len())?;
        let values = cells
            .into_iter()
            .zip(&self.table.columns)
            .map(|(cell, column)| decode_value(cell, column))
            .collect::<Result<Vec<_>, _>>()?;
        self.read = self.read.saturating_add(1);
        Ok(Some(Row {
            values,
            page: live.page,
            slot: live.slot,
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowCount {
    pub read: u64,
    pub catalog: Option<u64>,
}

impl RowCount {
    pub fn agrees(&self) -> bool {
        self.catalog == Some(self.read)
    }
}

impl<S: PageSource> Iterator for Rows<'_, S> {
    type Item = Result<Row, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.advance() {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => {
                self.ended = true;
                None
            }
            Err(error) => {
                self.ended = true;
                Some(Err(error))
            }
        }
    }
}

pub enum ScanSource<'a> {
    Unit(&'a AllocUnit),
    Chain(PagePtr),
    Clustered(&'a AllocUnit),
}

pub fn for_each_record<S: PageSource>(
    reader: &mut PageReader<S>,
    source: ScanSource<'_>,
    mut visit: impl FnMut(&FixedVarRecord<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut scan = match source {
        ScanSource::Unit(unit) => {
            reader.note_allocation_unit(unit.id)?;
            RecordScan::new(UnitMap::load(reader, unit)?)
        }
        ScanSource::Clustered(unit) => {
            reader.note_allocation_unit(unit.id)?;
            RecordScan::new(UnitMap::load(reader, unit)?).checking_leaf_chain()
        }
        ScanSource::Chain(first) => RecordScan::chain(first),
    };
    while let Some(live) = scan.next(reader)? {
        let record = live.frame()?;
        visit(&record)?;
    }
    Ok(())
}

pub fn for_each_record_in_chain<S: PageSource>(
    reader: &mut PageReader<S>,
    first: PagePtr,
    unit_id: i64,
    mut visit: impl FnMut(&FixedVarRecord<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut scan = RecordScan::chain_of(first, unit_id);
    while let Some(live) = scan.next(reader)? {
        let record = live.frame()?;
        visit(&record)?;
    }
    Ok(())
}
