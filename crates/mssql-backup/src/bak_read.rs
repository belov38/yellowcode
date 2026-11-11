use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{Read, Seek},
    ops::RangeInclusive,
};

use crate::{
    bak_bytes::{u8_at, u16_at, u32_at},
    bak_checksum::{Integrity, PAGE_SIZE, page_integrity},
    bak_pagemap::{self, PageMap, PageOrigin},
};

pub type PageBytes = [u8; PAGE_SIZE];

pub const MAX_READ_SET_RANGES: usize = 1 << 20;
pub const MAX_READ_SET_IDS: usize = 1 << 16;
const PFS_CACHE: usize = 16;
const TORN_PAGE_PROTECTION: &str = "a page with torn-page protection (TORN_PAGE_DETECTION moves bits of the page into its header and this reader does not undo that)";
const PFS_INTERVAL: u32 = 8_088;
pub const TYPE_DATA: u8 = 1;
pub const TYPE_INDEX: u8 = 2;
pub const TYPE_TEXT_MIX: u8 = 3;
pub const TYPE_TEXT_TREE: u8 = 4;
pub const TYPE_GAM: u8 = 8;
pub const TYPE_SGAM: u8 = 9;
pub const TYPE_IAM: u8 = 10;
pub const TYPE_PFS: u8 = 11;
pub const TYPE_BOOT: u8 = 13;
pub const TYPE_DCM: u8 = 16;
pub const TYPE_BCM: u8 = 17;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("page map: {0}")]
    PageMap(#[from] bak_pagemap::Error),
    #[error(
        "page {page} fails its checksum or torn-page check; that check was verified on pages of SQL Server 2014 and 2022 only, \
         so on another version a good page may fail it: use --server to let SQL Server read the backup"
    )]
    Corrupt { page: u32 },
    #[error("page {page} says it is page {header_file}:{header_page}")]
    WrongId {
        page: u32,
        header_page: u32,
        header_file: u16,
    },
    #[error("page {page} has type {found}, but only types {expected:?} are expected there")]
    WrongType {
        page: u32,
        found: u8,
        expected: &'static [u8],
    },
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("unsupported type {type_name} of column {column}")]
    UnsupportedType { column: String, type_name: String },
    #[error("inconsistent database: {0}")]
    Inconsistent(&'static str),
    #[error("column {column}: {what}")]
    BadValue { column: String, what: &'static str },
    #[error("column {column} is not read: {what}")]
    UnsupportedColumn { column: String, what: &'static str },
    #[error("column {column}: {what}")]
    UnsupportedValue { column: String, what: &'static str },
    #[error("page {page} is malformed: {what}")]
    Malformed { page: u32, what: &'static str },
    #[error("the leaf pages of a clustered index are not one chain (page {page}): {what}")]
    LeafChain { page: u32, what: String },
    #[error("the figures of page {page} are not consistent: {what}")]
    PageAccounting { page: u32, what: &'static str },
    #[error(
        "record {slot} of page {page} is a large-object record of type {record_type}, which is not read"
    )]
    UnsupportedLobRecord {
        page: u32,
        slot: u16,
        record_type: u16,
    },
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    #[error("writing a large value to its sink failed: {0}")]
    Sink(#[source] std::io::Error),
    #[error("table {schema}.{name} is not in the database")]
    NotFound { schema: String, name: String },
}

pub trait PageSource {
    fn read_page(
        &mut self,
        page: u32,
        out: &mut PageBytes,
    ) -> Result<Option<PageOrigin>, bak_pagemap::Error>;
    fn is_allocated(&mut self, page: u32) -> Result<bool, bak_pagemap::Error>;
    fn file_pages(&self) -> u64;
}

impl<R: Read + Seek> PageSource for PageMap<R> {
    fn read_page(
        &mut self,
        page: u32,
        out: &mut PageBytes,
    ) -> Result<Option<PageOrigin>, bak_pagemap::Error> {
        PageMap::read_page(self, page, out)
    }

    fn is_allocated(&mut self, page: u32) -> Result<bool, bak_pagemap::Error> {
        PageMap::is_allocated(self, page)
    }

    fn file_pages(&self) -> u64 {
        PageMap::file_pages(self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    pub checksum_verified: u64,
    pub torn_page_verified: u64,
    pub unprotected: u64,
}

impl IntegrityReport {
    pub fn verified(&self) -> u64 {
        self.checksum_verified
            .saturating_add(self.torn_page_verified)
    }

    pub fn pages(&self) -> u64 {
        self.verified().saturating_add(self.unprotected)
    }

    pub fn all_protected(&self) -> bool {
        self.unprotected == 0
    }
}

impl fmt::Display for IntegrityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} pages used: {} verified by checksum, {} unprotected (nothing to verify)",
            self.pages(),
            self.checksum_verified,
            self.unprotected
        )
    }
}

#[derive(Debug, Default, Clone)]
struct PageRuns {
    runs: BTreeMap<u32, u32>,
}

impl PageRuns {
    fn contains(&self, page: u32) -> bool {
        self.runs
            .range(..=page)
            .next_back()
            .is_some_and(|(_, &end)| end >= page)
    }

    fn count(&self) -> u64 {
        self.runs
            .iter()
            .map(|(&start, &end)| u64::from(end - start) + 1)
            .sum()
    }

    fn ranges(&self) -> impl Iterator<Item = RangeInclusive<u32>> + '_ {
        self.runs.iter().map(|(&start, &end)| start..=end)
    }

    fn insert(&mut self, page: u32) -> Result<bool, Error> {
        if self.contains(page) {
            return Ok(false);
        }
        let touches_before = self
            .runs
            .range(..page)
            .next_back()
            .map(|(&start, &end)| (start, end))
            .filter(|&(_, end)| end.checked_add(1) == Some(page));
        let next = page
            .checked_add(1)
            .filter(|next| self.runs.contains_key(next));
        match (touches_before, next) {
            (Some((start, _)), Some(next)) => {
                let end = self.runs.remove(&next).unwrap_or(next);
                self.runs.insert(start, end);
            }
            (Some((start, _)), None) => {
                self.runs.insert(start, page);
            }
            (None, Some(next)) => {
                let end = self.runs.remove(&next).unwrap_or(next);
                self.runs.insert(page, end);
            }
            (None, None) => {
                if self.runs.len() >= MAX_READ_SET_RANGES {
                    return Err(Error::Limit("separate runs of pages in the read set"));
                }
                self.runs.insert(page, page);
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReadSet {
    pages: PageRuns,
    region2: PageRuns,
    unprotected: PageRuns,
    allocation_units: BTreeSet<i64>,
    objects: BTreeSet<i32>,
}

impl ReadSet {
    pub fn new() -> ReadSet {
        ReadSet::default()
    }

    pub fn insert_page(&mut self, page: u32, from_region2: bool) -> Result<bool, Error> {
        if from_region2 {
            self.region2.insert(page)?;
        }
        self.pages.insert(page)
    }

    pub fn mark_unprotected(&mut self, page: u32) -> Result<(), Error> {
        self.unprotected.insert(page).map(|_| ())
    }

    pub fn insert_allocation_unit(&mut self, auid: i64) -> Result<bool, Error> {
        if !self.allocation_units.contains(&auid) && self.allocation_units.len() >= MAX_READ_SET_IDS
        {
            return Err(Error::Limit("allocation units in the read set"));
        }
        Ok(self.allocation_units.insert(auid))
    }

    pub fn insert_object(&mut self, object_id: i32) -> Result<bool, Error> {
        if !self.objects.contains(&object_id) && self.objects.len() >= MAX_READ_SET_IDS {
            return Err(Error::Limit("objects in the read set"));
        }
        Ok(self.objects.insert(object_id))
    }

    pub fn contains_page(&self, page: u32) -> bool {
        self.pages.contains(page)
    }

    pub fn contains_region2_page(&self, page: u32) -> bool {
        self.region2.contains(page)
    }

    pub fn contains_unprotected_page(&self, page: u32) -> bool {
        self.unprotected.contains(page)
    }

    pub fn page_count(&self) -> u64 {
        self.pages.count()
    }

    pub fn page_ranges(&self) -> impl Iterator<Item = RangeInclusive<u32>> + '_ {
        self.pages.ranges()
    }

    pub fn region2_page_ranges(&self) -> impl Iterator<Item = RangeInclusive<u32>> + '_ {
        self.region2.ranges()
    }

    pub fn unprotected_page_ranges(&self) -> impl Iterator<Item = RangeInclusive<u32>> + '_ {
        self.unprotected.ranges()
    }

    pub fn allocation_units(&self) -> &BTreeSet<i64> {
        &self.allocation_units
    }

    pub fn objects(&self) -> &BTreeSet<i32> {
        &self.objects
    }
}

fn pfs_page_of(page: u32) -> u32 {
    let base = page / PFS_INTERVAL * PFS_INTERVAL;
    if base == 0 { 1 } else { base }
}

fn check_header(page: u32, bytes: &PageBytes, expected_types: &'static [u8]) -> Result<(), Error> {
    let bytes = bytes.as_slice();
    let (Some(version), Some(kind), Some(id), Some(file)) = (
        u8_at(bytes, 0),
        u8_at(bytes, 1),
        u32_at(bytes, 32),
        u16_at(bytes, 36),
    ) else {
        return Err(Error::Malformed {
            page,
            what: "truncated page header",
        });
    };
    if version != 1 {
        return Err(Error::Malformed {
            page,
            what: "the page is not formatted (its header version is not 1)",
        });
    }
    if id != page || file != 1 {
        return Err(Error::WrongId {
            page,
            header_page: id,
            header_file: file,
        });
    }
    if !expected_types.is_empty() && !expected_types.contains(&kind) {
        return Err(Error::WrongType {
            page,
            found: kind,
            expected: expected_types,
        });
    }
    Ok(())
}

pub struct PageReader<S: PageSource> {
    source: S,
    report: IntegrityReport,
    read_set: ReadSet,
    pfs_verified: Vec<u32>,
}

impl<S: PageSource> PageReader<S> {
    pub fn new(source: S) -> PageReader<S> {
        PageReader {
            source,
            report: IntegrityReport::default(),
            read_set: ReadSet::new(),
            pfs_verified: Vec::new(),
        }
    }

    pub fn read(
        &mut self,
        page: u32,
        expected_types: &'static [u8],
        out: &mut PageBytes,
    ) -> Result<Option<PageOrigin>, Error> {
        let Some(origin) = self.source.read_page(page, out)? else {
            return Ok(None);
        };
        let integrity = page_integrity(out);
        if integrity.is_corrupt() {
            return Err(Error::Corrupt { page });
        }
        if matches!(integrity, Integrity::TornPage { .. }) {
            return Err(Error::Unsupported(TORN_PAGE_PROTECTION));
        }
        check_header(page, out, expected_types)?;
        let new = self
            .read_set
            .insert_page(page, origin == PageOrigin::Region2)?;
        match integrity {
            Integrity::Checksum { .. } if new => self.report.checksum_verified += 1,
            Integrity::Unprotected if new => {
                self.report.unprotected += 1;
                self.read_set.mark_unprotected(page)?;
            }
            Integrity::Checksum { .. } | Integrity::Unprotected => {}
            Integrity::TornPage { .. } | Integrity::InvalidProtection { .. } => {
                return Err(Error::Corrupt { page });
            }
        }
        Ok(Some(origin))
    }

    pub fn is_allocated(&mut self, page: u32) -> Result<bool, Error> {
        if u64::from(page) >= self.source.file_pages() {
            return Ok(false);
        }
        let pfs = pfs_page_of(page);
        if !self.pfs_verified.contains(&pfs) {
            self.verify_pfs(pfs)?;
        }
        Ok(self.source.is_allocated(page)?)
    }

    fn verify_pfs(&mut self, pfs: u32) -> Result<(), Error> {
        let mut bytes = [0_u8; PAGE_SIZE];
        if self.read(pfs, &[TYPE_PFS], &mut bytes)?.is_none() {
            return Err(Error::Inconsistent("a PFS page is not in the backup"));
        }
        if self.pfs_verified.len() >= PFS_CACHE {
            self.pfs_verified.remove(0);
        }
        self.pfs_verified.push(pfs);
        Ok(())
    }

    pub fn file_pages(&self) -> u64 {
        self.source.file_pages()
    }

    pub fn integrity(&self) -> IntegrityReport {
        self.report
    }

    pub fn read_set(&self) -> &ReadSet {
        &self.read_set
    }

    pub fn note_allocation_unit(&mut self, auid: i64) -> Result<(), Error> {
        self.read_set.insert_allocation_unit(auid).map(|_| ())
    }

    pub fn note_object(&mut self, object_id: i32) -> Result<(), Error> {
        self.read_set.insert_object(object_id).map(|_| ())
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn into_parts(self) -> (S, IntegrityReport, ReadSet) {
        (self.source, self.report, self.read_set)
    }
}
