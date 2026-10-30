use std::io::{self, Read, Seek, SeekFrom};

use crate::{media::DataRegion, pages::Page};

const PAGE_SIZE: usize = 8192;
const PAGE_SIZE_U64: u64 = 8192;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("raw page I/O: {0}")]
    Io(#[from] io::Error),
    #[error("raw page resource limit exceeded")]
    Limit,
    #[error("invalid or overlapping data region")]
    InvalidRegion,
    #[error("page occurrence was not observed")]
    Missing,
    #[error("more than one physical occurrence of this page was observed")]
    Ambiguous,
    #[error("page header changed after indexing")]
    Changed,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_regions: usize,
    pub max_pages: u64,
    pub max_scan_bytes: u64,
    pub max_index_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageKey {
    pub file_id: u16,
    pub page_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    pub region: usize,
    pub ordinal: u64,
    pub offset: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub zero_pages: u64,
    pub unknown_pages: u64,
    pub indexed_pages: u64,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    key: PageKey,
    location: Location,
}

pub struct PageIndex {
    entries: Vec<Entry>,
    stats: Stats,
}

impl PageIndex {
    pub fn scan(
        reader: &mut (impl Read + Seek),
        regions: &[DataRegion],
        limits: Limits,
    ) -> Result<Self, Error> {
        if regions.len() > limits.max_regions {
            return Err(Error::Limit);
        }
        let mut total = 0_u64;
        let mut previous_end = 0_u64;
        for region in regions {
            let bytes = region
                .pages
                .checked_mul(PAGE_SIZE_U64)
                .ok_or(Error::InvalidRegion)?;
            let end = region
                .offset
                .checked_add(bytes)
                .ok_or(Error::InvalidRegion)?;
            if region.offset < previous_end {
                return Err(Error::InvalidRegion);
            }
            previous_end = end;
            total = total.checked_add(region.pages).ok_or(Error::Limit)?;
        }
        let scan_bytes = total.checked_mul(PAGE_SIZE_U64).ok_or(Error::Limit)?;
        let count = usize::try_from(total).map_err(|_| Error::Limit)?;
        let required = count
            .checked_mul(std::mem::size_of::<Entry>())
            .ok_or(Error::Limit)?;
        if total > limits.max_pages
            || scan_bytes > limits.max_scan_bytes
            || required > limits.max_index_bytes
        {
            return Err(Error::Limit);
        }
        let length = reader.seek(SeekFrom::End(0))?;
        if previous_end > length {
            return Err(Error::InvalidRegion);
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|_| Error::Limit)?;
        let reserved = entries
            .capacity()
            .checked_mul(std::mem::size_of::<Entry>())
            .ok_or(Error::Limit)?;
        if reserved > limits.max_index_bytes {
            return Err(Error::Limit);
        }
        let mut stats = Stats::default();
        let mut bytes = [0_u8; PAGE_SIZE];
        for (region_index, region) in regions.iter().enumerate() {
            if region.pages == 0 {
                continue;
            }
            reader.seek(SeekFrom::Start(region.offset))?;
            for ordinal in 0..region.pages {
                reader.read_exact(&mut bytes)?;
                if bytes.iter().all(|byte| *byte == 0) {
                    stats.zero_pages += 1;
                } else if let Ok(page) = Page::parse(&bytes) {
                    let offset = region.offset + ordinal * PAGE_SIZE_U64;
                    entries.push(Entry {
                        key: PageKey {
                            file_id: page.file_id(),
                            page_id: page.page_id(),
                        },
                        location: Location {
                            region: region_index,
                            ordinal,
                            offset,
                        },
                    });
                    stats.indexed_pages += 1;
                } else {
                    stats.unknown_pages += 1;
                }
            }
        }
        entries.sort_unstable_by_key(|entry| (entry.key, entry.location));
        Ok(Self { entries, stats })
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn locations(&self, key: PageKey) -> impl ExactSizeIterator<Item = Location> + '_ {
        let lo = self.entries.partition_point(|entry| entry.key < key);
        let hi = self.entries.partition_point(|entry| entry.key <= key);
        self.entries
            .get(lo..hi)
            .unwrap_or_default()
            .iter()
            .map(|entry| entry.location)
    }

    pub fn read_at(
        &self,
        reader: &mut (impl Read + Seek),
        key: PageKey,
        location: Location,
    ) -> Result<[u8; PAGE_SIZE], Error> {
        if self
            .entries
            .binary_search_by_key(&(key, location), |entry| (entry.key, entry.location))
            .is_err()
        {
            return Err(Error::Missing);
        }
        let mut bytes = [0_u8; PAGE_SIZE];
        reader.seek(SeekFrom::Start(location.offset))?;
        reader.read_exact(&mut bytes)?;
        let page = Page::parse(&bytes).map_err(|_| Error::Changed)?;
        if page.file_id() != key.file_id || page.page_id() != key.page_id {
            return Err(Error::Changed);
        }
        Ok(bytes)
    }

    pub fn read_unique(
        &self,
        reader: &mut (impl Read + Seek),
        key: PageKey,
    ) -> Result<[u8; PAGE_SIZE], Error> {
        let mut locations = self.locations(key);
        let location = locations.next().ok_or(Error::Missing)?;
        if locations.next().is_some() {
            return Err(Error::Ambiguous);
        }
        self.read_at(reader, key, location)
    }
}
