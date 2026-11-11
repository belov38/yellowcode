// Derived from bcdb 0.2.1, src/BcDb.Core/Lob.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use std::{collections::HashSet, io::Write, ops::Range};

use crate::{
    bak_bytes::{u8_at, u16_at, u32_at, u64_at},
    bak_checksum::PAGE_SIZE,
    bak_read::{Error, PageBytes, PageReader, PageSource, TYPE_TEXT_MIX, TYPE_TEXT_TREE},
    bak_record::{HEADER_SIZE, PageHeader, RecordId, RecordKind},
    bak_table::{TableDef, UnitKind, UnitMap},
    bak_values::LobRoot,
};

pub const MAX_LOB_BYTES: u64 = 2_147_483_647;
pub const MAX_LOB_LEVEL: u16 = 3;
pub const MAX_LOB_RECORDS: usize = 1_048_576;

const LOB_PAGES: &[u8] = &[TYPE_TEXT_MIX, TYPE_TEXT_TREE];

const HEADER_LEN: usize = 14;
const STATUS_FRAGMENT: u8 = 0x08;
const STATUS_VERSIONED: u8 = 0x40;
const KIND_SMALL: u16 = 0;
const KIND_INTERNAL: u16 = 2;
const KIND_DATA: u16 = 3;
const KIND_ROOT: u16 = 5;
const KIND_NULL: u16 = 8;
const SMALL_HEAD: usize = 20;
const ROOT_HEAD: usize = 24;
const ROOT_LINK: usize = 12;
const INTERNAL_HEAD: usize = 20;
const INTERNAL_LINK: usize = 16;

pub struct LobUnits {
    lob: Option<UnitMap>,
    overflow: Option<UnitMap>,
    max_records: usize,
}

impl LobUnits {
    #[must_use]
    pub fn with_max_records(mut self, max_records: usize) -> LobUnits {
        self.max_records = max_records;
        self
    }

    pub fn load<S: PageSource>(
        reader: &mut PageReader<S>,
        table: &TableDef,
    ) -> Result<LobUnits, Error> {
        let mut load = |kind: UnitKind| -> Result<Option<UnitMap>, Error> {
            let Some(unit) = table.unit(kind) else {
                return Ok(None);
            };
            reader.note_allocation_unit(unit.id)?;
            UnitMap::load(reader, unit).map(Some)
        };
        let lob = load(UnitKind::Lob)?;
        let overflow = load(UnitKind::RowOverflow)?;
        Ok(LobUnits {
            lob,
            overflow,
            max_records: MAX_LOB_RECORDS,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LobStats {
    pub records: usize,
}

#[derive(Debug, Clone, Copy)]
struct Link {
    size: u64,
    record: RecordId,
}

enum Record {
    Small { data: Range<usize> },
    Data { data: Range<usize> },
    Root { level: u16, links: Vec<Link> },
    Internal { level: u16, links: Vec<Link> },
    Null,
}

fn wrong_kind() -> Error {
    Error::Inconsistent("a large-object link leads to a record of the wrong kind")
}

const _: () = assert!(usize::BITS <= 64);

fn byte_count(len: usize) -> u64 {
    len as u64
}

fn check_growth(links: &[Link]) -> Result<(), Error> {
    let mut previous = 0_u64;
    for link in links {
        if link.size <= previous {
            return Err(Error::Inconsistent(
                "the sizes of the links of a large-object record do not grow",
            ));
        }
        previous = link.size;
    }
    Ok(())
}

fn locate(
    page: &PageBytes,
    header: &PageHeader,
    page_no: u32,
    slot: u16,
) -> Result<(usize, usize), Error> {
    let malformed = |what| Error::Malformed {
        page: page_no,
        what,
    };
    let count = usize::from(header.slot_count);
    let directory = PAGE_SIZE
        .checked_sub(count.saturating_mul(2))
        .filter(|start| *start >= HEADER_SIZE)
        .ok_or_else(|| malformed("the slot array overlaps the page header"))?;
    let free_data = usize::from(header.free_data);
    if !(HEADER_SIZE..=directory).contains(&free_data) {
        return Err(malformed(
            "the free-data boundary is outside the record area",
        ));
    }
    let entry = |index: usize| -> usize {
        PAGE_SIZE
            .checked_sub((index + 1) * 2)
            .and_then(|at| u16_at(page, at))
            .map_or(0, usize::from)
    };
    let asked = usize::from(slot);
    let start = if asked < count { entry(asked) } else { 0 };
    let mut end = free_data;
    for index in 0..count {
        let offset = entry(index);
        if offset == 0 {
            continue;
        }
        if !(HEADER_SIZE..free_data).contains(&offset) {
            return Err(malformed("a slot points outside the record area"));
        }
        if index == asked {
            continue;
        }
        if offset == start {
            return Err(malformed("two slots point at one record"));
        }
        if offset > start {
            end = end.min(offset);
        }
    }
    if start == 0 {
        return Err(Error::Inconsistent(
            "a large-object link leads to an empty slot",
        ));
    }
    Ok((start, end))
}

fn parse_record(page: &PageBytes, id: RecordId, blob: u64) -> Result<Record, Error> {
    let malformed = |what| Error::Malformed {
        page: id.page,
        what,
    };
    let header = PageHeader::parse(page, id.page)?;
    let (start, end) = locate(page, &header, id.page, id.slot)?;
    let bytes = page
        .get(start..end)
        .ok_or_else(|| malformed("a record runs past the page"))?;
    let cut = || malformed("a large-object record is cut off");
    let status = u8_at(bytes, 0).ok_or_else(cut)?;
    if RecordKind::from_status(status) != RecordKind::BlobFragment {
        return Err(Error::Inconsistent(
            "a large-object link leads to a record that is not a large-object fragment",
        ));
    }
    let stated = usize::from(u16_at(bytes, 2).ok_or_else(cut)?);
    if stated < HEADER_LEN || stated > bytes.len() {
        return Err(malformed(
            "the stated length of a large-object record does not fit its place on the page",
        ));
    }
    let record = bytes.get(..stated).ok_or_else(cut)?;
    let status_a = u8_at(record, 0).ok_or_else(cut)?;
    let status_b = u8_at(record, 1).ok_or_else(cut)?;
    if status_a & !STATUS_VERSIONED != STATUS_FRAGMENT || status_b != 0 {
        return Err(Error::Unsupported(
            "a large-object record with status bits that were never seen",
        ));
    }
    if u64_at(record, 4).ok_or_else(cut)? != blob {
        return Err(Error::Inconsistent(
            "a large-object link leads to a record of another value",
        ));
    }
    let kind = u16_at(record, 12).ok_or_else(cut)?;
    let body = |from: usize, len: usize| -> Range<usize> {
        let first = start.saturating_add(from);
        first..first.saturating_add(len)
    };
    match kind {
        KIND_DATA => Ok(Record::Data {
            data: body(HEADER_LEN, stated - HEADER_LEN),
        }),
        KIND_SMALL => {
            let length = usize::from(u16_at(record, 14).ok_or_else(cut)?);
            if SMALL_HEAD + length > stated {
                return Err(malformed(
                    "a small large-object value is longer than its record",
                ));
            }
            Ok(Record::Small {
                data: body(SMALL_HEAD, length),
            })
        }
        KIND_NULL => {
            let size = u16_at(record, 14).ok_or_else(cut)?;
            if size != 0 {
                return Err(Error::Inconsistent(
                    "a NULL large-object root states a size",
                ));
            }
            Ok(Record::Null)
        }
        KIND_ROOT => {
            let (level, links) = parse_links(record, id, ROOT_HEAD, ROOT_LINK, true)?;
            Ok(Record::Root { level, links })
        }
        KIND_INTERNAL => {
            let (level, links) = parse_links(record, id, INTERNAL_HEAD, INTERNAL_LINK, false)?;
            Ok(Record::Internal { level, links })
        }
        other => Err(Error::UnsupportedLobRecord {
            page: id.page,
            slot: id.slot,
            record_type: other,
        }),
    }
}

fn parse_links(
    record: &[u8],
    id: RecordId,
    head: usize,
    link_len: usize,
    narrow: bool,
) -> Result<(u16, Vec<Link>), Error> {
    let malformed = |what| Error::Malformed {
        page: id.page,
        what,
    };
    let cut = || malformed("a large-object record is cut off");
    let max = usize::from(u16_at(record, 14).ok_or_else(cut)?);
    let count = usize::from(u16_at(record, 16).ok_or_else(cut)?);
    let level = u16_at(record, 18).ok_or_else(cut)?;
    if count > max {
        return Err(malformed(
            "a large-object record holds more links than it has room for",
        ));
    }
    if head.saturating_add(count.saturating_mul(link_len)) > record.len() {
        return Err(malformed(
            "the links of a large-object record run past the record",
        ));
    }
    let mut links = Vec::with_capacity(count);
    for index in 0..count {
        let at = head + index * link_len;
        let (size, id_at) = if narrow {
            (u64::from(u32_at(record, at).ok_or_else(cut)?), at + 4)
        } else {
            (u64_at(record, at).ok_or_else(cut)?, at + 8)
        };
        let record_id = RecordId::from_bytes(record, id_at).ok_or_else(cut)?;
        links.push(Link {
            size,
            record: record_id,
        });
    }
    check_growth(&links)?;
    Ok((level, links))
}

struct Walk<'a, S: PageSource> {
    reader: &'a mut PageReader<S>,
    unit: &'a UnitMap,
    blob: u64,
    sink: &'a mut dyn Write,
    limit: u64,
    written: u64,
    page: Box<PageBytes>,
    page_no: u32,
    seen: HashSet<(u32, u16)>,
    max_records: usize,
}

impl<'a, S: PageSource> Walk<'a, S> {
    fn new(
        reader: &'a mut PageReader<S>,
        unit: &'a UnitMap,
        blob: u64,
        sink: &'a mut dyn Write,
        limit: u64,
        max_records: usize,
    ) -> Walk<'a, S> {
        Walk {
            reader,
            unit,
            blob,
            sink,
            limit,
            written: 0,
            page: Box::new([0_u8; PAGE_SIZE]),
            page_no: 0,
            seen: HashSet::new(),
            max_records,
        }
    }

    fn stats(&self) -> LobStats {
        LobStats {
            records: self.seen.len(),
        }
    }
}

impl<S: PageSource> Walk<'_, S> {
    fn visit(&mut self, id: RecordId) -> Result<(), Error> {
        if !self.seen.insert((id.page, id.slot)) {
            return Err(Error::Inconsistent(
                "a large-object value leads to the same record twice",
            ));
        }
        if self.seen.len() > self.max_records {
            return Err(Error::Limit(
                "a large-object value has more records than this reader follows",
            ));
        }
        Ok(())
    }

    fn record(&mut self, id: RecordId) -> Result<Record, Error> {
        if id.file != 1 {
            return Err(Error::Unsupported(
                "a large-object link into a file other than file 1",
            ));
        }
        self.visit(id)?;
        if !self.unit.contains(id.page) {
            return Err(Error::Inconsistent(
                "a large-object link leads to a page outside the allocation unit that holds this kind of value",
            ));
        }
        if !self.reader.is_allocated(id.page)? {
            return Err(Error::Inconsistent(
                "a large-object link leads to a page that is not allocated",
            ));
        }
        if self
            .reader
            .read(id.page, LOB_PAGES, &mut self.page)?
            .is_none()
        {
            return Err(Error::Inconsistent(
                "a large-object page is not in the backup",
            ));
        }
        self.page_no = id.page;
        parse_record(&self.page, id, self.blob)
    }

    fn declare(&self, total: u64) -> Result<(), Error> {
        if total > MAX_LOB_BYTES {
            return Err(Error::Inconsistent(
                "a large-object value larger than SQL Server stores",
            ));
        }
        if total > self.limit {
            return Err(Error::Limit(
                "a large-object value is larger than the limit given for it",
            ));
        }
        Ok(())
    }

    fn emit(&mut self, range: Range<usize>) -> Result<(), Error> {
        let bytes = self.page.get(range).ok_or(Error::Malformed {
            page: self.page_no,
            what: "a piece runs past its page",
        })?;
        let len = byte_count(bytes.len());
        if self
            .written
            .checked_add(len)
            .is_none_or(|total| total > self.limit)
        {
            return Err(Error::Limit(
                "a large-object value is larger than the limit given for it",
            ));
        }
        self.sink.write_all(bytes).map_err(Error::Sink)?;
        self.written += len;
        Ok(())
    }

    fn links(&mut self, links: &[Link], level: u16) -> Result<(), Error> {
        let mut previous = 0_u64;
        for link in links {
            let size = link.size.saturating_sub(previous);
            if level == 0 {
                self.piece(link.record, size)?;
            } else {
                self.node(link.record, size, level - 1)?;
            }
            previous = link.size;
        }
        Ok(())
    }

    fn piece(&mut self, id: RecordId, size: u64) -> Result<(), Error> {
        match self.record(id)? {
            Record::Data { data } => {
                if byte_count(data.len()) != size {
                    return Err(Error::Inconsistent(
                        "a large-object piece does not have the size its link states",
                    ));
                }
                self.emit(data)
            }
            _ => Err(wrong_kind()),
        }
    }

    fn node(&mut self, id: RecordId, size: u64, level: u16) -> Result<(), Error> {
        match self.record(id)? {
            Record::Internal {
                level: found,
                links,
            } => {
                if found != level {
                    return Err(Error::Inconsistent(
                        "a large-object tree record is not at the level its parent expects",
                    ));
                }
                if links.last().map(|last| last.size) != Some(size) {
                    return Err(Error::Inconsistent(
                        "a large-object tree record does not cover the size its link states",
                    ));
                }
                self.links(&links, level)
            }
            _ => Err(wrong_kind()),
        }
    }

    fn root(&mut self, level: u16, links: &[Link]) -> Result<(), Error> {
        if level > MAX_LOB_LEVEL {
            return Err(Error::Limit(
                "a large-object tree has more levels than this reader follows",
            ));
        }
        check_growth(links)?;
        let total = links
            .last()
            .map(|last| last.size)
            .ok_or(Error::Inconsistent("a large-object record has no links"))?;
        self.declare(total)?;
        self.links(links, level)
    }
}

fn unit_of<'a>(unit: &'a Option<UnitMap>, what: &'static str) -> Result<&'a UnitMap, Error> {
    unit.as_ref().ok_or(Error::Inconsistent(what))
}

pub fn read_lob<S: PageSource>(
    reader: &mut PageReader<S>,
    units: &LobUnits,
    root: &LobRoot,
    sink: &mut dyn Write,
    limit: u64,
) -> Result<Option<u64>, Error> {
    read_lob_counted(reader, units, root, sink, limit).map(|(bytes, _)| bytes)
}

pub fn read_lob_counted<S: PageSource>(
    reader: &mut PageReader<S>,
    units: &LobUnits,
    root: &LobRoot,
    sink: &mut dyn Write,
    limit: u64,
) -> Result<(Option<u64>, LobStats), Error> {
    match root {
        LobRoot::TextPointer { timestamp, record } => {
            let unit = unit_of(&units.lob, "the table has no large-object allocation unit")?;
            let mut walk = Walk::new(reader, unit, *timestamp, sink, limit, units.max_records);
            let written = match walk.record(*record)? {
                Record::Null => None,
                Record::Small { data } => {
                    walk.emit(data)?;
                    Some(walk.written)
                }
                Record::Root { level, links } => {
                    walk.root(level, &links)?;
                    Some(walk.written)
                }
                _ => return Err(wrong_kind()),
            };
            Ok((written, walk.stats()))
        }
        LobRoot::Inline {
            overflow,
            level,
            timestamp,
            links,
            ..
        } => {
            let unit = if *overflow {
                unit_of(
                    &units.overflow,
                    "the table has no row-overflow allocation unit",
                )?
            } else {
                unit_of(&units.lob, "the table has no large-object allocation unit")?
            };
            let links: Vec<Link> = links
                .iter()
                .map(|link| Link {
                    size: u64::from(link.cumulative_size),
                    record: link.record,
                })
                .collect();
            let blob = u64::from(*timestamp) << 16;
            let mut walk = Walk::new(reader, unit, blob, sink, limit, units.max_records);
            walk.root(u16::from(*level), &links)?;
            Ok((Some(walk.written), walk.stats()))
        }
    }
}
