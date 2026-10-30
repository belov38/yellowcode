use std::collections::HashSet;
use std::fmt;
use std::io;

use crate::blobstore::{BlobStore, ChainWriter};
use crate::catalog::CatalogEntry;
use crate::db::RowId;
use crate::error::{Constraint, Error, Location, NotFound, Result, Unsupported};
use crate::format::FormatInfo;
use crate::format::blob::{BLOCK, BLOCK_DATA, BlobBlock};
use crate::format::bytes::{OrCorrupt, put_u32le, u32le};
use crate::format::descr::{FieldDescr, FieldType, IndexField, TableDescr};
use crate::format::key::{KeyCtx, build_key, index_key_len};
use crate::format::layout::RecordLayout;
use crate::format::record::{
    decode_field, decode_row, encode_row, free_next, hidden_version, is_used, set_free_link,
};
use crate::format::value::{BlobRef, BlobVal, Row, Value};
use crate::ifile::IFile;
use crate::index::IndexRead;
use crate::index::write::{IndexHead, IndexLoc, IndexWriter};
use crate::pager::PageRead;
use crate::pager::txn::TxnPages;

pub struct TableRead<'a> {
    pr: &'a dyn PageRead,
    fi: FormatInfo,
    keys: KeyCtx<'a>,
    entry: &'a CatalogEntry,
    files: Stored,
}

#[derive(Debug)]
struct Stored {
    data: Option<IFile>,
    blob: Option<BlobStore>,
    num_slots: u32,
}

impl Stored {
    fn open(pr: &dyn PageRead, fi: FormatInfo, entry: &CatalogEntry) -> Result<Stored> {
        let [data_page, blob_page, _] = entry.descr.files;
        let record_len = u64::from(entry.layout.record_len.max(1));
        let (data, num_slots) = if data_page == 0 {
            (None, 0)
        } else {
            let f = IFile::open(pr, fi, data_page)?;
            if !f.length.is_multiple_of(record_len) {
                if pr.memory_budget().is_some() {
                    return Err(Error::CorruptStatic {
                        location: Location::InternalFile(data_page),
                        what: "data length is not a multiple of the record length",
                    });
                }
                return Err(Error::corrupt(
                    Location::Table(entry.descr.name.clone()),
                    format!(
                        "data length {} is not a multiple of the record length {record_len}",
                        f.length
                    ),
                ));
            }
            let slots = u32::try_from(f.length / record_len).map_err(|_| {
                if pr.memory_budget().is_some() {
                    return Error::CorruptStatic {
                        location: Location::InternalFile(data_page),
                        what: "too many record slots",
                    };
                }
                Error::corrupt(
                    Location::Table(entry.descr.name.clone()),
                    "too many record slots",
                )
            })?;
            (Some(f), slots)
        };
        let blob = if blob_page == 0 {
            None
        } else {
            let store = BlobStore::new(IFile::open(pr, fi, blob_page)?)?;
            Some(if pr.memory_budget().is_some() {
                store
            } else {
                store.named(&entry.descr.name)
            })
        };
        Ok(Stored {
            data,
            blob,
            num_slots,
        })
    }

    fn read_slot(
        &self,
        pr: &dyn PageRead,
        entry: &CatalogEntry,
        row: RowId,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        let name = &entry.descr.name;
        let Some(data) = self.data.as_ref().filter(|_| row < self.num_slots) else {
            return Err(Error::NotFound(NotFound::Row {
                table: name.clone(),
                row,
            }));
        };
        let len = u64::from(entry.layout.record_len);
        buf.clear();
        buf.resize(entry.layout.record_len as usize, 0);
        data.read_at(pr, u64::from(row) * len, buf)
            .map_err(|e| at_record(name, row, e))
    }

    fn blob_store(&self, entry: &CatalogEntry) -> Result<&BlobStore> {
        self.blob.as_ref().ok_or_else(|| {
            Error::corrupt(
                Location::Table(entry.descr.name.clone()),
                "a blob value is referenced but the table has no blob file",
            )
        })
    }

    fn blob_bytes(&self, pr: &dyn PageRead, entry: &CatalogEntry, r: BlobRef) -> Result<Vec<u8>> {
        if r.len == 0 {
            return Ok(Vec::new());
        }
        self.blob_store(entry)?.read_value(pr, r)
    }
}

pub type RowRead<'s> = (&'s Row, &'s mut memory_budget::Scratch<Vec<u8>>);

#[derive(Debug)]
pub struct RowScratch {
    raw: memory_budget::Scratch<Vec<u8>>,
    row: memory_budget::Scratch<Row>,
    blob: memory_budget::Scratch<Vec<u8>>,
}

impl RowScratch {
    pub fn try_new(context: &memory_budget::RequestMemory) -> Result<Self> {
        fn new<T>(context: &memory_budget::RequestMemory) -> Result<memory_budget::Scratch<T>> {
            memory_budget::Scratch::try_new(&context.budget, memory_budget::Category::Row)
                .map_err(|e| Error::Memory(memory_budget::BudgetError::Limit(e)))
        }
        Ok(Self {
            raw: new(context)?,
            row: new(context)?,
            blob: new(context)?,
        })
    }
    pub fn held(&self) -> u64 {
        self.raw.held() + self.row.held() + self.blob.held()
    }
    pub fn release(&mut self) {
        for scratch in [&mut self.raw, &mut self.blob] {
            scratch.clear();
            scratch.trim();
        }
        self.row.clear();
        self.row.trim();
    }
}

impl<'a> TableRead<'a> {
    pub(crate) fn open(
        pr: &'a dyn PageRead,
        fi: FormatInfo,
        entry: &'a CatalogEntry,
        keys: KeyCtx<'a>,
    ) -> Result<TableRead<'a>> {
        Ok(TableRead {
            pr,
            fi,
            keys,
            entry,
            files: Stored::open(pr, fi, entry)?,
        })
    }

    pub fn descr(&self) -> &TableDescr {
        &self.entry.descr
    }

    pub fn strings_equal(&self, field: &str, left: &str, right: &str) -> Result<bool> {
        Ok(self.string_key(field, left)? == self.string_key(field, right)?)
    }

    pub fn string_key(&self, field: &str, text: &str) -> Result<Vec<u8>> {
        let field = self.string_field(field)?;
        let collate = self
            .keys
            .collate
            .ok_or(Error::Unsupported(Unsupported::NoCollator))?;
        crate::collate::field_string_key(collate, self.keys.locale, field, text)
    }

    #[cfg(feature = "icu")]
    pub fn string_key_budgeted(
        &self,
        field: &str,
        text: &str,
        keys: &crate::collate::KeyRequest,
    ) -> Result<memory_budget::BudgetedVec<u8>> {
        use crate::collate::prepared;
        let field = self.string_field(field)?;
        let trimmed = text.trim_end_matches(' ');
        let mut units = prepared(memory_budget::BudgetedVec::try_with_capacity(
            keys.budget(),
            memory_budget::Category::Execution,
            trimmed.encode_utf16().count(),
        ))?;
        for unit in trimmed.encode_utf16() {
            prepared(units.try_push(unit))?;
        }
        let strength = if field.case_sensitive {
            crate::collate::Strength::Tertiary
        } else {
            crate::collate::Strength::Secondary
        };
        keys.sort_key(self.keys.locale, strength, units.as_slice())
    }

    #[cfg(feature = "icu")]
    pub fn strings_equal_budgeted(
        &self,
        field: &str,
        left: &str,
        right: &str,
        keys: &crate::collate::KeyRequest,
    ) -> Result<bool> {
        let left = self.string_key_budgeted(field, left, keys)?;
        let right = self.string_key_budgeted(field, right, keys)?;
        Ok(left.as_slice() == right.as_slice())
    }

    fn string_field(&self, field: &str) -> Result<&FieldDescr> {
        let (_, field) = self.descr().field(field).ok_or_else(|| {
            Error::Constraint(Constraint::BadDescr(format!(
                "unknown string field {}",
                crate::error::brief(field)
            )))
        })?;
        if !matches!(field.ty, FieldType::NC | FieldType::NVC) {
            return Err(Error::Constraint(Constraint::BadDescr(format!(
                "{} is not an NC/NVC field",
                field.name
            ))));
        }
        Ok(field)
    }

    pub fn layout(&self) -> &RecordLayout {
        &self.entry.layout
    }

    pub fn num_slots(&self) -> u32 {
        self.files.num_slots
    }

    pub fn index(&self, name: &str) -> Result<IndexRead<'_>> {
        IndexRead::open(self, name)
    }

    pub(crate) fn pager(&self) -> &'a dyn PageRead {
        self.pr
    }

    pub(crate) fn format_info(&self) -> FormatInfo {
        self.fi
    }

    pub(crate) fn key_ctx(&self) -> KeyCtx<'a> {
        self.keys
    }

    fn name(&self) -> &str {
        &self.entry.descr.name
    }

    fn not_found(&self, row: RowId) -> Error {
        Error::NotFound(NotFound::Row {
            table: self.name().to_string(),
            row,
        })
    }

    fn read_slot(&self, row: RowId, buf: &mut Vec<u8>) -> Result<()> {
        self.files.read_slot(self.pr, self.entry, row, buf)
    }

    fn at_record(&self, row: RowId, e: Error) -> Error {
        at_record(self.name(), row, e)
    }

    fn decode_record(&self, row: RowId, rec: &[u8]) -> Result<Row> {
        decode_row(self.descr(), self.layout(), rec).map_err(|e| self.at_record(row, e))
    }

    fn slots(&self) -> impl Iterator<Item = (RowId, Result<Row>)> + '_ {
        self.slots_from(1)
    }

    fn slots_from(&self, start: RowId) -> impl Iterator<Item = (RowId, Result<Row>)> + '_ {
        self.slots_in(start, self.files.num_slots)
    }

    fn slots_in(
        &self,
        start: RowId,
        end: RowId,
    ) -> impl Iterator<Item = (RowId, Result<Row>)> + '_ {
        let mut buf = Vec::new();
        (start.max(1)..end.min(self.files.num_slots)).filter_map(move |row| {
            match self.read_slot(row, &mut buf) {
                Err(e) => Some((row, Err(e))),
                Ok(()) if is_used(&buf) => Some((row, self.decode_record(row, &buf))),
                Ok(()) => None,
            }
        })
    }

    pub fn scan(&self) -> impl Iterator<Item = Result<(RowId, Row)>> + '_ {
        self.scan_from(1)
    }

    pub fn scan_from(&self, start: RowId) -> impl Iterator<Item = Result<(RowId, Row)>> + '_ {
        self.slots_from(start)
            .map(|(id, row)| row.map(|row| (id, row)))
    }

    pub fn scan_range(
        &self,
        start: RowId,
        end: RowId,
    ) -> impl Iterator<Item = Result<(RowId, Row)>> + '_ {
        self.slots_in(start, end)
            .map(|(id, row)| row.map(|row| (id, row)))
    }

    #[allow(clippy::type_complexity)]
    pub fn scan_lossy(&self) -> (Vec<(RowId, Row)>, Vec<(RowId, Error)>) {
        let (mut rows, mut errors) = (Vec::new(), Vec::new());
        for (id, r) in self.slots() {
            match r {
                Ok(row) => rows.push((id, row)),
                Err(e) => {
                    let fatal = e.is_fatal();
                    errors.push((id, e));
                    if fatal {
                        break;
                    }
                }
            }
        }
        (rows, errors)
    }

    pub fn get(&self, row: RowId) -> Result<Option<Row>> {
        let mut buf = Vec::new();
        self.read_slot(row, &mut buf)?;
        if row == 0 || !is_used(&buf) {
            return Ok(None);
        }
        self.decode_record(row, &buf).map(Some)
    }

    pub fn get_budgeted(
        &self,
        row: RowId,
        context: &memory_budget::RequestMemory,
    ) -> Result<Option<memory_budget::Charged<Row>>> {
        let raw = self.raw_record_budgeted(row, context)?;
        if row == 0 || !is_used(&raw) {
            return Ok(None);
        }
        crate::format::record::decode_row_budgeted(self.descr(), self.layout(), &raw, context)
            .map(Some)
    }

    pub fn get_budgeted_into<'s>(
        &self,
        row: RowId,
        context: &memory_budget::RequestMemory,
        scratch: &'s mut RowScratch,
    ) -> Result<Option<RowRead<'s>>> {
        let RowScratch {
            raw,
            row: decoded,
            blob,
        } = scratch;
        let raw = self.raw_record_into(row, context, raw)?;
        if row == 0 || !is_used(raw) {
            return Ok(None);
        }
        let decoded = crate::format::record::decode_row_into(
            self.descr(),
            self.layout(),
            raw,
            decoded,
            memory_budget::Category::Row,
            context.row_limit,
        )?;
        Ok(Some((decoded, blob)))
    }

    fn raw_record_into<'s>(
        &self,
        row: RowId,
        context: &memory_budget::RequestMemory,
        scratch: &'s mut memory_budget::Scratch<Vec<u8>>,
    ) -> Result<&'s [u8]> {
        use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
        let data = self
            .files
            .data
            .as_ref()
            .filter(|_| row < self.files.num_slots)
            .ok_or_else(|| {
                Error::NotFound(NotFound::Row {
                    table: String::new(),
                    row,
                })
            })?;
        let length =
            usize::try_from(self.layout().record_len).map_err(|_| BudgetError::Overflow)?;
        let bytes = length
            .checked_add(std::mem::size_of::<BudgetedVec<u8>>())
            .ok_or(BudgetError::Overflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?;
        if bytes > context.row_limit {
            return Err(BudgetError::Limit(MemoryLimit {
                category: memory_budget::Category::Row,
                requested: bytes,
                reason: LimitReason::Category,
            })
            .into());
        }
        let offset = u64::from(row)
            .checked_mul(u64::from(self.layout().record_len))
            .ok_or(BudgetError::Overflow)?;
        let out = scratch.refill(bytes, |grant| -> Result<Vec<u8>> {
            let mut out = grant.try_vec_unowned::<u8>(length)?;
            memory_budget::resize_within(&mut out, length, 0)?;
            data.read_at(self.pr, offset, &mut out)?;
            Ok(out)
        })?;
        Ok(out.as_slice())
    }

    pub fn blob_bytes_into<'s>(
        &self,
        r: BlobRef,
        max: Option<usize>,
        context: &memory_budget::RequestMemory,
        scratch: &'s mut memory_budget::Scratch<Vec<u8>>,
    ) -> Result<&'s [u8]> {
        use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
        let category = memory_budget::Category::Row;
        if r.len == 0 {
            let bytes = std::mem::size_of::<BudgetedVec<u8>>() as u64;
            if bytes > context.row_limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category,
                    requested: bytes,
                    reason: LimitReason::Category,
                })
                .into());
            }
            let out = scratch.refill(bytes, |grant| grant.try_vec_unowned::<u8>(0))?;
            return Ok(out.as_slice());
        }
        self.files
            .blob
            .as_ref()
            .ok_or(Error::CorruptStatic {
                location: Location::Root,
                what: "missing BLOB storage",
            })?
            .read_value_into(self.pr, r, max, scratch, category, context.row_limit)
    }

    pub fn get_managed(
        &self,
        row: RowId,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        limit: u64,
    ) -> Result<Option<memory_budget::Charged<Row>>> {
        let raw = self.raw_record_managed(row, budget, category, limit)?;
        if row == 0 || !is_used(&raw) {
            return Ok(None);
        }
        crate::format::record::decode_row_managed(
            self.descr(),
            self.layout(),
            &raw,
            budget,
            category,
            limit,
        )
        .map(Some)
    }

    pub fn scan_from_budgeted<'b>(
        &'b self,
        start: RowId,
        context: &'b memory_budget::RequestMemory,
    ) -> impl Iterator<Item = Result<(RowId, memory_budget::Charged<Row>)>> + 'b {
        let mut rows = start.max(1)..self.files.num_slots;
        let mut stopped = false;
        std::iter::from_fn(move || {
            if stopped {
                return None;
            }
            for row in rows.by_ref() {
                match self.get_budgeted(row, context) {
                    Ok(Some(value)) => return Some(Ok((row, value))),
                    Ok(None) => {}
                    Err(error) => {
                        stopped = error.is_fatal();
                        return Some(Err(error));
                    }
                }
            }
            None
        })
    }

    pub fn raw_record_budgeted(
        &self,
        row: RowId,
        context: &memory_budget::RequestMemory,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        self.raw_record_managed(
            row,
            &context.budget,
            memory_budget::Category::Row,
            context.row_limit,
        )
    }

    pub fn raw_record_managed(
        &self,
        row: RowId,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        limit: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
        let data = self
            .files
            .data
            .as_ref()
            .filter(|_| row < self.files.num_slots)
            .ok_or_else(|| {
                Error::NotFound(NotFound::Row {
                    table: String::new(),
                    row,
                })
            })?;
        let length =
            usize::try_from(self.layout().record_len).map_err(|_| BudgetError::Overflow)?;
        let bytes = length
            .checked_add(std::mem::size_of::<BudgetedVec<u8>>())
            .ok_or(BudgetError::Overflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| BudgetError::Overflow)?;
        if bytes > limit {
            return Err(BudgetError::Limit(MemoryLimit {
                category,
                requested: bytes,
                reason: LimitReason::Category,
            })
            .into());
        }
        let mut output = BudgetedVec::try_with_capacity(budget, category, length)?;
        output.try_resize(length, 0)?;
        let offset = u64::from(row)
            .checked_mul(u64::from(self.layout().record_len))
            .ok_or(BudgetError::Overflow)?;
        data.read_at(self.pr, offset, &mut output)?;
        Ok(output.into_charged_vec())
    }

    pub fn raw_record(&self, row: RowId) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.read_slot(row, &mut buf)?;
        Ok(buf)
    }

    pub fn hidden_version(&self, row: RowId) -> Result<Option<[u32; 2]>> {
        let mut buf = Vec::new();
        self.read_slot(row, &mut buf)?;
        if row == 0 || !is_used(&buf) {
            return Ok(None);
        }
        Ok(hidden_version(self.layout(), &buf))
    }

    pub fn free_chain(&self) -> Result<Vec<RowId>> {
        let num_slots = self.files.num_slots;
        if num_slots == 0 {
            return Ok(Vec::new());
        }
        let at = |row| Location::Record {
            table: self.name().to_string(),
            row,
        };
        let mut buf = Vec::new();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = 0;
        loop {
            self.read_slot(cur, &mut buf)?;
            let next = free_next(&buf).ok_or_else(|| {
                Error::corrupt(at(cur), "record is too short for a free-list link")
            })?;
            if next == 0 {
                return Ok(out);
            }
            if next >= num_slots {
                return Err(Error::corrupt(
                    at(cur),
                    format!("free record link {next} is beyond the {num_slots} slots"),
                ));
            }
            if !seen.insert(next) {
                return Err(Error::corrupt(
                    at(cur),
                    format!("free record chain has a cycle at slot {next}"),
                ));
            }
            out.push(next);
            cur = next;
        }
    }

    pub fn blob(&self, row: RowId, field: &str) -> Result<BlobReader<'_>> {
        let (i, f) = self.descr().field(field).ok_or_else(|| {
            Error::NotFound(NotFound::Field(crate::error::brief(field).to_owned()))
        })?;
        if !f.ty.is_blob() {
            return Err(Error::Constraint(Constraint::TypeMismatch {
                table: self.name().to_string(),
                field: f.name.clone(),
            }));
        }
        let mut rec = Vec::new();
        self.read_slot(row, &mut rec)?;
        if row == 0 || !is_used(&rec) {
            return Err(self.not_found(row));
        }
        let value = decode_field(self.descr(), self.layout(), i, &rec)
            .map_err(|e| self.at_record(row, e))?;
        match value {
            Value::Null => Ok(BlobReader::empty(self.pr)),
            Value::Text(BlobVal::Ref(r))
            | Value::NText(BlobVal::Ref(r))
            | Value::Image(BlobVal::Ref(r)) => self.blob_reader(r),
            _ => Err(Error::corrupt(
                Location::Record {
                    table: self.name().to_string(),
                    row,
                },
                format!("field {} does not hold a blob reference", f.name),
            )),
        }
    }

    fn blob_reader(&self, r: BlobRef) -> Result<BlobReader<'_>> {
        if r.len == 0 {
            return Ok(BlobReader::empty(self.pr));
        }
        let store = self.blob_store()?;
        let mut blocks = Vec::new();
        let mut ends = Vec::new();
        let mut total = 0u64;
        for (block, used) in store.value_blocks(self.pr, r)? {
            total += u64::from(used);
            blocks.push(block);
            ends.push(total);
        }
        Ok(BlobReader {
            pr: self.pr,
            store: Some(store),
            blocks,
            ends,
            len: total,
            pos: 0,
            cur: None,
        })
    }

    fn blob_store(&self) -> Result<&BlobStore> {
        self.files.blob_store(self.entry)
    }

    pub fn blob_bytes_budgeted(
        &self,
        r: BlobRef,
        context: &memory_budget::RequestMemory,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        self.blob_bytes_managed(
            r,
            &context.budget,
            memory_budget::Category::Row,
            context.row_limit,
        )
    }

    pub fn blob_bytes_managed(
        &self,
        r: BlobRef,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
        limit: u64,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        if r.len == 0 {
            use memory_budget::{BudgetError, BudgetedVec, LimitReason, MemoryLimit};
            let bytes = std::mem::size_of::<BudgetedVec<u8>>() as u64;
            if bytes > limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category,
                    requested: bytes,
                    reason: LimitReason::Category,
                })
                .into());
            }
            return Ok(BudgetedVec::new(budget, category)?.into_charged_vec());
        }
        self.files
            .blob
            .as_ref()
            .ok_or(Error::CorruptStatic {
                location: Location::Root,
                what: "missing BLOB storage",
            })?
            .read_value_managed(self.pr, r, budget, category, limit)
    }

    pub fn blob_bytes(&self, r: BlobRef) -> Result<Vec<u8>> {
        self.files.blob_bytes(self.pr, self.entry, r)
    }

    pub fn blob_prefix(&self, r: BlobRef, max_bytes: usize) -> Result<Vec<u8>> {
        if r.len == 0 {
            return Ok(Vec::new());
        }
        self.blob_store()?.read_value_prefix(self.pr, r, max_bytes)
    }

    pub fn blob_prefix_budgeted(
        &self,
        r: BlobRef,
        max_bytes: usize,
        context: &memory_budget::RequestMemory,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        if u64::from(r.len) <= max_bytes as u64 {
            return self.blob_bytes_budgeted(r, context);
        }
        self.blob_store()?.read_value_prefix_managed(
            self.pr,
            r,
            max_bytes,
            &context.budget,
            memory_budget::Category::Row,
            context.row_limit,
        )
    }
}

fn at_record(table: &str, row: RowId, e: Error) -> Error {
    let Error::Corrupt { location, what } = e else {
        return e;
    };
    let record = Location::Record {
        table: table.to_string(),
        row,
    };
    match location {
        Location::Record { .. } => Error::Corrupt {
            location: record,
            what,
        },
        Location::Table(_) => Error::corrupt(record, what),
        other => Error::corrupt(record, format!("{what} ({other})")),
    }
}

impl fmt::Debug for TableRead<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableRead")
            .field("name", &self.name())
            .field("num_slots", &self.files.num_slots)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct RowSource {
    entry: CatalogEntry,
    files: Stored,
}

impl RowSource {
    pub(crate) fn open(
        pr: &dyn PageRead,
        fi: FormatInfo,
        entry: &CatalogEntry,
    ) -> Result<RowSource> {
        Ok(RowSource {
            entry: entry.clone(),
            files: Stored::open(pr, fi, entry)?,
        })
    }

    fn row(&self, pr: &dyn PageRead, row: RowId, buf: &mut Vec<u8>) -> Result<Option<Row>> {
        self.files.read_slot(pr, &self.entry, row, buf)?;
        if !is_used(buf) {
            return Ok(None);
        }
        let (d, l) = (&*self.entry.descr, &*self.entry.layout);
        (decode_row(d, l, buf).map(Some)).map_err(|e| at_record(&d.name, row, e))
    }
}

pub struct BlobReader<'a> {
    pr: &'a dyn PageRead,
    store: Option<&'a BlobStore>,
    blocks: Vec<u32>,
    ends: Vec<u64>,
    len: u64,
    pos: u64,
    cur: Option<(usize, BlobBlock)>,
}

impl fmt::Debug for BlobReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobReader")
            .field("len", &self.len)
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl<'a> BlobReader<'a> {
    fn empty(pr: &'a dyn PageRead) -> BlobReader<'a> {
        BlobReader {
            pr,
            store: None,
            blocks: Vec::new(),
            ends: Vec::new(),
            len: 0,
            pos: 0,
            cur: None,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn load(&mut self, i: usize) -> Result<&BlobBlock> {
        if self.cur.as_ref().map(|c| c.0) != Some(i) {
            let (Some(store), Some(&no)) = (self.store, self.blocks.get(i)) else {
                return Err(Error::corrupt(
                    Location::File,
                    "blob block index is out of range",
                ));
            };
            self.cur = Some((i, store.block(self.pr, no)?));
        }
        match &self.cur {
            Some((_, blk)) => Ok(blk),
            None => Err(Error::corrupt(Location::File, "blob block is not loaded")),
        }
    }
}

impl BlobReader<'_> {
    fn read_from_block(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        let damaged = |what: &str| io::Error::other(what.to_string());
        let i = self.ends.partition_point(|&end| end <= self.pos);
        let start = i
            .checked_sub(1)
            .and_then(|prev| self.ends.get(prev))
            .copied()
            .unwrap_or(0);
        let in_block = usize::try_from(self.pos - start).unwrap_or(usize::MAX);
        let blk = self.load(i).map_err(io::Error::other)?;
        let avail = blk
            .bytes()
            .get(in_block..)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| damaged("blob block is shorter than recorded"))?;
        let n = avail.len().min(dst.len());
        let (Some(src), Some(dst)) = (avail.get(..n), dst.get_mut(..n)) else {
            return Err(damaged("blob read range is out of bounds"));
        };
        dst.copy_from_slice(src);
        self.pos += n as u64;
        Ok(n)
    }
}

impl io::Read for BlobReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        while self.pos < self.len {
            let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) else {
                break;
            };
            match self.read_from_block(rest) {
                Ok(n) => filled += n,
                Err(_) if filled > 0 => break,
                Err(e) => return Err(e),
            }
        }
        Ok(filled)
    }
}

impl io::Seek for BlobReader<'_> {
    fn seek(&mut self, from: io::SeekFrom) -> io::Result<u64> {
        let target = match from {
            io::SeekFrom::Start(n) => i128::from(n),
            io::SeekFrom::End(d) => i128::from(self.len) + i128::from(d),
            io::SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        let target = u64::try_from(target).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a position before the start of the blob",
            )
        })?;
        self.pos = target;
        Ok(target)
    }
}

const BLOB_LENGTH_LIMIT: &str = "blob value length limit";

pub(crate) fn check_writable(d: &TableDescr) -> Result<()> {
    if d.recordlock {
        return Err(Error::Unsupported(Unsupported::Recordlock {
            table: d.name.clone(),
        }));
    }
    let unsupported = |field: &str, ty| {
        Err(Error::Unsupported(Unsupported::FieldType {
            table: d.name.clone(),
            field: field.to_string(),
            ty,
        }))
    };
    let odd = |f: &&FieldDescr| matches!(f.ty, FieldType::C | FieldType::VC | FieldType::D);
    if let Some(f) = d.fields.iter().find(odd) {
        return unsupported(&f.name, f.ty.code());
    }
    let cs_string = |name: &str| {
        d.field(name)
            .is_some_and(|(_, f)| f.ty.is_string() && f.case_sensitive)
    };
    if let Some(ix) = d
        .indexes
        .iter()
        .find(|ix| ix.fields.iter().any(|k| cs_string(&k.name)))
    {
        return Err(Error::Unsupported(Unsupported::CsStringIndex {
            table: d.name.clone(),
            index: ix.name.clone(),
        }));
    }
    Ok(())
}

pub(crate) fn has_string_keys(d: &TableDescr) -> bool {
    let string = |k: &IndexField| d.field(&k.name).is_some_and(|(_, f)| f.ty.is_string());
    d.indexes.iter().any(|ix| ix.fields.iter().any(string))
}

#[derive(Debug)]
pub(crate) struct TableFiles {
    data: IFile,
    blob: Option<BlobStore>,
    index: Option<IFile>,
    version: Option<u32>,
    pending: Option<ChainWriter>,
    heads: Vec<Option<IndexHead>>,
}

impl TableFiles {
    pub(crate) fn open(
        pr: &dyn PageRead,
        fi: FormatInfo,
        entry: &CatalogEntry,
    ) -> Result<TableFiles> {
        let d = &entry.descr;
        let bad = |what: String| Error::corrupt(Location::Table(d.name.clone()), what);
        let unsupported = |field: &str, ty| {
            Err(Error::Unsupported(Unsupported::FieldType {
                table: d.name.clone(),
                field: field.to_string(),
                ty,
            }))
        };
        let [data_page, blob_page, index_page] = d.files;
        if data_page == 0 {
            return unsupported("Files", "table without a data file");
        }
        if !d.indexes.is_empty() && index_page == 0 {
            return Err(bad(format!(
                "{} indexes are described but the table has no index file",
                d.indexes.len()
            )));
        }
        if let Some(f) = d
            .fields
            .iter()
            .find(|f| f.ty.is_blob())
            .filter(|_| blob_page == 0)
        {
            return unsupported(&f.name, "blob field without a blob file");
        }
        let data = IFile::open(pr, fi, data_page)?.owned_by(&d.name, "data");
        let record_len = u64::from(entry.layout.record_len.max(1));
        let slots = data.length / record_len;
        if !data.length.is_multiple_of(record_len) || u32::try_from(slots).is_err() {
            return Err(bad(format!(
                "data length {} is not a whole number of records of {record_len} bytes",
                data.length
            )));
        }
        let blob = match blob_page {
            0 => None,
            p => Some(BlobStore::new(IFile::open(pr, fi, p)?)?.named(&d.name)),
        };
        let index = match d.indexes.is_empty() {
            true => None,
            false => Some(IFile::open(pr, fi, index_page)?.owned_by(&d.name, "index")),
        };
        Ok(TableFiles::created(data, blob, index, d.indexes.len()))
    }

    pub(crate) fn created(
        data: IFile,
        blob: Option<BlobStore>,
        index: Option<IFile>,
        n_indexes: usize,
    ) -> TableFiles {
        TableFiles {
            data,
            blob,
            index,
            version: None,
            pending: None,
            heads: vec![None; n_indexes],
        }
    }

    pub(crate) fn settle(&mut self, tx: &mut TxnPages) -> Result<()> {
        let Some(chain) = self.pending.take() else {
            return Ok(());
        };
        match self.blob.as_mut() {
            Some(store) => store.abandon_chain(tx, chain),
            None => tx.run(|_| {
                Err(Error::corrupt(
                    Location::File,
                    "a blob value is pending in a table without a blob file",
                ))
            }),
        }
    }
}

fn blob_of(ty: FieldType, v: &Value) -> Option<&BlobVal> {
    match (ty, v) {
        (FieldType::T, Value::Text(b))
        | (FieldType::NT, Value::NText(b))
        | (FieldType::I, Value::Image(b)) => Some(b),
        _ => None,
    }
}

fn blob_value(ty: FieldType, b: BlobVal) -> Value {
    match ty {
        FieldType::T => Value::Text(b),
        FieldType::NT => Value::NText(b),
        _ => Value::Image(b),
    }
}

const NO_BLOB: BlobRef = BlobRef { start: 0, len: 0 };

enum BlobPlan<'r> {
    Store(&'r [u8]),
    Keep(BlobRef),
    Null,
}

struct Prepared<'r> {
    row: Row,
    blobs: Vec<(usize, BlobPlan<'r>)>,
}

struct Current {
    rec: Vec<u8>,
    blobs: Vec<(usize, Option<BlobRef>)>,
}

impl Current {
    fn blob(&self, i: usize) -> Option<BlobRef> {
        self.blobs
            .iter()
            .find(|(j, _)| *j == i)
            .and_then(|(_, r)| *r)
    }
}

struct Target<'t> {
    fi: FormatInfo,
    entry: &'t CatalogEntry,
    files: &'t mut TableFiles,
    rv_next: &'t mut u32,
    keys: KeyCtx<'t>,
}

struct IndexStep {
    i: usize,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
}

fn aborting<T>(tx: &mut TxnPages, e: Error) -> Result<T> {
    if tx.check_alive().is_err() {
        return Err(e);
    }
    tx.run(|_| Err(e))
}

impl<'t> Target<'t> {
    fn reborrow(&mut self) -> Target<'_> {
        Target {
            fi: self.fi,
            entry: self.entry,
            files: &mut *self.files,
            rv_next: &mut *self.rv_next,
            keys: self.keys,
        }
    }

    fn descr(&self) -> &'t TableDescr {
        &self.entry.descr
    }

    fn layout(&self) -> &'t RecordLayout {
        &self.entry.layout
    }

    fn name(&self) -> &'t str {
        &self.entry.descr.name
    }

    fn record_len(&self) -> u64 {
        u64::from(self.layout().record_len.max(1))
    }

    fn num_slots(&self) -> u32 {
        u32::try_from(self.files.data.length / self.record_len()).unwrap_or(u32::MAX)
    }

    fn not_found(&self, row: RowId) -> Error {
        Error::NotFound(NotFound::Row {
            table: self.name().to_string(),
            row,
        })
    }

    fn record_loc(&self, row: RowId) -> Location {
        Location::Record {
            table: self.name().to_string(),
            row,
        }
    }

    fn misfit(&self, f: &FieldDescr, why: &str) -> Error {
        Error::Constraint(Constraint::ValueDoesNotFit {
            table: self.name().to_string(),
            field: f.name.clone(),
            why: why.into(),
        })
    }

    fn in_field(&self, e: Error, i: usize) -> Error {
        match (e, self.descr().fields.get(i)) {
            (Error::Constraint(Constraint::ValueDoesNotFit { why, .. }), Some(f)) => {
                self.misfit(f, &why)
            }
            (e, _) => e,
        }
    }

    fn store(&mut self) -> Result<&mut BlobStore> {
        let table = self.name();
        self.files.blob.as_mut().ok_or_else(|| {
            Error::corrupt(Location::Table(table.to_string()), "table has no blob file")
        })
    }

    fn read_record(&self, pr: &dyn PageRead, row: RowId) -> Result<Vec<u8>> {
        let rl = self.record_len();
        let mut rec = vec![0u8; self.layout().record_len as usize];
        let off = u64::from(row).saturating_mul(rl);
        (self.files.data.read_at(pr, off, &mut rec)).map_err(|e| at_record(self.name(), row, e))?;
        Ok(rec)
    }

    fn write_record(&mut self, tx: &mut TxnPages, row: RowId, rec: &[u8]) -> Result<()> {
        let off = u64::from(row).saturating_mul(self.record_len());
        let name = self.name();
        (self.files.data.write_at(tx, off, rec)).map_err(|e| at_record(name, row, e))
    }

    fn free_head(&self, pr: &dyn PageRead) -> Result<u32> {
        let mut b = [0u8; 4];
        (self.files.data.read_at(pr, 1, &mut b)).map_err(|e| at_record(self.name(), 0, e))?;
        Ok(u32::from_le_bytes(b))
    }

    fn set_free_head(&mut self, tx: &mut TxnPages, head: u32) -> Result<()> {
        let name = self.name();
        (self.files.data.write_at(tx, 1, &head.to_le_bytes())).map_err(|e| at_record(name, 0, e))
    }

    fn blob_refs(&self, row: RowId, rec: &[u8]) -> Result<Vec<(usize, Option<BlobRef>)>> {
        let (d, l) = (self.descr(), self.layout());
        let blob_fields = d.fields.iter().enumerate().filter(|(_, f)| f.ty.is_blob());
        let mut out = Vec::new();
        for (i, f) in blob_fields {
            let v = decode_field(d, l, i, rec).map_err(|e| at_record(self.name(), row, e))?;
            let r = match (&v, blob_of(f.ty, &v)) {
                (Value::Null, _) => None,
                (_, Some(BlobVal::Ref(r))) => Some(*r),
                _ => {
                    return Err(Error::corrupt(
                        self.record_loc(row),
                        format!("field {} does not hold a blob reference", f.name),
                    ));
                }
            };
            out.push((i, r));
        }
        Ok(out)
    }

    fn encode(&self, row: &Row) -> Result<Vec<u8>> {
        let mut rec = vec![0u8; self.layout().record_len as usize];
        encode_row(self.descr(), self.layout(), row, &mut rec)?;
        Ok(rec)
    }

    fn prepare<'r>(&self, row: &'r Row, insert: bool) -> Result<Prepared<'r>> {
        let d = self.descr();
        if row.len() != d.fields.len() {
            return Err(Error::Constraint(Constraint::WrongArity {
                table: d.name.clone(),
                expected: d.fields.len(),
                got: row.len(),
            }));
        }
        let mut out = Vec::with_capacity(row.len());
        let mut blobs = Vec::new();
        for (i, (f, v)) in d.fields.iter().zip(row).enumerate() {
            let value = match (f.ty, blob_of(f.ty, v)) {
                (FieldType::RV, _) => Value::RowVersion([0; 4]),
                (_, Some(BlobVal::Data(b))) => {
                    if u32::try_from(b.len()).is_err() {
                        return Err(self.misfit(f, BLOB_LENGTH_LIMIT));
                    }
                    blobs.push((i, BlobPlan::Store(b.as_slice())));
                    blob_value(f.ty, BlobVal::Ref(NO_BLOB))
                }
                (_, Some(BlobVal::Ref(r))) => {
                    if insert {
                        return Err(self.misfit(
                            f,
                            "a new row takes the bytes of a blob value, not a reference",
                        ));
                    }
                    blobs.push((i, BlobPlan::Keep(*r)));
                    v.clone()
                }
                (ty, None) => {
                    if ty.is_blob() && f.nullable && *v == Value::Null {
                        blobs.push((i, BlobPlan::Null));
                    }
                    v.clone()
                }
            };
            out.push(value);
        }
        self.encode(&out)?;
        Ok(Prepared { row: out, blobs })
    }

    fn check_kept(&self, p: &Prepared<'_>, cur: &Current) -> Result<()> {
        for (i, plan) in &p.blobs {
            let (BlobPlan::Keep(r), Some(f)) = (plan, self.descr().fields.get(*i)) else {
                continue;
            };
            if cur.blob(*i) != Some(*r) {
                return Err(self.misfit(
                    f,
                    "a blob reference has to be the value stored in the field",
                ));
            }
        }
        Ok(())
    }

    fn check_room(&self, p: &Prepared<'_>) -> Result<()> {
        let Some(store) = &self.files.blob else {
            return Ok(());
        };
        let n = u64::from(store.nblocks());
        let mut blocks = n + u64::from(n == 0);
        for (i, plan) in &p.blobs {
            if let BlobPlan::Store(b) = plan {
                blocks = blocks.saturating_add((b.len() as u64).div_ceil(BLOCK_DATA as u64));
                let len = blocks.saturating_mul(BLOCK as u64);
                (store.file.check_len(self.fi, len)).map_err(|e| self.in_field(e, *i))?;
            }
        }
        Ok(())
    }

    fn version(&mut self, tx: &TxnPages) -> Result<Option<(u32, u32)>> {
        if !self.layout().has_rowversion {
            return Ok(None);
        }
        let v = match self.files.version {
            Some(v) => v,
            None => {
                let v = tx.v1_after_commit(self.files.data.header_page)?;
                self.files.version = Some(v);
                v
            }
        };
        Ok(Some((v, *self.rv_next)))
    }

    fn next_version(&mut self, tx: &TxnPages) -> Result<Option<(u32, u32)>> {
        let r = self.version(tx)?;
        if r.is_some() {
            *self.rv_next = self.rv_next.wrapping_add(1);
        }
        Ok(r)
    }

    fn created_version(&self, row: RowId, rec: &[u8]) -> Result<(u32, u32)> {
        let at = self.rv_offset();
        match (u32le(rec, at), u32le(rec, at.saturating_add(4))) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => Err(Error::corrupt(self.record_loc(row), "record is too short")),
        }
    }

    fn rv_offset(&self) -> usize {
        let rv = self
            .descr()
            .fields
            .iter()
            .position(|f| f.ty == FieldType::RV);
        rv.and_then(|i| self.layout().slots.get(i))
            .map_or(1, |s| s.offset as usize)
    }

    fn set_versions(&self, row: &mut Row, words: [u32; 4]) {
        for (f, v) in self.descr().fields.iter().zip(row.iter_mut()) {
            if f.ty == FieldType::RV {
                *v = Value::RowVersion(words);
            }
        }
    }

    fn put_blob(&self, row: &mut Row, i: usize, r: BlobRef) {
        if let (Some(f), Some(v)) = (self.descr().fields.get(i), row.get_mut(i)) {
            *v = blob_value(f.ty, BlobVal::Ref(r));
        }
    }

    fn free_value(&mut self, tx: &mut TxnPages, r: BlobRef) -> Result<()> {
        if r.len == 0 {
            return Ok(());
        }
        let store = self.store()?;
        store.value_blocks(&*tx, r)?;
        store.free_chain(tx, r.start)
    }

    fn take_slot(&mut self, tx: &mut TxnPages) -> Result<RowId> {
        let rl = self.record_len();
        if self.files.data.length == 0 {
            self.files.data.set_len(tx, rl)?;
            let mut slot0 = self.layout().empty_template(self.descr());
            set_free_link(&mut slot0, 0);
            self.write_record(tx, 0, &slot0)?;
        }
        let n = self.num_slots();
        let head = self.free_head(&*tx)?;
        if head == 0 {
            let end = (u64::from(n).checked_add(1))
                .and_then(|s| s.checked_mul(rl))
                .or_corrupt(
                    || Location::Table(self.name().to_string()),
                    "too many slots",
                )?;
            self.files.data.set_len(tx, end)?;
            return Ok(n);
        }
        if head >= n {
            return Err(Error::corrupt(
                self.record_loc(0),
                format!("free record link {head} is beyond the {n} slots"),
            ));
        }
        let rec = self.read_record(&*tx, head)?;
        if is_used(&rec) {
            return Err(Error::corrupt(
                self.record_loc(head),
                format!("the head of the free records, slot {head}, is a live record"),
            ));
        }
        let next = free_next(&rec).or_corrupt(|| self.record_loc(head), "record is too short")?;
        self.set_free_head(tx, next)?;
        Ok(head)
    }

    fn apply_insert(&mut self, tx: &mut TxnPages, p: Prepared<'_>) -> Result<RowId> {
        let row = self.take_slot(tx)?;
        let mut out = p.row;
        for (i, plan) in p.blobs {
            if let BlobPlan::Store(b) = plan {
                let r = self.store()?.write_new_chain(tx, b)?;
                self.put_blob(&mut out, i, r);
            }
        }
        if let Some((v, k)) = self.next_version(tx)? {
            self.set_versions(&mut out, [v, k, v, k]);
        }
        let rec = self.encode(&out)?;
        self.write_record(tx, row, &rec)?;
        Ok(row)
    }

    fn apply_update(
        &mut self,
        tx: &mut TxnPages,
        row: RowId,
        cur: &Current,
        p: Prepared<'_>,
    ) -> Result<()> {
        let mut out = p.row;
        for (i, plan) in p.blobs {
            let old = cur.blob(i);
            match plan {
                BlobPlan::Store(b) => {
                    let r = self.store()?.rewrite_chain(tx, old.unwrap_or(NO_BLOB), b)?;
                    self.put_blob(&mut out, i, r);
                }
                BlobPlan::Null => {
                    if let Some(old) = old {
                        self.free_value(tx, old)?;
                    }
                }
                BlobPlan::Keep(_) => {}
            }
        }
        if let Some((v, k)) = self.next_version(tx)? {
            let (a, b) = self.created_version(row, &cur.rec)?;
            self.set_versions(&mut out, [a, b, v, k]);
        }
        let rec = self.encode(&out)?;
        self.write_record(tx, row, &rec)
    }

    fn apply_delete(&mut self, tx: &mut TxnPages, row: RowId, cur: &Current) -> Result<()> {
        let head = self.free_head(&*tx)?;
        if head == row {
            return Err(Error::corrupt(
                self.record_loc(0),
                format!("the head of the free records is row {row} being deleted, a live record"),
            ));
        }
        for &(_, old) in &cur.blobs {
            if let Some(old) = old {
                self.free_value(tx, old)?;
            }
        }
        let mut rec = self.layout().empty_template(self.descr());
        set_free_link(&mut rec, head);
        self.write_record(tx, row, &rec)?;
        self.set_free_head(tx, row)
    }

    fn index_at(&self, i: usize) -> IndexLoc<'t> {
        let index = (self.descr().indexes.get(i)).map_or("", |ix| ix.name.as_str());
        IndexLoc {
            header_page: self.files.index.as_ref().map_or(0, |f| f.header_page),
            names: Some((self.name(), index)),
        }
    }

    fn check_collator(&self) -> Result<()> {
        if has_string_keys(self.descr()) && self.keys.collate.is_none() {
            return Err(Error::Unsupported(Unsupported::NoCollator));
        }
        Ok(())
    }

    fn keys_of(&self, rec: &[u8], row: Option<RowId>) -> Result<Vec<Vec<u8>>> {
        let d = self.descr();
        let mut out = Vec::with_capacity(d.indexes.len());
        for (i, idx) in d.indexes.iter().enumerate() {
            let at = self.index_at(i);
            let key_len = index_key_len(d, idx).map_err(|e| match e {
                Error::Constraint(Constraint::BadDescr(why)) => at.corrupt(None, why),
                other => other,
            })?;
            let key = build_key(d, self.layout(), idx, rec, key_len, &self.keys);
            out.push(key.map_err(|e| match row {
                Some(row) => at_record(self.name(), row, e),
                None => at.adopt(None, e),
            })?);
        }
        Ok(out)
    }

    fn planned(
        &mut self,
        tx: &TxnPages,
        p: &Prepared<'_>,
        created: Option<(u32, u32)>,
    ) -> Result<Vec<u8>> {
        let mut row = p.row.clone();
        if let Some((v, k)) = self.version(tx)? {
            let (a, b) = created.unwrap_or((v, k));
            self.set_versions(&mut row, [a, b, v, k]);
        }
        self.encode(&row)
    }

    fn plan_insert(&mut self, tx: &TxnPages, p: &Prepared<'_>) -> Result<Vec<IndexStep>> {
        if self.descr().indexes.is_empty() {
            return Ok(Vec::new());
        }
        self.check_collator()?;
        let rec = self.planned(tx, p, None)?;
        let keys = self.keys_of(&rec, None)?.into_iter().enumerate();
        Ok(keys
            .map(|(i, k)| IndexStep {
                i,
                old: None,
                new: Some(k),
            })
            .collect())
    }

    fn plan_delete(&self, row: RowId, cur: &Current) -> Result<Vec<IndexStep>> {
        if self.descr().indexes.is_empty() {
            return Ok(Vec::new());
        }
        self.check_collator()?;
        let keys = self.keys_of(&cur.rec, Some(row))?.into_iter().enumerate();
        Ok(keys
            .map(|(i, k)| IndexStep {
                i,
                old: Some(k),
                new: None,
            })
            .collect())
    }

    fn plan_update(
        &mut self,
        tx: &TxnPages,
        row: RowId,
        cur: &Current,
        p: &Prepared<'_>,
    ) -> Result<Vec<IndexStep>> {
        if self.descr().indexes.is_empty() {
            return Ok(Vec::new());
        }
        self.check_collator()?;
        let created = match self.layout().has_rowversion {
            true => Some(self.created_version(row, &cur.rec)?),
            false => None,
        };
        let rec = self.planned(tx, p, created)?;
        let old = self.keys_of(&cur.rec, Some(row))?;
        let new = self.keys_of(&rec, None)?;
        Ok((old.into_iter().zip(new).enumerate())
            .filter(|(_, (o, n))| o != n)
            .map(|(i, (o, n))| IndexStep {
                i,
                old: Some(o),
                new: Some(n),
            })
            .collect())
    }

    fn writer<'s>(
        &'s mut self,
        tx: &'s mut TxnPages,
        i: usize,
        key_len: usize,
    ) -> Result<IndexWriter<'s>> {
        let (at, fi, n) = (self.index_at(i), self.fi, self.descr().indexes.len());
        let table = self.name();
        let head = self.files.heads.get(i).copied().flatten();
        let file = self.files.index.as_mut().ok_or_else(|| {
            Error::corrupt(
                Location::Table(table.to_string()),
                "table has no index file",
            )
        })?;
        let ix = match head {
            Some(head) => IndexWriter::resume(tx, file, fi, head, at.names)?,
            None => IndexWriter::open(tx, file, fi, n, i, at.names)?,
        };
        if ix.key_len() as usize != key_len {
            return Err(at.corrupt(
                Some(ix.descr_page()),
                format!(
                    "descriptor key length {} differs from the {key_len} of the description",
                    ix.key_len()
                ),
            ));
        }
        Ok(ix)
    }

    fn with_writer<T>(
        &mut self,
        tx: &mut TxnPages,
        i: usize,
        key_len: usize,
        op: impl FnOnce(&mut IndexWriter<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut ix = self.writer(tx, i, key_len)?;
        let out = op(&mut ix);
        let head = ix.head();
        if let Some(slot) = self.files.heads.get_mut(i) {
            *slot = Some(head);
        }
        out
    }

    fn check_steps(
        &mut self,
        tx: &mut TxnPages,
        steps: &[IndexStep],
        row: Option<RowId>,
    ) -> Result<()> {
        for s in steps {
            let (table, idx) = (self.name(), self.descr().indexes.get(s.i));
            let key_len = s.new.as_ref().or(s.old.as_ref()).map_or(0, Vec::len);
            let unique = s.new.as_ref().zip(idx.filter(|ix| ix.unique));
            let found = self.with_writer(tx, s.i, key_len, |ix| match unique {
                Some((key, _)) => ix.contains_key(key),
                None => Ok(None),
            })?;
            let Some((_, idx)) = unique else {
                continue;
            };
            if found.is_some_and(|r| Some(r) != row) {
                return Err(Error::Constraint(Constraint::UniqueViolation {
                    table: table.to_string(),
                    index: idx.name.clone(),
                }));
            }
        }
        Ok(())
    }

    fn remove_pairs(&mut self, tx: &mut TxnPages, steps: &[IndexStep], row: RowId) -> Result<()> {
        let mut written = false;
        for s in steps {
            let Some(key) = &s.old else {
                continue;
            };
            let at = self.index_at(s.i);
            let removed = self.with_writer(tx, s.i, key.len(), |ix| ix.delete(key, row));
            let e = match removed {
                Ok(true) => {
                    written = true;
                    continue;
                }
                Ok(false) => at.corrupt(
                    None,
                    format!("row {row} is not in the index under the key of its record"),
                ),
                Err(e) => e,
            };
            return match written {
                true => aborting(tx, e),
                false => Err(e),
            };
        }
        Ok(())
    }

    fn add_pairs(&mut self, tx: &mut TxnPages, steps: &[IndexStep], row: RowId) -> Result<()> {
        for s in steps {
            let Some(key) = &s.new else {
                continue;
            };
            self.with_writer(tx, s.i, key.len(), |ix| ix.insert(key, row))?;
        }
        Ok(())
    }

    fn apply_blob(
        &mut self,
        tx: &mut TxnPages,
        row: RowId,
        field: usize,
        chain: ChainWriter,
    ) -> Result<BlobRef> {
        let r = self.store()?.finish_chain(tx, chain)?;
        let mut rec = self.read_record(&*tx, row)?;
        if !is_used(&rec) {
            return Err(Error::corrupt(self.record_loc(row), "record is free"));
        }
        let old = self.blob_refs(row, &rec)?;
        if let Some(&(_, Some(old))) = old.iter().find(|(i, _)| *i == field) {
            self.free_value(tx, old)?;
        }
        let loc = self.record_loc(row);
        let short = || loc.clone();
        let (Some(f), Some(slot)) = (
            self.descr().fields.get(field),
            self.layout().slots.get(field),
        ) else {
            return Err(Error::corrupt(short(), format!("no field {field}")));
        };
        let mut at = slot.offset as usize;
        if f.nullable {
            *rec.get_mut(at).or_corrupt(short, "record is too short")? = 1;
            at = at.saturating_add(1);
        }
        put_u32le(&mut rec, at, r.start)
            .and_then(|()| put_u32le(&mut rec, at.saturating_add(4), r.len))
            .or_corrupt(short, "record is too short")?;
        if let Some((v, k)) = self.next_version(tx)? {
            let at = self.rv_offset();
            put_u32le(&mut rec, at.saturating_add(8), v)
                .and_then(|()| put_u32le(&mut rec, at.saturating_add(12), k))
                .or_corrupt(short, "record is too short")?;
        }
        self.write_record(tx, row, &rec)?;
        Ok(r)
    }
}

pub struct TableWrite<'t> {
    tx: &'t mut TxnPages,
    t: Target<'t>,
}

impl fmt::Debug for TableWrite<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TableWrite")
            .field("name", &self.t.name())
            .field("num_slots", &self.t.num_slots())
            .finish_non_exhaustive()
    }
}

impl<'t> TableWrite<'t> {
    pub(crate) fn new(
        tx: &'t mut TxnPages,
        fi: FormatInfo,
        entry: &'t CatalogEntry,
        files: &'t mut TableFiles,
        rv_next: &'t mut u32,
        keys: KeyCtx<'t>,
    ) -> TableWrite<'t> {
        TableWrite {
            tx,
            t: Target {
                fi,
                entry,
                files,
                rv_next,
                keys,
            },
        }
    }

    pub fn insert(&mut self, row: &Row) -> Result<RowId> {
        let _unwind = self.tx.abort_on_panic();
        self.settle()?;
        let p = self.t.prepare(row, true)?;
        let data = &self.t.files.data;
        let rl = self.t.record_len();
        let grow = if data.length == 0 { 2 * rl } else { rl };
        data.check_len(self.t.fi, data.length.saturating_add(grow))?;
        self.t.check_room(&p)?;
        let steps = self.t.plan_insert(self.tx, &p)?;
        self.t.check_steps(self.tx, &steps, None)?;
        let t = &mut self.t;
        self.tx.run(|tx| {
            let id = t.apply_insert(tx, p)?;
            t.add_pairs(tx, &steps, id)?;
            Ok(id)
        })
    }

    pub fn update(&mut self, row: RowId, new: &Row) -> Result<()> {
        let _unwind = self.tx.abort_on_panic();
        self.settle()?;
        let p = self.t.prepare(new, false)?;
        let cur = self.live(row)?;
        self.t.check_kept(&p, &cur)?;
        self.t.check_room(&p)?;
        let steps = self.t.plan_update(self.tx, row, &cur, &p)?;
        self.t.check_steps(self.tx, &steps, Some(row))?;
        let t = &mut self.t;
        t.remove_pairs(self.tx, &steps, row)?;
        self.tx.run(|tx| {
            t.apply_update(tx, row, &cur, p)?;
            t.add_pairs(tx, &steps, row)
        })
    }

    pub fn delete(&mut self, row: RowId) -> Result<()> {
        let _unwind = self.tx.abort_on_panic();
        self.settle()?;
        let cur = self.live(row)?;
        let steps = self.t.plan_delete(row, &cur)?;
        self.t.check_steps(self.tx, &steps, Some(row))?;
        let t = &mut self.t;
        t.remove_pairs(self.tx, &steps, row)?;
        self.tx.run(|tx| t.apply_delete(tx, row, &cur))
    }

    pub fn blob_writer(&mut self, row: RowId, field: &str) -> Result<BlobWriter<'_>> {
        let _unwind = self.tx.abort_on_panic();
        self.settle()?;
        let (i, f) = (self.t.descr().field(field)).ok_or_else(|| {
            Error::NotFound(NotFound::Field(crate::error::brief(field).to_owned()))
        })?;
        if !f.ty.is_blob() {
            return Err(Error::Constraint(Constraint::TypeMismatch {
                table: self.t.name().to_string(),
                field: f.name.clone(),
            }));
        }
        self.live(row)?;
        self.t.files.pending = Some(ChainWriter::default());
        Ok(BlobWriter {
            tx: &mut *self.tx,
            t: self.t.reborrow(),
            row,
            field: i,
        })
    }

    pub(crate) fn move_from(
        &mut self,
        old: &RowSource,
        convert: &mut dyn FnMut(&Row) -> Result<Row>,
    ) -> Result<()> {
        let mut rec = Vec::new();
        for slot in 1..old.files.num_slots {
            let Some(row) = old.row(&*self.tx, slot, &mut rec)? else {
                continue;
            };
            let mut new = convert(&row)?;
            self.copy_blobs(old, &row, &mut new)?;
            self.insert(&new)?;
        }
        Ok(())
    }

    fn copy_blobs(&self, old: &RowSource, row: &Row, new: &mut Row) -> Result<()> {
        let held = |r: BlobRef| {
            (old.entry.descr.fields.iter().zip(row))
                .any(|(f, v)| blob_of(f.ty, v) == Some(&BlobVal::Ref(r)))
        };
        for (f, v) in self.t.descr().fields.iter().zip(new.iter_mut()) {
            let Some(&BlobVal::Ref(r)) = blob_of(f.ty, v) else {
                continue;
            };
            if r.len != 0 && !held(r) {
                return Err(self.t.misfit(
                    f,
                    "a blob reference has to be a value of the row being moved",
                ));
            }
            let bytes = old.files.blob_bytes(&*self.tx, &old.entry, r)?;
            *v = blob_value(f.ty, BlobVal::Data(bytes));
        }
        Ok(())
    }

    fn settle(&mut self) -> Result<()> {
        self.tx.check_alive()?;
        self.t.files.settle(self.tx)
    }

    fn live(&self, row: RowId) -> Result<Current> {
        if row == 0 || row >= self.t.num_slots() {
            return Err(self.t.not_found(row));
        }
        let rec = self.t.read_record(&*self.tx, row)?;
        if !is_used(&rec) {
            return Err(self.t.not_found(row));
        }
        let blobs = self.t.blob_refs(row, &rec)?;
        Ok(Current { rec, blobs })
    }
}

pub struct BlobWriter<'a> {
    tx: &'a mut TxnPages,
    t: Target<'a>,
    row: RowId,
    field: usize,
}

impl fmt::Debug for BlobWriter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobWriter")
            .field("table", &self.t.name())
            .field("row", &self.row)
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl BlobWriter<'_> {
    pub fn finish(mut self) -> Result<BlobRef> {
        self.tx.check_alive()?;
        let _unwind = self.tx.abort_on_panic();
        let chain = self.t.files.pending.take().unwrap_or_default();
        let (t, row, field) = (&mut self.t, self.row, self.field);
        self.tx.run(|tx| t.apply_blob(tx, row, field, chain))
    }
}

impl io::Write for BlobWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _unwind = self.tx.abort_on_panic();
        let t = &mut self.t;
        let files = &mut *t.files;
        let r = match (files.blob.as_mut(), files.pending.as_mut()) {
            (Some(store), Some(chain)) => store.push(self.tx, chain, buf),
            (_, None) => return Err(io::Error::other("blob writer has no value")),
            (None, _) => Err(Error::corrupt(
                Location::Table(t.name().to_string()),
                "table has no blob file",
            )),
        };
        r.map_err(|e| io::Error::other(t.in_field(e, self.field)))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BlobWriter<'_> {
    fn drop(&mut self) {
        let _ = self.t.files.settle(self.tx);
    }
}
