// Derived from bcdb 0.2.1, src/BcDb.Core/Records.cs and src/BcDb.Core/PageFile.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use crate::{
    bak_bytes::{slice, u8_at, u16_at, u32_at},
    bak_checksum::PAGE_SIZE,
    bak_read::{Error, PageBytes},
    records::{self, FixedVarRecord},
};

pub const HEADER_SIZE: usize = 96;

pub use crate::bak_read::{
    TYPE_BCM, TYPE_BOOT, TYPE_DATA, TYPE_DCM, TYPE_GAM, TYPE_IAM, TYPE_INDEX, TYPE_PFS, TYPE_SGAM,
    TYPE_TEXT_MIX, TYPE_TEXT_TREE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PagePtr {
    pub page: u32,
    pub file: u16,
}

impl PagePtr {
    pub const NULL: PagePtr = PagePtr { page: 0, file: 0 };

    pub fn is_null(&self) -> bool {
        self.page == 0 && self.file == 0
    }

    pub fn from_bytes(bytes: &[u8], at: usize) -> Option<PagePtr> {
        Some(PagePtr {
            page: u32_at(bytes, at)?,
            file: u16_at(bytes, at.checked_add(4)?)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub page_type: u8,
    pub type_flags: u8,
    pub level: u8,
    pub flag_bits: u16,
    pub index_id: u16,
    pub prev: PagePtr,
    pub min_record_len: u16,
    pub next: PagePtr,
    pub slot_count: u16,
    pub object_id: u32,
    pub free_count: u16,
    pub free_data: u16,
    pub page_id: u32,
    pub file_id: u16,
    pub ghost_records: u16,
}

impl PageHeader {
    pub fn parse(page: &PageBytes, page_no: u32) -> Result<PageHeader, Error> {
        let bytes = page.as_slice();
        if u8_at(bytes, 0) != Some(1) {
            return Err(Error::Malformed {
                page: page_no,
                what: "unknown page header version",
            });
        }
        let cut = || Error::Malformed {
            page: page_no,
            what: "truncated page header",
        };
        let u8_ = |at| u8_at(bytes, at).ok_or_else(cut);
        let u16_ = |at| u16_at(bytes, at).ok_or_else(cut);
        let u32_ = |at| u32_at(bytes, at).ok_or_else(cut);
        let ptr = |at| PagePtr::from_bytes(bytes, at).ok_or_else(cut);
        Ok(PageHeader {
            page_type: u8_(1)?,
            type_flags: u8_(2)?,
            level: u8_(3)?,
            flag_bits: u16_(4)?,
            index_id: u16_(6)?,
            prev: ptr(8)?,
            min_record_len: u16_(14)?,
            next: ptr(16)?,
            slot_count: u16_(22)?,
            object_id: u32_(24)?,
            free_count: u16_(28)?,
            free_data: u16_(30)?,
            page_id: u32_(32)?,
            file_id: u16_(36)?,
            ghost_records: u16_(58)?,
        })
    }

    pub fn unit_identity(&self) -> i64 {
        let bits = (u64::from(self.index_id) << 48) | (u64::from(self.object_id) << 16);
        i64::from_le_bytes(bits.to_le_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Primary,
    Forwarded,
    ForwardingStub,
    Index,
    BlobFragment,
    GhostIndex,
    GhostData,
    GhostVersion,
}

impl RecordKind {
    pub fn from_status(status: u8) -> RecordKind {
        match (status >> 1) & 7 {
            0 => RecordKind::Primary,
            1 => RecordKind::Forwarded,
            2 => RecordKind::ForwardingStub,
            3 => RecordKind::Index,
            4 => RecordKind::BlobFragment,
            5 => RecordKind::GhostIndex,
            6 => RecordKind::GhostData,
            _ => RecordKind::GhostVersion,
        }
    }

    pub fn is_ghost(&self) -> bool {
        matches!(
            self,
            RecordKind::GhostIndex | RecordKind::GhostData | RecordKind::GhostVersion
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordSpan {
    pub slot: u16,
    pub kind: RecordKind,
    pub start: usize,
    pub end: usize,
}

impl RecordSpan {
    pub fn bytes<'a>(&self, page: &'a PageBytes) -> Option<&'a [u8]> {
        page.get(self.start..self.end)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RecordRef<'a> {
    pub slot: u16,
    pub kind: RecordKind,
    pub bytes: &'a [u8],
}

pub fn record_spans(
    page: &PageBytes,
    header: &PageHeader,
    page_no: u32,
) -> Result<Vec<RecordSpan>, Error> {
    let malformed = |what| Error::Malformed {
        page: page_no,
        what,
    };
    let count = usize::from(header.slot_count);
    let directory = PAGE_SIZE
        .checked_sub(count * 2)
        .filter(|start| *start >= HEADER_SIZE)
        .ok_or_else(|| malformed("the slot array overlaps the page header"))?;
    let free_data = usize::from(header.free_data);
    if !(HEADER_SIZE..=directory).contains(&free_data) {
        return Err(malformed(
            "the free-data boundary is outside the record area",
        ));
    }
    let bytes = page.as_slice();
    let mut placed: Vec<(usize, u16)> = Vec::new();
    placed
        .try_reserve_exact(count)
        .map_err(|_| malformed("the slot array does not fit in memory"))?;
    for slot in 0..header.slot_count {
        let at = PAGE_SIZE - 2 * (usize::from(slot) + 1);
        let offset =
            usize::from(u16_at(bytes, at).ok_or_else(|| malformed("the slot array is cut off"))?);
        if offset == 0 {
            continue;
        }
        if !(HEADER_SIZE..free_data).contains(&offset) {
            return Err(malformed("a slot points outside the record area"));
        }
        placed.push((offset, slot));
    }
    let mut by_offset = placed.clone();
    by_offset.sort_unstable();
    if by_offset
        .windows(2)
        .any(|pair| pair.first().map(|p| p.0) == pair.get(1).map(|p| p.0))
    {
        return Err(malformed("two slots point at one record"));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(placed.len())
        .map_err(|_| malformed("the records do not fit in memory"))?;
    for (offset, slot) in placed {
        let index = by_offset
            .binary_search_by_key(&offset, |entry| entry.0)
            .map_err(|_| malformed("a slot lost its record"))?;
        let end = by_offset.get(index + 1).map_or(free_data, |next| next.0);
        let status = u8_at(bytes, offset).ok_or_else(|| malformed("an empty record"))?;
        out.push(RecordSpan {
            slot,
            kind: RecordKind::from_status(status),
            start: offset,
            end,
        });
    }
    Ok(out)
}

const STUB_LEN: usize = 9;
const VERSIONING_TAG: u8 = 0x40;
const VERSIONING_TAG_LEN: usize = 14;
const IN_ROW_FORM_MASK: u16 = 0x1FFF;
const IN_ROW_FORM: u16 = 0x1FFC;
const IN_ROW_KIND_DELETED: u16 = 1;
const IN_ROW_KIND_BLOCK: u16 = 2;
const IN_ROW_KIND_NONE: u16 = 3;
const HAS_NULL_BITMAP: u8 = 0x10;
const HAS_VARIABLE_COLUMNS: u8 = 0x20;

pub fn carries_version_info(bytes: &[u8]) -> bool {
    u8_at(bytes, 0).is_some_and(|status| status & 1 == 0 && status & VERSIONING_TAG != 0)
}

const PERSISTENT_STORE_PAGE: u32 = 0x8000_0000;
const IN_ROW_RELEASED_WORD: u16 = 0xDFFC;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Pointer { page: u32 },
    InRow { kind: u16, length: usize, word: u16 },
}

fn read_tag(tag: &[u8]) -> Option<Tag> {
    let tag = tag.get(..VERSIONING_TAG_LEN)?;
    let word = u16_at(tag, 6)?;
    if word & IN_ROW_FORM_MASK != IN_ROW_FORM {
        return Some(Tag::Pointer {
            page: u32_at(tag, 0)?,
        });
    }
    let kind_and_length = u16_at(tag, 4)?;
    Some(Tag::InRow {
        kind: kind_and_length & 0x1F,
        length: usize::from(kind_and_length >> 5),
        word,
    })
}

fn structure_end(bytes: &[u8]) -> Option<usize> {
    let status = u8_at(bytes, 0)?;
    let fixed_end = usize::from(u16_at(bytes, 2)?);
    if fixed_end < RECORD_HEADER {
        return None;
    }
    let columns = usize::from(u16_at(bytes, fixed_end)?);
    let mut end = fixed_end.checked_add(2)?;
    if status & HAS_NULL_BITMAP != 0 {
        end = end.checked_add(columns.div_ceil(8))?;
    }
    if status & HAS_VARIABLE_COLUMNS != 0 {
        let count = usize::from(u16_at(bytes, end)?);
        end = end.checked_add(2)?;
        if let Some(last) = count.checked_sub(1) {
            let at = end.checked_add(last.checked_mul(2)?)?;
            end = usize::from(u16_at(bytes, at)? & 0x7FFF);
        }
    }
    Some(end)
}

pub fn has_unreleased_version(kind: RecordKind, bytes: &[u8]) -> bool {
    if !carries_version_info(bytes) {
        return false;
    }
    let at = if kind == RecordKind::GhostVersion {
        Some(1)
    } else {
        structure_end(bytes)
    };
    let Some(tag) = at.and_then(|at| bytes.get(at..)).and_then(read_tag) else {
        return false;
    };
    match tag {
        Tag::Pointer { page } => page & PERSISTENT_STORE_PAGE != 0,
        Tag::InRow { kind, length, word } => {
            !(word == IN_ROW_RELEASED_WORD
                && length == 0
                && matches!(kind, IN_ROW_KIND_DELETED | IN_ROW_KIND_NONE))
        }
    }
}

fn version_info_room(rest: &[u8]) -> Option<usize> {
    let Some(tag) = read_tag(rest) else {
        return Some(VERSIONING_TAG_LEN);
    };
    match tag {
        Tag::Pointer { .. } => Some(VERSIONING_TAG_LEN),
        Tag::InRow { kind, length, .. } => match (kind, length) {
            (IN_ROW_KIND_DELETED | IN_ROW_KIND_NONE, 0) => Some(VERSIONING_TAG_LEN),
            (IN_ROW_KIND_BLOCK, 1..) => VERSIONING_TAG_LEN.checked_add(length),
            _ => None,
        },
    }
}

pub fn record_room(kind: RecordKind, bytes: &[u8]) -> Option<usize> {
    let status = u8_at(bytes, 0)?;
    if status & 1 != 0 {
        return None;
    }
    match kind {
        RecordKind::ForwardingStub => return Some(STUB_LEN),
        RecordKind::Forwarded if u8_at(bytes, 1)? != 0 => return None,
        RecordKind::GhostVersion if status & VERSIONING_TAG != 0 => {
            return Some(1 + VERSIONING_TAG_LEN);
        }
        RecordKind::Index | RecordKind::GhostIndex | RecordKind::BlobFragment => return None,
        _ => {}
    }
    let mut end = structure_end(bytes)?;
    if status & VERSIONING_TAG != 0 {
        let rest = bytes.get(end..).unwrap_or_default();
        end = end.checked_add(version_info_room(rest)?)?;
    }
    Some(end)
}

pub fn records<'a>(
    page: &'a PageBytes,
    header: &PageHeader,
    page_no: u32,
) -> Result<Vec<RecordRef<'a>>, Error> {
    record_spans(page, header, page_no)?
        .into_iter()
        .map(|span| {
            Ok(RecordRef {
                slot: span.slot,
                kind: span.kind,
                bytes: span.bytes(page).ok_or(Error::Malformed {
                    page: page_no,
                    what: "a record runs past the page",
                })?,
            })
        })
        .collect()
}

pub fn parse_fixed_var(page_no: u32, bytes: &[u8]) -> Result<FixedVarRecord<'_>, Error> {
    FixedVarRecord::parse(bytes).map_err(|error| match error {
        records::Error::Invalid(what) => Error::Malformed {
            page: page_no,
            what,
        },
        records::Error::Unsupported(what) => Error::Unsupported(what),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordId {
    pub page: u32,
    pub file: u16,
    pub slot: u16,
}

impl RecordId {
    pub fn from_bytes(bytes: &[u8], at: usize) -> Option<RecordId> {
        Some(RecordId {
            page: u32_at(bytes, at)?,
            file: u16_at(bytes, at.checked_add(4)?)?,
            slot: u16_at(bytes, at.checked_add(6)?)?,
        })
    }
}

pub fn stub_target(page_no: u32, bytes: &[u8]) -> Result<RecordId, Error> {
    let malformed = |what| Error::Malformed {
        page: page_no,
        what,
    };
    let status = u8_at(bytes, 0).ok_or_else(|| malformed("an empty record"))?;
    if RecordKind::from_status(status) != RecordKind::ForwardingStub {
        return Err(malformed(
            "a record that should be a forwarding stub is not",
        ));
    }
    RecordId::from_bytes(bytes, 1).ok_or_else(|| malformed("a forwarding stub is cut off"))
}

const BACK_POINTER_LEN: usize = 10;
const BACK_POINTER_MARK: [u8; 2] = [0x00, 0x04];

pub fn parse_forwarded(
    page_no: u32,
    bytes: &[u8],
) -> Result<(FixedVarRecord<'_>, RecordId), Error> {
    let malformed = |what| Error::Malformed {
        page: page_no,
        what,
    };
    let status = u8_at(bytes, 0).ok_or_else(|| malformed("an empty record"))?;
    if RecordKind::from_status(status) != RecordKind::Forwarded {
        return Err(malformed(
            "a record that should be a forwarded record is not",
        ));
    }
    if u8_at(bytes, 1) != Some(0) {
        return Err(Error::Unsupported("a ghost forwarded record"));
    }
    let mut record = parse_fixed_var(page_no, bytes)?;
    let pointer = record
        .variables
        .pop()
        .filter(|last| last.complex && last.data.len() == BACK_POINTER_LEN)
        .ok_or_else(|| malformed("a forwarded record has no back pointer"))?;
    if pointer.data.get(..2) != Some(&BACK_POINTER_MARK[..]) {
        return Err(Error::Unsupported(
            "a forwarded record with a back pointer of an unknown kind",
        ));
    }
    let back = RecordId::from_bytes(pointer.data, 2)
        .ok_or_else(|| malformed("a back pointer is cut off"))?;
    Ok((record, back))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Fixed { offset: u16, width: u16 },
    Bit { offset: u16, bit: u8 },
    Variable { ordinal: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysSlot {
    pub null_bit: u16,
    pub storage: Storage,
    pub column: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell<'a> {
    Null,
    Absent,
    Bytes(&'a [u8]),
    Complex(&'a [u8]),
    Bit(bool),
}

pub fn decode_cells<'a>(
    record: &FixedVarRecord<'a>,
    layout: &[PhysSlot],
    logical: usize,
) -> Result<Vec<Cell<'a>>, Error> {
    let known = layout
        .iter()
        .map(|slot| usize::from(slot.null_bit))
        .max()
        .unwrap_or(0);
    if record.column_count > known {
        return Err(Error::Inconsistent(
            "a record holds more columns than its rowset layout describes",
        ));
    }
    let mut cells: Vec<Option<Cell<'a>>> = Vec::new();
    cells
        .try_reserve_exact(logical)
        .map_err(|_| Error::Limit("columns of a table"))?;
    cells.resize(logical, None);
    for slot in layout {
        let Some(target) = slot.column else {
            continue;
        };
        let place = cells.get_mut(target).ok_or(Error::Inconsistent(
            "the rowset layout names a column the table does not have",
        ))?;
        if place.is_some() {
            return Err(Error::Inconsistent(
                "two physical columns hold one logical column",
            ));
        }
        *place = Some(cell_of(record, slot)?);
    }
    cells
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::Inconsistent(
            "a column of the table has no physical slot",
        ))
}

const RECORD_HEADER: usize = 4;

fn cell_of<'a>(record: &FixedVarRecord<'a>, slot: &PhysSlot) -> Result<Cell<'a>, Error> {
    let null_bit = usize::from(slot.null_bit);
    if null_bit == 0 {
        return Err(Error::Inconsistent("a physical column has null bit 0"));
    }
    if null_bit > record.column_count {
        return Ok(Cell::Absent);
    }
    if record.is_null(null_bit - 1) == Some(true) {
        return Ok(Cell::Null);
    }
    let fixed_at = |offset: u16| {
        usize::from(offset)
            .checked_sub(RECORD_HEADER)
            .ok_or(Error::Inconsistent(
                "a fixed column starts inside the record header",
            ))
    };
    match slot.storage {
        Storage::Fixed { offset, width } => {
            let data = slice(record.fixed, fixed_at(offset)?, usize::from(width)).ok_or(
                Error::Inconsistent("the fixed part of a record ends inside a column"),
            )?;
            Ok(Cell::Bytes(data))
        }
        Storage::Bit { offset, bit } => {
            if bit > 7 {
                return Err(Error::Inconsistent(
                    "a bit column has a bit position above 7",
                ));
            }
            let byte = u8_at(record.fixed, fixed_at(offset)?).ok_or(Error::Inconsistent(
                "the fixed part of a record ends before a bit column",
            ))?;
            Ok(Cell::Bit((byte >> bit) & 1 == 1))
        }
        Storage::Variable { ordinal } => {
            let index = usize::from(ordinal)
                .checked_sub(1)
                .ok_or(Error::Inconsistent("a variable column has ordinal 0"))?;
            Ok(match record.variables.get(index) {
                Some(variable) if variable.complex => Cell::Complex(variable.data),
                Some(variable) => Cell::Bytes(variable.data),
                None => Cell::Bytes(&[]),
            })
        }
    }
}
