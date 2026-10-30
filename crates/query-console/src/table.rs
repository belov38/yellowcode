use crate::cell::{cell, column_kind, row_bytes};
use crate::{Column, Console, Lang, MAX_BYTES};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ibschema::fields::FieldValue;
use ibschema::store::{Direction, IndexPosition, RowHandle, Scanned};
use query_ir::QTableId;
use query_ir::schema::StdRole;
use query_testhost::{ConditionKind, RecordField};
use serde::{Deserialize, Serialize};
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn generation() -> String {
    static OPEN: AtomicU64 = AtomicU64::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        time,
        OPEN.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug)]
pub enum TableReadError {
    Invalid(String),
    NotFound(String),
    Unsupported(String),
    Storage(String),
    IndexRejected {
        error: String,
        available_indexes: Vec<AvailableIndex>,
    },
    Memory(memory_budget::MemoryLimit),
}
impl std::fmt::Display for TableReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IndexRejected { error, .. } => f.write_str(error),
            Self::Memory(m) => f.write_str(m.public_text()),
            Self::Invalid(s) | Self::NotFound(s) | Self::Unsupported(s) | Self::Storage(s) => {
                f.write_str(s)
            }
        }
    }
}
impl std::error::Error for TableReadError {}

pub const FILTER_SCAN_ROWS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RowFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_mark: Option<bool>,
}

impl RowFilter {
    pub fn is_empty(&self) -> bool {
        self.posted.is_none() && self.deletion_mark.is_none()
    }

    pub fn normalized(&self) -> Option<RowFilter> {
        (!self.is_empty()).then_some(*self)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePage {
    pub schema_version: u32,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<crate::Cell>>,
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<TableAccess>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<RowFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned: Option<u64>,
    #[serde(skip)]
    pub(crate) _retained: Option<memory_budget::Reservation>,
    #[serde(skip)]
    pub(crate) cut: Vec<(usize, usize, usize)>,
}

struct PageCells<'a> {
    mode: CellMode<'a>,
    text_bytes: usize,
    max_text_bytes: Option<usize>,
    cut_len: Option<usize>,
    parts_only: bool,
    rows: usize,
    cut: Vec<(usize, usize, usize)>,
}

enum CellMode<'a> {
    Legacy {
        bytes: usize,
    },
    Budgeted {
        lease: memory_budget::Reservation,
        evictor: std::sync::Arc<onecd::Database>,
        base: &'a query_testhost::TestBase,
        sizes: crate::cell::RowSizes,
    },
}

enum Take {
    Row(Vec<crate::Cell>),
    Last(Vec<crate::Cell>),
    Full,
}

impl<'a> PageCells<'a> {
    fn new(
        console: &'a Console,
        columns: &[Column],
        options: PageOptions,
    ) -> Result<Self, TableReadError> {
        Ok(Self {
            mode: CellMode::new(console, columns)?,
            text_bytes: 0,
            max_text_bytes: options.max_text_bytes,
            cut_len: options.cut_len,
            parts_only: options.whole_bytes.is_some(),
            rows: 0,
            cut: Vec::new(),
        })
    }

    fn take(
        &mut self,
        console: &Console,
        values: &[query_ir::QValue],
        parts: &[(usize, u64)],
        first: bool,
    ) -> Result<Take, TableReadError> {
        let take = match (self.max_text_bytes, self.mode.take(console, values, first)?) {
            (Some(max), Take::Row(row)) => {
                self.text_bytes = row
                    .iter()
                    .fold(self.text_bytes, |n, c| n.saturating_add(c.t.len()));
                if self.text_bytes >= max {
                    Take::Last(row)
                } else {
                    Take::Row(row)
                }
            }
            (_, take) => take,
        };
        Ok(match take {
            Take::Row(row) => Take::Row(self.cut_row(row, values, parts)),
            Take::Last(row) => Take::Last(self.cut_row(row, values, parts)),
            Take::Full => Take::Full,
        })
    }

    fn cut_row(
        &mut self,
        mut row: Vec<crate::Cell>,
        values: &[query_ir::QValue],
        parts: &[(usize, u64)],
    ) -> Vec<crate::Cell> {
        let r = self.rows;
        self.rows += 1;
        let Some(cut_len) = self.cut_len else {
            return row;
        };
        for (c, len) in cut_cells(&mut row, values, parts, cut_len, self.parts_only) {
            self.cut.push((r, c, len));
        }
        row
    }

    fn finish(
        self,
    ) -> (
        Option<memory_budget::Reservation>,
        Vec<(usize, usize, usize)>,
    ) {
        (self.mode.retained(), self.cut)
    }
}

impl<'a> CellMode<'a> {
    fn new(console: &'a Console, columns: &[Column]) -> Result<Self, TableReadError> {
        let base = &*console.base;
        let Some(budget) = base.memory_budget() else {
            return Ok(Self::Legacy { bytes: 0 });
        };
        let evictor = base.evictor();
        let lease = budget
            .try_reserve_or_evict(
                memory_budget::Category::Result,
                columns_model(columns),
                &*evictor,
            )
            .map_err(TableReadError::Memory)?;
        Ok(Self::Budgeted {
            sizes: crate::cell::RowSizes::new(budget),
            lease,
            evictor,
            base,
        })
    }

    fn take(
        &mut self,
        console: &Console,
        values: &[query_ir::QValue],
        first: bool,
    ) -> Result<Take, TableReadError> {
        match self {
            Self::Legacy { bytes } => {
                let row: Vec<_> = values
                    .iter()
                    .map(|v| cell(v, &console.base, query_syntax::Lang::En))
                    .collect();
                let size = row_bytes(&row);
                if size > MAX_BYTES.saturating_sub(*bytes) {
                    if first {
                        return Err(TableReadError::Unsupported(
                            "one row exceeds the retained-cell memory budget".into(),
                        ));
                    }
                    return Ok(Take::Full);
                }
                *bytes += size;
                Ok(Take::Row(row))
            }
            Self::Budgeted {
                lease,
                evictor,
                base,
                sizes,
            } => {
                let refused = |e: memory_budget::BudgetError| match e {
                    memory_budget::BudgetError::Limit(m)
                        if m.reason == memory_budget::LimitReason::Category
                            && m.category == memory_budget::Category::Result
                            && !first =>
                    {
                        Ok(Take::Full)
                    }
                    memory_budget::BudgetError::Limit(m) => Err(TableReadError::Memory(m)),
                    other => Err(TableReadError::Storage(other.to_string())),
                };
                #[cfg(test)]
                if crate::cell::reference::per_cell() {
                    return per_cell_take(lease, &**evictor, base, values).or_else(refused);
                }
                let lang = query_syntax::Lang::En;
                let total = match sizes.measure(values, base, lang) {
                    Ok(total) => total,
                    Err(e) => return refused(e),
                };
                let mut row = Vec::new();
                if let Err(e) = row.try_reserve_exact(values.len()) {
                    return Err(TableReadError::Storage(e.to_string()));
                }
                if let Err(e) = lease.try_grow_or_evict(total, &**evictor) {
                    return refused(memory_budget::BudgetError::Limit(e));
                }
                if let Err(e) = crate::cell::row_write(
                    values,
                    sizes.get(),
                    base,
                    lang,
                    lease.budget(),
                    &mut row,
                ) {
                    drop(row);
                    let _ = lease.shrink(total);
                    return refused(e);
                }
                Ok(Take::Row(row))
            }
        }
    }

    fn retained(self) -> Option<memory_budget::Reservation> {
        match self {
            Self::Legacy { .. } => None,
            Self::Budgeted { lease, .. } => Some(lease),
        }
    }
}

pub(crate) fn cut_cells(
    row: &mut [crate::Cell],
    values: &[query_ir::QValue],
    parts: &[(usize, u64)],
    cut_len: usize,
    parts_only: bool,
) -> Vec<(usize, usize)> {
    let mut cut = Vec::new();
    for (c, cell) in row.iter_mut().enumerate() {
        let binary = match cell.k {
            crate::CellKind::String => false,
            crate::CellKind::Binary => true,
            _ => continue,
        };
        let stored = parts.iter().find(|p| p.0 == c).map(|p| p.1);
        if parts_only && stored.is_none() {
            continue;
        }
        let Some((at, _)) = cell.t.char_indices().nth(cut_len) else {
            continue;
        };
        let len = match (stored, values.get(c)) {
            (Some(bytes), _) if binary => bytes,
            (Some(bytes), _) => bytes / 2,
            (None, Some(query_ir::QValue::String(s))) => s.len_utf16() as u64,
            (None, Some(query_ir::QValue::Binary(b))) => b.len() as u64,
            (None, _) => cell.t.chars().count() as u64,
        };
        cell.t.truncate(at);
        cell.t.shrink_to_fit();
        cut.push((c, usize::try_from(len).unwrap_or(usize::MAX)));
    }
    cut
}

fn columns_model(columns: &[Column]) -> u64 {
    fn block(n: u64) -> u64 {
        n.div_ceil(16).saturating_mul(16).saturating_add(16)
    }
    let names: u64 = columns
        .iter()
        .map(|c| block(c.name.len() as u64).saturating_add(block(c.ty.len() as u64)))
        .sum();
    block((columns.len() as u64).saturating_mul(std::mem::size_of::<Column>() as u64))
        .saturating_add(names)
}

pub(crate) fn store_error(e: ibschema::store::StoreError) -> TableReadError {
    match e {
        ibschema::store::StoreError::Memory(m) => TableReadError::Memory(m),
        other => TableReadError::Storage(other.to_string()),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PageOptions {
    pub max_text_bytes: Option<usize>,
    pub cut_len: Option<usize>,
    pub whole_bytes: Option<usize>,
    pub ref_pairs: bool,
    pub filter: RowFilter,
}

impl PageOptions {
    pub(crate) fn prefix_bytes(self) -> Option<usize> {
        let cut_len = self.cut_len?;
        Some(
            self.whole_bytes
                .unwrap_or_else(|| cut_len.saturating_mul(PREFIX_BYTES_PER_CHAR)),
        )
    }
}

const PREFIX_BYTES_PER_CHAR: usize = 4;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FilterCursor {
    generation: String,
    table: u32,
    #[serde(default)]
    filter: RowFilter,
    row: u64,
}

const ANOTHER_FILTER: &str = "cursor belongs to another filter; restart the scan";
const ANOTHER_TABLE: &str =
    "cursor belongs to another table/opening or is invalid; restart the scan";

fn filter_cursor_decode(cursor: &str) -> Option<FilterCursor> {
    let text = cursor.strip_prefix("f1:")?;
    if text.len() > 8192 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn filter_fields(
    stored: &query_testhost::StoredTable<'_>,
    filter: &RowFilter,
) -> Result<Vec<(ibschema::storage::FieldId, bool)>, TableReadError> {
    let mut out = Vec::new();
    for (param, role, field, want) in [
        ("posted", StdRole::Posted, "Проведен", filter.posted),
        (
            "deletionMark",
            StdRole::DeletionMark,
            "ПометкаУдаления",
            filter.deletion_mark,
        ),
    ] {
        let Some(want) = want else {
            continue;
        };
        match stored.record_filter_field(role) {
            RecordField::Field(id) => out.push((id, want)),
            RecordField::Absent => {
                return Err(TableReadError::Unsupported(format!(
                    "{param}: the table has no {field} field"
                )));
            }
            RecordField::NotForEveryRow => {
                return Err(TableReadError::Unsupported(format!(
                    "{param}: the field applies only to items or folders; use /query"
                )));
            }
            RecordField::NotBoolean => {
                return Err(TableReadError::Unsupported(format!(
                    "{param}: the field is not a stored boolean"
                )));
            }
        }
    }
    Ok(out)
}

impl Console {
    pub fn read_table_page(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<TablePage, TableReadError> {
        self.read_table_page_with(kind, name, limit, cursor, PageOptions::default())
    }

    pub fn read_table_page_filtered(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
        filter: &RowFilter,
    ) -> Result<TablePage, TableReadError> {
        let options = PageOptions {
            filter: *filter,
            ..PageOptions::default()
        };
        self.read_table_page_with(kind, name, limit, cursor, options)
    }

    fn storage_cursor(
        &self,
        table: QTableId,
        cursor: Option<&str>,
        filter: Option<RowFilter>,
    ) -> Result<u64, TableReadError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        let prefix = format!("v1:{}:{}:", self.cursor_generation, table.0);
        if let Some(row) = cursor
            .strip_prefix(&prefix)
            .and_then(|s| s.parse::<u32>().ok())
        {
            if filter.is_some() {
                return Err(TableReadError::Invalid(ANOTHER_FILTER.into()));
            }
            return Ok(u64::from(row));
        }
        let c = filter_cursor_decode(cursor)
            .filter(|c| {
                c.generation == self.cursor_generation
                    && c.table == table.0
                    && c.row <= u64::from(u32::MAX)
            })
            .ok_or_else(|| TableReadError::Invalid(ANOTHER_TABLE.into()))?;
        if c.filter.normalized() != filter {
            return Err(TableReadError::Invalid(ANOTHER_FILTER.into()));
        }
        Ok(c.row)
    }

    pub(crate) fn read_table_page_with(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
        options: PageOptions,
    ) -> Result<TablePage, TableReadError> {
        use TableReadError::{Invalid, Unsupported};
        if !(1..=10_000).contains(&limit) {
            return Err(Invalid("limit must be between 1 and 10000".into()));
        }
        if [kind, name]
            .iter()
            .any(|s| s.is_empty() || s.trim() != *s || s.contains(['.', '/', '\0']))
        {
            return Err(Invalid(
                "kind and name must be separate nonempty metadata identifiers".into(),
            ));
        }
        let schema = self.base.schema();
        let table = self.main_table(kind, name)?;
        let filter = options.filter.normalized();
        let after = match filter {
            None => self.storage_cursor(table, cursor, None)?,
            Some(_) => 0,
        };
        let stored = self
            .base
            .stored_table(table)
            .map_err(store_error)?
            .ok_or_else(|| {
                Unsupported("only mapped, stored main object tables are supported".into())
            })?;
        if let Some(filter) = filter {
            let checks = filter_fields(&stored, &filter)?;
            let after = self.storage_cursor(table, cursor, Some(filter))?;
            return self.filtered_storage_page(
                schema, table, &stored, limit, after, filter, &checks, options,
            );
        }
        let columns = stored
            .fields
            .iter()
            .map(|f| Column {
                name: f.name_ru.clone(),
                ty: query_testkit::repr::type_repr(schema, &f.ty, Lang::En.syn()),
                width: 0,
                kind: column_kind(&f.ty),
            })
            .collect::<Vec<_>>();
        if let Some(prefix) = options.prefix_bytes() {
            stored.set_blob_prefix(Some(prefix));
        }
        let mut cells = PageCells::new(self, &columns, options)?;
        let mut rows = Vec::new();
        let mut last = after;
        let mut more = false;
        let mut stopped = false;
        for row in stored
            .scan_after_typed(after)
            .map_err(store_error)?
            .take(limit)
        {
            let (handle, values) = row.map_err(store_error)?;
            let parts = stored.take_blob_cuts();
            match cells.take(self, &values, &parts, rows.is_empty())? {
                Take::Row(row) => rows.push(row),
                Take::Last(row) => {
                    rows.push(row);
                    last = handle;
                    stopped = true;
                    break;
                }
                Take::Full => {
                    more = true;
                    break;
                }
            }
            last = handle;
        }
        if !more && (stopped || rows.len() == limit) {
            more = stored.next_used_after(last).map_err(store_error)?.is_some();
        }
        let (retained, cut) = cells.finish();
        let prefix = format!("v1:{}:{}:", self.cursor_generation, table.0);
        Ok(TablePage {
            access: None,
            schema_version: 1,
            columns,
            rows,
            next_cursor: more.then(|| format!("{prefix}{last}")),
            filter: None,
            scanned: None,
            _retained: retained,
            cut,
        })
    }

    #[allow(clippy::too_many_arguments, reason = "the parts of one page read")]
    fn filtered_storage_page(
        &self,
        schema: &query_ir::QuerySchema,
        table: QTableId,
        stored: &query_testhost::StoredTable<'_>,
        limit: usize,
        after: u64,
        filter: RowFilter,
        checks: &[(ibschema::storage::FieldId, bool)],
        options: PageOptions,
    ) -> Result<TablePage, TableReadError> {
        let columns = page_columns(schema, stored);
        if let Some(prefix) = options.prefix_bytes() {
            stored.set_blob_prefix(Some(prefix));
        }
        let mut cells = PageCells::new(self, &columns, options)?;
        let slots = stored.scan_end().map_err(store_error)?;
        let end = after
            .saturating_add(1)
            .saturating_add(self.filter_scan_rows)
            .min(slots);
        let mut rows = Vec::new();
        let mut stop = None;
        let mut full = false;
        for item in stored
            .scan_after_filtered(after, checks, end)
            .map_err(store_error)?
        {
            let (handle, values) = match item.map_err(store_error)? {
                Scanned::Skipped(_) => continue,
                Scanned::Row(handle, values) => (handle, values),
            };
            let parts = stored.take_blob_cuts();
            match cells.take(self, &values, &parts, rows.is_empty())? {
                Take::Row(row) => {
                    rows.push(row);
                    if rows.len() == limit {
                        stop = Some(handle);
                        break;
                    }
                }
                Take::Last(row) => {
                    rows.push(row);
                    stop = Some(handle);
                    break;
                }
                Take::Full => {
                    stop = Some(handle.saturating_sub(1));
                    full = true;
                    break;
                }
            }
        }
        let cursor = stop.unwrap_or(end.saturating_sub(1)).max(after);
        let more = full || cursor.saturating_add(1) < slots;
        let next_cursor = if more {
            let bytes = serde_json::to_vec(&FilterCursor {
                generation: self.cursor_generation.clone(),
                table: table.0,
                filter,
                row: cursor,
            })
            .map_err(|e| TableReadError::Storage(e.to_string()))?;
            Some(format!("f1:{}", URL_SAFE_NO_PAD.encode(bytes)))
        } else {
            None
        };
        let (retained, cut) = cells.finish();
        Ok(TablePage {
            access: None,
            schema_version: 1,
            columns,
            rows,
            next_cursor,
            filter: Some(filter),
            scanned: Some(cursor.saturating_sub(after)),
            _retained: retained,
            cut,
        })
    }
}

fn page_columns(
    schema: &query_ir::QuerySchema,
    stored: &query_testhost::StoredTable<'_>,
) -> Vec<Column> {
    stored
        .fields
        .iter()
        .map(|f| Column {
            name: f.name_ru.clone(),
            ty: query_testkit::repr::type_repr(schema, &f.ty, Lang::En.syn()),
            width: 0,
            kind: column_kind(&f.ty),
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct IndexField {
    pub name: String,
    pub conditions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct AvailableIndex {
    pub name: String,
    pub fields: Vec<IndexField>,
    #[serde(skip)]
    pub(crate) paid: Option<memory_budget::Reservation>,
}

const REASON_BYTES: usize = 128;
const NAME_BYTES: usize = 256;

pub(crate) fn brief(name: &str) -> &str {
    let mut end = name.len().min(NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or_default()
}

fn prepare(
    budget: Option<&memory_budget::MemoryBudget>,
    bytes: u64,
) -> Result<Option<memory_budget::Reservation>, TableReadError> {
    budget
        .map(|b| {
            b.try_reserve(memory_budget::Category::Execution, bytes)
                .map_err(TableReadError::Memory)
        })
        .transpose()
}

pub(crate) fn available_indexes(
    stored: &query_testhost::StoredTable<'_>,
    indexes: &[ibschema::store::IndexAccess],
    budget: Option<&memory_budget::MemoryBudget>,
) -> Result<Vec<AvailableIndex>, TableReadError> {
    let list = u64::try_from(
        indexes
            .len()
            .saturating_mul(std::mem::size_of::<AvailableIndex>()),
    )
    .unwrap_or(u64::MAX);
    let entry_bytes = |ix: &ibschema::store::IndexAccess| -> u64 {
        let fields = ix.fields.iter().fold(0usize, |sum, (f, _)| {
            let name = stored.index_field_name(*f).map_or(32, str::len);
            sum.saturating_add(std::mem::size_of::<IndexField>() + REASON_BYTES + name)
        });
        u64::try_from(ix.name.len().saturating_add(fields)).unwrap_or(u64::MAX)
    };
    let total = indexes
        .iter()
        .fold(list, |sum, ix| sum.saturating_add(entry_bytes(ix)));
    let mut paid = prepare(budget, total)?;
    let mut out = Vec::with_capacity(indexes.len());
    for ix in indexes {
        let mut fields = Vec::with_capacity(ix.fields.len());
        for (f, reason) in &ix.fields {
            let name = stored.index_field_name(*f);
            let reason = if name.is_none() {
                Some("field is not mapped to query metadata".into())
            } else {
                reason.clone()
            };
            fields.push(IndexField {
                name: name
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("storage#{}", f.0)),
                conditions: reason.is_none(),
                reason,
            });
        }
        out.push(AvailableIndex {
            name: ix.name.clone(),
            fields,
            paid: paid.take(),
        });
    }
    Ok(out)
}
#[derive(Serialize)]
pub struct TableAccess {
    pub index: String,
    pub fields: Vec<String>,
    pub order: IndexOrder,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexOrder {
    #[default]
    Asc,
    Desc,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IndexRange {
    pub gt: Option<String>,
    pub gte: Option<String>,
    pub lt: Option<String>,
    pub lte: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TableIndexRequest {
    pub index: String,
    pub eq: Vec<String>,
    pub range: IndexRange,
    #[serde(default)]
    pub order: IndexOrder,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexCursor {
    generation: String,
    table: u32,
    request: TableIndexRequest,
    key: String,
    row: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filter: Option<RowFilter>,
}
fn cursor_encode(cursor: &IndexCursor) -> Result<String, TableReadError> {
    let bytes = serde_json::to_vec(cursor).map_err(|e| TableReadError::Storage(e.to_string()))?;
    Ok(format!("i1:{}", URL_SAFE_NO_PAD.encode(bytes)))
}
fn parse_date_condition(text: &str) -> Option<onecd::DateTime> {
    let (s, quoted) = match text.strip_prefix('\'') {
        Some(rest) => (rest.strip_suffix('\'')?, true),
        None => (text, false),
    };
    let b = s.as_bytes();
    let with_time = match b.len() {
        19 => true,
        10 if !quoted => false,
        _ => return None,
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(at(4, b'-') && at(7, b'-')) {
        return None;
    }
    if with_time && !(at(10, b'T') && at(13, b':') && at(16, b':')) {
        return None;
    }
    let digits = |from: usize, to: usize| -> Option<u16> {
        b.get(from..to)?.iter().try_fold(0u16, |n, &d| {
            d.is_ascii_digit().then(|| n * 10 + u16::from(d - b'0'))
        })
    };
    let year = digits(0, 4)?;
    let small = |from: usize| digits(from, from + 2).and_then(|v| u8::try_from(v).ok());
    let (month, day) = (small(5)?, small(8)?);
    let (hour, minute, second) = if with_time {
        (small(11)?, small(14)?, small(17)?)
    } else {
        (0, 0, 0)
    };
    if year == 0 {
        return None;
    }
    onecd::DateTime::new(year, month, day, hour, minute, second)
}

fn cursor_decode(cursor: &str) -> Option<IndexCursor> {
    let text = cursor.strip_prefix("i1:")?;
    if text.len() > 8192 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl Console {
    pub fn read_index_page(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
        request: &TableIndexRequest,
    ) -> Result<TablePage, TableReadError> {
        self.read_index_page_with(kind, name, limit, cursor, request, PageOptions::default())
    }

    pub fn read_index_page_filtered(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
        request: &TableIndexRequest,
        filter: &RowFilter,
    ) -> Result<TablePage, TableReadError> {
        let options = PageOptions {
            filter: *filter,
            ..PageOptions::default()
        };
        self.read_index_page_with(kind, name, limit, cursor, request, options)
    }

    pub(crate) fn read_index_page_with(
        &self,
        kind: &str,
        name: &str,
        limit: usize,
        cursor: Option<&str>,
        request: &TableIndexRequest,
        options: PageOptions,
    ) -> Result<TablePage, TableReadError> {
        use TableReadError::Invalid;
        if !(1..=10_000).contains(&limit) {
            return Err(Invalid("limit must be between 1 and 10000".into()));
        }
        if [kind, name]
            .iter()
            .any(|s| s.is_empty() || s.trim() != *s || s.contains(['.', '/', '\0']))
        {
            return Err(Invalid(
                "kind and name must be separate metadata identifiers".into(),
            ));
        }
        let table = self.main_table(kind, name)?;
        self.read_index_page_of(table, limit, cursor, request, options)
    }

    pub(crate) fn main_table(&self, kind: &str, name: &str) -> Result<QTableId, TableReadError> {
        let schema = self.base.schema();
        let table = schema
            .find_table(&[kind, name])
            .ok_or_else(|| TableReadError::NotFound(format!("no such table: {kind}.{name}")))?;
        if !matches!(
            schema.table(table).kind,
            query_ir::schema::TableKind::Object(_)
        ) {
            return Err(TableReadError::Unsupported(
                "only mapped, stored main object tables are supported".into(),
            ));
        }
        Ok(table)
    }

    pub(crate) fn stored_of(
        &self,
        table: QTableId,
    ) -> Result<query_testhost::StoredTable<'_>, TableReadError> {
        self.base
            .stored_table(table)
            .map_err(store_error)?
            .ok_or_else(|| {
                TableReadError::Unsupported(
                    "only mapped, stored main object and tabular tables are supported".into(),
                )
            })
    }

    pub(crate) fn count_index_of(
        &self,
        table: QTableId,
        request: &TableIndexRequest,
    ) -> Result<u64, TableReadError> {
        let stored = self.stored_of(table)?;
        let keys = self.index_keys(&stored, request, false)?;
        stored
            .count_index_rows(keys.ix.id, keys.lo(), keys.hi())
            .map_err(store_error)
    }

    pub(crate) fn index_has_entry_of(
        &self,
        table: QTableId,
        request: &TableIndexRequest,
        options: PageOptions,
    ) -> Result<bool, TableReadError> {
        let stored = self.stored_of(table)?;
        let keys = self.index_keys(&stored, request, options.ref_pairs)?;
        let dir = match keys.normalized.order {
            IndexOrder::Asc => Direction::Forward,
            IndexOrder::Desc => Direction::Backward,
        };
        Ok(stored
            .next_index_position(keys.ix.id, keys.lo(), keys.hi(), dir, None)
            .map_err(store_error)?
            .is_some())
    }

    pub(crate) fn read_index_page_of(
        &self,
        table: QTableId,
        limit: usize,
        cursor: Option<&str>,
        request: &TableIndexRequest,
        options: PageOptions,
    ) -> Result<TablePage, TableReadError> {
        use TableReadError::Invalid;
        if !(1..=10_000).contains(&limit) {
            return Err(Invalid("limit must be between 1 and 10000".into()));
        }
        let schema = self.base.schema();
        let stored = self.stored_of(table)?;
        let reject = |error: String| self.rejected(&stored, error);
        let keys = self.index_keys(&stored, request, options.ref_pairs)?;
        let ix = &keys.ix;
        let _cursor = prepare(
            self.base.memory_budget(),
            u64::try_from(cursor.map_or(0, |c| c.len().saturating_mul(4))).unwrap_or(u64::MAX),
        )?;
        let (lo, hi) = (keys.lo(), keys.hi());
        let normalized = keys.normalized.clone();
        let filter = options.filter.normalized();
        let checks = match &filter {
            Some(filter) => filter_fields(&stored, filter)?,
            None => Vec::new(),
        };
        let after = cursor.map(|c| {
            let c = cursor_decode(c).filter(|c| c.generation == self.cursor_generation && c.table == table.0 && c.request == normalized && c.row <= u64::from(u32::MAX)
                        )
                .ok_or_else(|| Invalid("cursor belongs to another request/opening or is invalid; restart the scan".into()))?;
            if c.filter.and_then(|f| f.normalized()) != filter {
                return Err(Invalid(ANOTHER_FILTER.into()));
            }
            let key = URL_SAFE_NO_PAD.decode(&c.key).map_err(|_| Invalid("invalid cursor key".into()))?;
            if key.len() != ix.key_len { return Err(Invalid("invalid cursor key length".into())); }
            Ok(IndexPosition::new(key, RowHandle(c.row)))
        }).transpose()?;
        let dir = match normalized.order {
            IndexOrder::Asc => Direction::Forward,
            IndexOrder::Desc => Direction::Backward,
        };
        if let Some(prefix) = options.prefix_bytes() {
            stored.set_blob_prefix(Some(prefix));
        }
        let access = || -> Result<TableAccess, TableReadError> {
            let listed =
                available_indexes(&stored, std::slice::from_ref(ix), self.base.memory_budget())?;
            Ok(TableAccess {
                index: ix.name.clone(),
                fields: listed
                    .into_iter()
                    .next()
                    .map(|a| a.fields.into_iter().map(|f| f.name).collect())
                    .unwrap_or_default(),
                order: normalized.order,
            })
        };
        if let Some(filter) = filter {
            let iter = stored
                .index_rows_filtered(ix.id, lo, hi, dir, after.as_ref(), &checks)
                .map_err(|e| match e {
                    ibschema::store::StoreError::Unsupported(_)
                    | ibschema::store::StoreError::ValueDoesNotFit { .. }
                    | ibschema::store::StoreError::NullNotAllowed { .. } => {
                        match stored.next_index_position(ix.id, lo, hi, dir, after.as_ref()) {
                            Ok(_) => TableReadError::Unsupported(e.to_string()),
                            Err(_) => reject(e.to_string()),
                        }
                    }
                    other => store_error(other),
                })?;
            let columns = page_columns(schema, &stored);
            let mut cells = PageCells::new(self, &columns, options)?;
            let mut rows = Vec::new();
            let mut last = after;
            let mut consumed = 0u64;
            let mut more = None;
            let mut iter = iter;
            while consumed < self.filter_scan_rows {
                let Some(item) = iter.next() else {
                    more = Some(false);
                    break;
                };
                let (position, values) = match item.map_err(store_error)? {
                    Scanned::Skipped(position) => {
                        last = Some(position);
                        consumed += 1;
                        continue;
                    }
                    Scanned::Row(position, values) => (position, values),
                };
                let parts = stored.take_blob_cuts();
                match cells.take(self, &values, &parts, rows.is_empty())? {
                    Take::Row(row) => {
                        rows.push(row);
                        last = Some(position);
                        consumed += 1;
                        if rows.len() == limit {
                            break;
                        }
                    }
                    Take::Last(row) => {
                        rows.push(row);
                        last = Some(position);
                        consumed += 1;
                        break;
                    }
                    Take::Full => {
                        more = Some(true);
                        break;
                    }
                }
            }
            drop(iter);
            let more = match more {
                Some(more) => more,
                None => stored
                    .next_index_position(ix.id, lo, hi, dir, last.as_ref())
                    .map_err(store_error)?
                    .is_some(),
            };
            let next_cursor = match last.filter(|_| more) {
                Some(p) => Some(cursor_encode(&IndexCursor {
                    generation: self.cursor_generation.clone(),
                    table: table.0,
                    request: normalized.clone(),
                    key: URL_SAFE_NO_PAD.encode(&p.key),
                    row: p.row.0,
                    filter: Some(filter),
                })?),
                None => None,
            };
            let (retained, cut) = cells.finish();
            return Ok(TablePage {
                schema_version: 1,
                columns,
                rows,
                next_cursor,
                access: Some(access()?),
                filter: Some(filter),
                scanned: Some(consumed),
                _retained: retained,
                cut,
            });
        }
        let iter = stored
            .index_rows_typed(ix.id, lo, hi, dir, after.as_ref())
            .map_err(|e| match e {
                ibschema::store::StoreError::Unsupported(_)
                | ibschema::store::StoreError::ValueDoesNotFit { .. }
                | ibschema::store::StoreError::NullNotAllowed { .. } => reject(e.to_string()),
                other => store_error(other),
            })?;
        let columns = stored
            .fields
            .iter()
            .map(|f| Column {
                name: f.name_ru.clone(),
                ty: query_testkit::repr::type_repr(schema, &f.ty, Lang::En.syn()),
                width: 0,
                kind: column_kind(&f.ty),
            })
            .collect::<Vec<_>>();
        let mut cells = PageCells::new(self, &columns, options)?;
        let mut rows = Vec::new();
        let mut last = None;
        let mut more = false;
        let mut stopped = false;
        for item in iter.take(limit) {
            let (position, values) = item.map_err(store_error)?;
            let parts = stored.take_blob_cuts();
            match cells.take(self, &values, &parts, rows.is_empty())? {
                Take::Row(row) => rows.push(row),
                Take::Last(row) => {
                    rows.push(row);
                    last = Some(position);
                    stopped = true;
                    break;
                }
                Take::Full => {
                    more = true;
                    break;
                }
            }
            last = Some(position);
        }
        if !more && (stopped || rows.len() == limit) {
            more = stored
                .next_index_position(ix.id, lo, hi, dir, last.as_ref())
                .map_err(store_error)?
                .is_some();
        }
        let next_cursor = if more {
            last.map(|p| {
                cursor_encode(&IndexCursor {
                    generation: self.cursor_generation.clone(),
                    table: table.0,
                    request: normalized.clone(),
                    key: URL_SAFE_NO_PAD.encode(&p.key),
                    row: p.row.0,
                    filter: None,
                })
            })
            .transpose()?
        } else {
            None
        };
        let (retained, cut) = cells.finish();
        Ok(TablePage {
            schema_version: 1,
            columns,
            rows,
            next_cursor,
            _retained: retained,
            cut,
            access: Some(access()?),
            filter: None,
            scanned: None,
        })
    }
}

struct IndexKeys {
    ix: ibschema::store::IndexAccess,
    normalized: TableIndexRequest,
    eq: Vec<FieldValue>,
    lower: Option<Vec<FieldValue>>,
    upper: Option<Vec<FieldValue>>,
    _prepared: Option<memory_budget::Reservation>,
}

impl IndexKeys {
    fn lo(&self) -> Bound<&[FieldValue]> {
        match self.lower.as_deref() {
            Some(v) if self.normalized.range.gt.is_some() => Bound::Excluded(v),
            Some(v) => Bound::Included(v),
            None if self.eq.is_empty() => Bound::Unbounded,
            None => Bound::Included(self.eq.as_slice()),
        }
    }

    fn hi(&self) -> Bound<&[FieldValue]> {
        match self.upper.as_deref() {
            Some(v) if self.normalized.range.lt.is_some() => Bound::Excluded(v),
            Some(v) => Bound::Included(v),
            None if self.eq.is_empty() => Bound::Unbounded,
            None => Bound::Included(self.eq.as_slice()),
        }
    }
}

impl Console {
    fn rejected(&self, stored: &query_testhost::StoredTable<'_>, error: String) -> TableReadError {
        let listed = stored
            .indexes()
            .map_err(store_error)
            .and_then(|indexes| available_indexes(stored, &indexes, self.base.memory_budget()));
        match listed {
            Ok(available_indexes) => TableReadError::IndexRejected {
                error,
                available_indexes,
            },
            Err(e) => e,
        }
    }

    fn normalization_bytes(&self, request: &TableIndexRequest) -> u64 {
        let ref_name = self
            .base
            .ref_kinds_borrowed()
            .map(|(_, name)| name.len())
            .max()
            .unwrap_or(0);
        let range = &request.range;
        let ends = [&range.gt, &range.gte, &range.lt, &range.lte];
        let texts = request
            .eq
            .iter()
            .map(String::len)
            .chain(ends.iter().filter_map(|e| e.as_ref().map(String::len)));
        let conditions = request.eq.len() + ends.iter().filter(|e| e.is_some()).count();
        let per_text = |len: usize| {
            len.saturating_mul(8)
                .saturating_add(ref_name)
                .saturating_add(256 + std::mem::size_of::<String>() * 2)
        };
        let bytes = texts
            .fold(request.index.len().saturating_mul(2), |sum, len| {
                sum.saturating_add(per_text(len))
            })
            .saturating_add(
                conditions
                    .saturating_add(1)
                    .saturating_mul(3 * std::mem::size_of::<FieldValue>()),
            );
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    fn index_keys(
        &self,
        stored: &query_testhost::StoredTable<'_>,
        request: &TableIndexRequest,
        ref_pairs: bool,
    ) -> Result<IndexKeys, TableReadError> {
        use TableReadError::Invalid;
        if (request.range.gt.is_some() && request.range.gte.is_some())
            || (request.range.lt.is_some() && request.range.lte.is_some())
        {
            return Err(Invalid(
                "range accepts one lower and one upper bound".into(),
            ));
        }
        let prepared = prepare(self.base.memory_budget(), self.normalization_bytes(request))?;
        let reject = |error: String| self.rejected(stored, error);
        let access = stored
            .index_named(&request.index)
            .map_err(store_error)?
            .ok_or_else(|| reject(format!("no mapped index named {}", brief(&request.index))))?;
        let ix = &access;
        let range = &request.range;
        let has_range =
            range.gt.is_some() || range.gte.is_some() || range.lt.is_some() || range.lte.is_some();
        let bound_fields = request.eq.len() + usize::from(has_range);
        if bound_fields > ix.fields.len() {
            return Err(reject(
                "conditions must form an index prefix, with range on the next field".into(),
            ));
        }
        let pair = |f: &ibschema::storage::FieldId| ref_pairs && ix.ref_pairs.contains(f);
        for (f, reason) in ix.fields.iter().take(bound_fields) {
            if stored.index_field_name(*f).is_none() {
                return Err(reject(
                    "condition field has no query metadata mapping".into(),
                ));
            }
            if pair(f) {
                continue;
            }
            if let Some(reason) = reason {
                return Err(reject(reason.clone()));
            }
        }
        let mut normalized = request.clone();
        normalized.index = ix.name.clone();
        let condition = |k: usize, text: &mut String| -> Result<FieldValue, TableReadError> {
            if ix.fields.get(k).is_some_and(|(f, _)| pair(f)) {
                let (table, id) = self.base.parse_reference(text, u32::MAX).ok_or_else(|| {
                    reject("condition must be a reference <Type>Ref.<Name>(<uuid>)".into())
                })?;
                let name = self.base.ref_xml_name(table).ok_or_else(|| {
                    reject("condition must be a reference <Type>Ref.<Name>(<uuid>)".into())
                })?;
                *text = format!("{name}({})", query_ir::uuid_of(&id));
                return Ok(FieldValue::Ref {
                    table,
                    id: ibschema::values::RefId(id),
                });
            }
            let kind = ix
                .fields
                .get(k)
                .and_then(|(f, _)| stored.condition_kind(*f));
            match kind {
                Some(ConditionKind::Date) => {
                    let d = parse_date_condition(text).ok_or_else(|| {
                        reject("condition must be a date CLI representation".into())
                    })?;
                    *text = format!("'{}'", query_ir::convert::date_to_q(&d).to_xml_string());
                    Ok(FieldValue::Date(d))
                }
                Some(ConditionKind::String) => {
                    let value = text.trim_end_matches(' ').to_owned();
                    *text = value.to_lowercase();
                    Ok(FieldValue::String(value))
                }
                Some(ConditionKind::Ref { table }) => {
                    let name = self.base.ref_xml_name(table).ok_or_else(|| {
                        reject("reference type of the field has no metadata".into())
                    })?;
                    let (of, id) = self.base.parse_reference(text, table).ok_or_else(|| {
                        reject(format!(
                            "condition must be a reference {}(<uuid>) or a bare uuid",
                            brief(name)
                        ))
                    })?;
                    if of != table {
                        return Err(reject(format!(
                            "reference of another type; the field holds {}",
                            brief(name)
                        )));
                    }
                    *text = format!("{name}({})", query_ir::uuid_of(&id));
                    Ok(FieldValue::Ref {
                        table,
                        id: ibschema::values::RefId(id),
                    })
                }
                Some(ConditionKind::Number) | None => {
                    let n = ibschema::Numeric::parse(text).ok_or_else(|| {
                        reject("condition must be a numeric CLI representation".into())
                    })?;
                    *text = query_ir::convert::number_to_q(&n).to_xml_string();
                    Ok(FieldValue::Number(n))
                }
            }
        };
        let eq = normalized
            .eq
            .iter_mut()
            .enumerate()
            .map(|(k, text)| condition(k, text))
            .collect::<Result<Vec<_>, _>>()?;
        let parse_end =
            |text: &mut Option<String>| -> Result<Option<Vec<FieldValue>>, TableReadError> {
                text.as_mut()
                    .map(|s| {
                        let mut v = eq.clone();
                        v.push(condition(eq.len(), s)?);
                        Ok(v)
                    })
                    .transpose()
            };
        let lower = parse_end(if normalized.range.gt.is_some() {
            &mut normalized.range.gt
        } else {
            &mut normalized.range.gte
        })?;
        let upper = parse_end(if normalized.range.lt.is_some() {
            &mut normalized.range.lt
        } else {
            &mut normalized.range.lte
        })?;
        Ok(IndexKeys {
            ix: access,
            normalized,
            eq,
            lower,
            upper,
            _prepared: prepared,
        })
    }
}
