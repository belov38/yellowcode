use super::{CUT_LEN, condition_text, default_limit, portion};
use crate::cell::row_bytes;
use crate::table::{PageOptions, available_indexes, store_error};
use crate::{
    Cell, CellKind, Column, Console, ErrorOut, IndexOrder, IndexRange, MAX_BYTES, MAX_ROWS,
    TableIndexRequest, TablePage, TableReadError,
};
use query_ir::QTableId;
use query_ir::schema::TableKind;
use serde::Serialize;

pub const CARD_WHOLE_BYTES: usize = 1 << 20;

pub const VALUE_MAX_BYTES: usize = 16 << 20;

pub const VALUE_TOO_LARGE: &str = "значение больше 16 МиБ — просмотр недоступен";

pub const CARD_BYTES: usize = MAX_BYTES;

const REF_FIELD: &str = "Ссылка";

const LINE_FIELD: &str = "НомерСтроки";

const PROBE_ROWS: usize = 1000;

fn card_options() -> PageOptions {
    PageOptions {
        max_text_bytes: None,
        cut_len: Some(CUT_LEN),
        whole_bytes: Some(CARD_WHOLE_BYTES),
        ref_pairs: false,
        filter: crate::table::RowFilter::default(),
    }
}

fn probe_options() -> PageOptions {
    PageOptions {
        max_text_bytes: None,
        cut_len: Some(CUT_LEN),
        whole_bytes: None,
        ref_pairs: false,
        filter: crate::table::RowFilter::default(),
    }
}

fn whole_options(bytes: usize) -> PageOptions {
    PageOptions {
        max_text_bytes: None,
        cut_len: Some(CUT_LEN),
        whole_bytes: Some(bytes),
        ref_pairs: false,
        filter: crate::table::RowFilter::default(),
    }
}

fn bound_text(bytes: usize) -> String {
    const MIB: usize = 1 << 20;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} МиБ", bytes / MIB)
    } else {
        format!("{bytes} байт")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cut {
    pub row: usize,
    pub col: usize,
    pub length: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub path: String,
    pub reference: String,
    pub columns: Vec<Column>,
    pub values: Vec<Cell>,
    pub cut: Vec<Cut>,
    pub tabular: Vec<TabularPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values_note: Option<String>,
    #[serde(skip)]
    value_bytes: usize,
    #[serde(skip)]
    _retained: Option<memory_budget::Reservation>,
}

#[derive(Serialize)]
pub struct TabularPart {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    pub cut: Vec<Cut>,
    pub total: u64,
    pub truncated: bool,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip)]
    bytes: usize,
    #[serde(skip)]
    _retained: Vec<memory_budget::Reservation>,
}

impl TabularPart {
    #[doc(hidden)]
    pub fn retained_bytes(&self) -> u64 {
        self._retained
            .iter()
            .map(memory_budget::Reservation::bytes)
            .sum()
    }

    fn empty(name: String) -> Self {
        Self {
            name,
            columns: Vec::new(),
            rows: Vec::new(),
            cut: Vec::new(),
            total: 0,
            truncated: false,
            error: None,
            note: None,
            bytes: 0,
            _retained: Vec::new(),
        }
    }
}

impl Card {
    #[doc(hidden)]
    pub fn counted_bytes(&self) -> usize {
        self.tabular
            .iter()
            .fold(self.value_bytes, |n, p| n.saturating_add(p.bytes))
    }

    #[doc(hidden)]
    pub fn value_bytes(&self) -> usize {
        self.value_bytes
    }
}

#[derive(Debug)]
pub enum DataError {
    Invalid(String),
    NotFound(String),
    Query(ErrorOut),
    Table(TableReadError),
    TooLarge(String),
}

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) | Self::NotFound(s) | Self::TooLarge(s) => f.write_str(s),
            Self::Query(e) => f.write_str(&e.chain.join(": ")),
            Self::Table(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for DataError {}

fn stored_bytes(kind: CellKind, length: usize) -> usize {
    match kind {
        CellKind::String => length.saturating_mul(2),
        _ => length,
    }
}

#[derive(Clone, Copy, Debug)]
struct PartLimits {
    rows: usize,
    bytes: usize,
    budget: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Rows,
    Bytes,
    Memory,
}

#[derive(Clone, Debug)]
pub(super) struct KeyField {
    pub(super) name: String,
    pub(super) conditions: bool,
    pub(super) area: bool,
    pub(super) pair: bool,
}

#[derive(Clone, Debug)]
pub(super) struct KeyIndex {
    pub(super) name: String,
    pub(super) fields: Vec<KeyField>,
}

pub(super) struct KeyIndexes {
    pub(super) indexes: Vec<KeyIndex>,
    _paid: Option<memory_budget::Reservation>,
}

pub(super) fn key_indexes(
    stored: &query_testhost::StoredTable<'_>,
    indexes: &[ibschema::store::IndexAccess],
    budget: Option<&memory_budget::MemoryBudget>,
) -> Result<KeyIndexes, TableReadError> {
    let mut listed = available_indexes(stored, indexes, budget)?;
    let paid = listed.first_mut().and_then(|first| first.paid.take());
    let keyed = indexes
        .iter()
        .zip(listed)
        .map(|(ix, listed)| KeyIndex {
            name: listed.name,
            fields: ix
                .fields
                .iter()
                .zip(listed.fields)
                .map(|((id, _), f)| KeyField {
                    name: f.name,
                    conditions: f.conditions,
                    area: stored.is_separator(*id),
                    pair: ix.ref_pairs.contains(id),
                })
                .collect(),
        })
        .collect();
    Ok(KeyIndexes {
        indexes: keyed,
        _paid: paid,
    })
}

pub(super) fn area_prefix(index: &KeyIndex, key: &str) -> Option<(usize, Vec<String>)> {
    let at = index.fields.iter().position(|f| f.name == key)?;
    let leading = index.fields.get(..at)?;
    leading
        .iter()
        .all(|f| f.area && f.conditions)
        .then(|| (at, leading.iter().map(|f| f.name.clone()).collect()))
}

pub(super) fn choose_ref_index(indexes: &[KeyIndex]) -> Option<(String, Vec<String>)> {
    indexes
        .iter()
        .filter_map(|ix| {
            let (at, leading) = area_prefix(ix, REF_FIELD)?;
            ix.fields
                .get(at)?
                .conditions
                .then(|| (at, ix.fields.len(), ix.name.clone(), leading))
        })
        .min_by_key(|(at, len, _, _)| (*at, *len))
        .map(|(_, _, index, leading)| (index, leading))
}

pub(super) struct RefIndex {
    pub(super) table: QTableId,
    pub(super) index: String,
    pub(super) leading: Vec<String>,
}

impl RefIndex {
    pub(super) fn request(&self, eq: Vec<String>, gt: Option<String>) -> TableIndexRequest {
        TableIndexRequest {
            index: self.index.clone(),
            eq,
            range: IndexRange {
                gt,
                ..IndexRange::default()
            },
            order: IndexOrder::Asc,
        }
    }
}

pub(super) struct ObjectRow {
    pub(super) page: TablePage,
    pub(super) index: RefIndex,
    pub(super) at: Vec<String>,
    pub(super) areas: usize,
}

struct Walk {
    found: Option<(TablePage, Vec<String>)>,
    areas: usize,
    others: bool,
}

impl Walk {
    fn done(&self) -> bool {
        self.found.is_some() && (!self.others || self.areas > 1)
    }
}

pub(super) fn values_of(
    names: &[String],
    columns: &[Column],
    values: &[Cell],
) -> Result<Vec<String>, TableReadError> {
    names
        .iter()
        .map(|name| {
            columns
                .iter()
                .position(|c| c.name == *name)
                .and_then(|c| values.get(c))
                .and_then(condition_text)
                .ok_or_else(|| {
                    TableReadError::Unsupported(format!(
                        "{}: the object has no condition value of this field",
                        crate::table::brief(name)
                    ))
                })
        })
        .collect()
}

fn order_by_line(part: &mut TabularPart) {
    let Some(col) = part.columns.iter().position(|c| c.name == LINE_FIELD) else {
        return;
    };
    let key = |row: &[Cell]| row.get(col).and_then(|c| c.t.parse::<u64>().ok());
    if part.rows.is_sorted_by_key(|r| key(r)) {
        return;
    }
    let mut keyed: Vec<(Option<u64>, usize, Vec<Cell>)> = std::mem::take(&mut part.rows)
        .into_iter()
        .enumerate()
        .map(|(i, r)| (key(&r), i, r))
        .collect();
    keyed.sort_by_key(|(k, i, _)| (*k, *i));
    let mut moved = vec![0; keyed.len()];
    for (new, (_, old, _)) in keyed.iter().enumerate() {
        if let Some(m) = moved.get_mut(*old) {
            *m = new;
        }
    }
    part.rows = keyed.into_iter().map(|(_, _, r)| r).collect();
    for c in &mut part.cut {
        if let Some(&new) = moved.get(c.row) {
            c.row = new;
        }
    }
    part.cut.sort_by_key(|c| (c.row, c.col));
}

fn whole_cell(
    page: TablePage,
    row: usize,
    col: usize,
    reread: impl FnOnce(PageOptions) -> Result<TablePage, TableReadError>,
) -> Result<Option<Cell>, DataError> {
    let cut = page
        .cut
        .iter()
        .find(|&&(r, c, _)| (r, c) == (row, col))
        .map(|&(_, _, length)| length);
    let Some(length) = cut else {
        return Ok(page
            .rows
            .into_iter()
            .nth(row)
            .and_then(|r| r.into_iter().nth(col)));
    };
    let kind = page.columns.get(col).map_or(CellKind::Binary, |c| c.kind);
    let stored = stored_bytes(kind, length);
    if stored > VALUE_MAX_BYTES {
        return Err(DataError::TooLarge(VALUE_TOO_LARGE.into()));
    }
    drop(page);
    let page = reread(whole_options(stored)).map_err(DataError::Table)?;
    if page.cut.iter().any(|&(r, c, _)| (r, c) == (0, col)) {
        return Err(DataError::Table(TableReadError::Storage(
            "value still cut after the whole read".into(),
        )));
    }
    Ok(page
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().nth(col)))
}

impl Console {
    pub(super) fn card_ref(&self, reference: &str) -> Result<(QTableId, String), DataError> {
        let bad = || {
            DataError::Invalid(format!(
                "reference: expected <Type>Ref.<Name>(<uuid>), got {reference:?}"
            ))
        };
        let (head, rest) = reference.split_once('(').ok_or_else(bad)?;
        if !rest.ends_with(')') {
            return Err(bad());
        }
        let schema = self.base.schema();
        let atom = schema
            .ref_by_name(head)
            .ok_or_else(|| DataError::Invalid(format!("no reference type {head}")))?;
        let (_, id) = self
            .base
            .parse_reference(reference, u32::MAX)
            .ok_or_else(bad)?;
        let info = schema.ref_info(atom);
        Ok((
            info.table,
            format!("{}({})", info.xml_name, query_ir::uuid_of(&id)),
        ))
    }

    fn ref_index(&self, table: QTableId) -> Result<RefIndex, TableReadError> {
        let schema = self.base.schema();
        let stored = self
            .base
            .stored_table(table)
            .map_err(store_error)?
            .ok_or_else(|| {
                TableReadError::Unsupported(format!(
                    "{}: not a stored table",
                    schema.table(table).name_en
                ))
            })?;
        let indexes = stored.indexes().map_err(store_error)?;
        choose_ref_index(&key_indexes(&stored, &indexes, self.base.memory_budget())?.indexes)
            .map(|(index, leading)| RefIndex {
                table,
                index,
                leading,
            })
            .ok_or_else(|| {
                TableReadError::Unsupported(format!(
                    "{}: no index by {REF_FIELD}",
                    schema.table(table).name_en
                ))
            })
    }

    pub fn object_card(&self, reference: &str) -> Result<Option<Card>, DataError> {
        self.object_card_with_bytes(reference, CARD_BYTES)
    }

    #[doc(hidden)]
    pub fn object_card_with_bytes(
        &self,
        reference: &str,
        bytes: usize,
    ) -> Result<Option<Card>, DataError> {
        let (main, reference) = self.card_ref(reference)?;
        let Some(found) = self.object_row(main, &reference, card_options(), true)? else {
            return Ok(None);
        };
        let ObjectRow {
            mut page,
            index,
            at,
            areas,
        } = found;
        let mut values_note = None;
        if page.rows.first().map_or(0, |r| row_bytes(r)) > bytes {
            let mut point = at.clone();
            point.push(reference.clone());
            let request = index.request(point, None);
            drop(page);
            page = self
                .read_index_page_of(main, 1, None, &request, probe_options())
                .map_err(DataError::Table)?;
            if !page.cut.is_empty() {
                values_note = Some(format!(
                    "Значения длиннее {CUT_LEN} символов показаны началом: поля не уместились в предел ответа \
                     карточки ({}); полное значение — «Показать полностью»",
                    bound_text(bytes)
                ));
            }
        }
        let TablePage {
            columns,
            rows,
            cut,
            _retained,
            ..
        } = page;
        let Some(values) = rows.into_iter().next() else {
            return Ok(None);
        };
        let schema = self.base.schema();
        let cut = cut
            .iter()
            .map(|&(row, col, length)| Cut { row, col, length })
            .collect();
        let value_bytes = row_bytes(&values);
        let mut left = bytes.saturating_sub(value_bytes);
        let mut tabular = Vec::new();
        for (i, t) in schema.tables().iter().enumerate() {
            if !matches!(t.kind, TableKind::Tabular) || t.owner != Some(main) {
                continue;
            }
            let Ok(i) = u32::try_from(i) else { continue };
            let name = t.name_ru.rsplit('.').next().unwrap_or_default().to_owned();
            let limits = PartLimits {
                rows: MAX_ROWS,
                bytes: left,
                budget: bytes,
            };
            let part = self.card_tabular(QTableId(i), name, &columns, &values, &reference, limits);
            left = left.saturating_sub(part.bytes);
            tabular.push(part);
        }
        let note = (areas > 1).then(|| {
            let shown: Vec<String> = index
                .leading
                .iter()
                .zip(&at)
                .map(|(name, value)| format!("{name} = {value}"))
                .collect();
            format!(
                "Ссылка есть и в других областях данных; показан объект первой из них ({})",
                shown.join(", ")
            )
        });
        Ok(Some(Card {
            path: schema.table(main).name_en.clone(),
            reference,
            columns,
            values,
            cut,
            tabular,
            note,
            values_note,
            value_bytes,
            _retained,
        }))
    }

    pub(super) fn object_row(
        &self,
        main: QTableId,
        reference: &str,
        options: PageOptions,
        others: bool,
    ) -> Result<Option<ObjectRow>, DataError> {
        let index = self.ref_index(main).map_err(DataError::Table)?;
        let mut walk = Walk {
            found: None,
            areas: 0,
            others,
        };
        self.object_row_under(&index, &mut Vec::new(), reference, options, &mut walk)
            .map_err(DataError::Table)?;
        Ok(walk.found.map(|(page, at)| ObjectRow {
            page,
            index,
            at,
            areas: walk.areas,
        }))
    }

    fn object_row_under(
        &self,
        index: &RefIndex,
        eq: &mut Vec<String>,
        reference: &str,
        options: PageOptions,
        walk: &mut Walk,
    ) -> Result<(), TableReadError> {
        let Some(field) = index.leading.get(eq.len()) else {
            let mut point = eq.clone();
            point.push(reference.to_owned());
            let request = index.request(point, None);
            if walk.found.is_none() {
                let page = self.read_index_page_of(index.table, 1, None, &request, options)?;
                if !page.rows.is_empty() {
                    walk.areas += 1;
                    walk.found = Some((page, eq.clone()));
                }
            } else if self.count_index_of(index.table, &request)? > 0 {
                walk.areas += 1;
            }
            return Ok(());
        };
        let mut after: Option<String> = None;
        loop {
            let next = self.read_index_page_of(
                index.table,
                1,
                None,
                &index.request(eq.clone(), after.take()),
                portion(),
            )?;
            let Some(row) = next.rows.first() else {
                return Ok(());
            };
            let value = next
                .columns
                .iter()
                .position(|c| c.name == *field)
                .and_then(|c| row.get(c))
                .and_then(condition_text)
                .ok_or_else(|| {
                    TableReadError::Unsupported(format!(
                        "{field}: an index entry has no condition value"
                    ))
                })?;
            eq.push(value);
            let walked = self.object_row_under(index, eq, reference, options, walk);
            let value = eq.pop();
            walked?;
            if walk.done() {
                return Ok(());
            }
            after = value;
        }
    }

    fn line_request(
        &self,
        t: QTableId,
        columns: &[Column],
        values: &[Cell],
        reference: &str,
    ) -> Result<TableIndexRequest, TableReadError> {
        let index = self.ref_index(t)?;
        let mut eq = values_of(&index.leading, columns, values)?;
        eq.push(reference.to_owned());
        Ok(index.request(eq, None))
    }

    fn card_tabular(
        &self,
        t: QTableId,
        name: String,
        columns: &[Column],
        values: &[Cell],
        reference: &str,
        limits: PartLimits,
    ) -> TabularPart {
        let mut part = TabularPart::empty(name);
        if let Err(e) = self.read_part(&mut part, t, columns, values, reference, limits) {
            let why = match &e {
                TableReadError::Memory(_) => format!("не хватило памяти ({e})"),
                _ => e.to_string(),
            };
            part = TabularPart::empty(std::mem::take(&mut part.name));
            part.error = Some(format!("Табличная часть не прочитана: {why}"));
        }
        part
    }

    fn read_part(
        &self,
        part: &mut TabularPart,
        t: QTableId,
        columns: &[Column],
        values: &[Cell],
        reference: &str,
        limits: PartLimits,
    ) -> Result<(), TableReadError> {
        let request = self.line_request(t, columns, values, reference)?;
        let portion = default_limit(self.base.schema().table(t).fields.len());
        let mut cursor: Option<String> = None;
        let mut stop = None;
        loop {
            let room = limits.rows.saturating_sub(part.rows.len());
            if room == 0 {
                stop = Some(Stop::Rows);
                break;
            }
            let left = limits.bytes.saturating_sub(part.bytes);
            if left == 0 {
                stop = Some(Stop::Bytes);
                break;
            }
            let options = PageOptions {
                max_text_bytes: Some(left),
                ..card_options()
            };
            let page = match self.read_index_page_of(
                t,
                portion.min(room),
                cursor.as_deref(),
                &request,
                options,
            ) {
                Ok(page) => page,
                Err(TableReadError::Memory(_)) if !part.rows.is_empty() => {
                    stop = Some(Stop::Memory);
                    break;
                }
                Err(e) => return Err(e),
            };
            let TablePage {
                columns,
                rows,
                next_cursor,
                _retained,
                cut,
                ..
            } = page;
            if part.columns.is_empty() {
                part.columns = columns;
            }
            let offset = part.rows.len();
            let mut keep = rows.len();
            for (i, row) in rows.iter().enumerate() {
                let size = row_bytes(row);
                if part.bytes.saturating_add(size) > limits.bytes {
                    keep = i;
                    break;
                }
                part.bytes = part.bytes.saturating_add(size);
            }
            part.cut.extend(
                cut.iter()
                    .filter(|c| c.0 < keep)
                    .map(|&(row, col, length)| Cut {
                        row: offset + row,
                        col,
                        length,
                    }),
            );
            let whole = keep == rows.len();
            part.rows.extend(rows.into_iter().take(keep));
            part._retained.extend(_retained);
            if !whole {
                stop = Some(Stop::Bytes);
                break;
            }
            match next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let kept = part.rows.len() as u64;
        part.truncated = stop.is_some();
        part.total = if part.truncated {
            self.count_index_of(t, &request).unwrap_or(kept).max(kept)
        } else {
            kept
        };
        if let Some(stop) = stop {
            let why = match stop {
                Stop::Rows => format!("предел {} строк табличной части", limits.rows),
                Stop::Bytes => format!(
                    "остальные не уместились в предел ответа карточки ({})",
                    bound_text(limits.budget)
                ),
                Stop::Memory => "не хватило памяти".to_owned(),
            };
            part.note = Some(if kept == 0 {
                format!("Строки не показаны (всего {}): {why}", part.total)
            } else {
                format!("Показаны первые {kept} строк из {}: {why}", part.total)
            });
        }
        order_by_line(part);
        Ok(())
    }

    pub fn card_value(
        &self,
        reference: &str,
        field: &str,
        line: Option<(&str, u32)>,
    ) -> Result<Option<Cell>, DataError> {
        let (main, reference) = self.card_ref(reference)?;
        let schema = self.base.schema();
        let table = match line {
            None => main,
            Some((part, _)) => {
                let owner = &schema.table(main).name_ru;
                schema
                    .find_table(&[owner, part])
                    .filter(|&t| {
                        !part.contains('.')
                            && matches!(schema.table(t).kind, TableKind::Tabular)
                            && schema.table(t).owner == Some(main)
                    })
                    .ok_or_else(|| {
                        DataError::Invalid(format!("no tabular part {part} in {owner}"))
                    })?
            }
        };
        let no_field = || {
            DataError::Invalid(format!(
                "no field {field} in {}",
                schema.table(table).name_ru
            ))
        };
        let f = schema
            .find_field(table, field)
            .and_then(|f| schema.field(table, f))
            .filter(|f| f.nested.is_none())
            .ok_or_else(no_field)?;
        let Some(found) = self.object_row(main, &reference, probe_options(), false)? else {
            return Ok(None);
        };
        let Some((_, n)) = line else {
            let col = found
                .page
                .columns
                .iter()
                .position(|c| c.name == f.name_ru)
                .ok_or_else(no_field)?;
            let mut point = found.at.clone();
            point.push(reference.clone());
            let request = found.index.request(point, None);
            return whole_cell(found.page, 0, col, |options| {
                self.read_index_page_of(main, 1, None, &request, options)
            });
        };
        let request = {
            let values = found
                .page
                .rows
                .first()
                .map(Vec::as_slice)
                .unwrap_or_default();
            self.line_request(table, &found.page.columns, values, &reference)
                .map_err(DataError::Table)?
        };
        drop(found);
        let n = n.to_string();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .read_index_page_of(
                    table,
                    PROBE_ROWS,
                    cursor.as_deref(),
                    &request,
                    probe_options(),
                )
                .map_err(DataError::Table)?;
            let at = |name: &str| page.columns.iter().position(|c| c.name == name);
            let (Some(line_col), Some(col)) = (at(LINE_FIELD), at(&f.name_ru)) else {
                return Err(no_field());
            };
            let found = page
                .rows
                .iter()
                .position(|r| r.get(line_col).is_some_and(|c| c.t == n));
            if let Some(i) = found {
                return whole_cell(page, i, col, |options| {
                    let before = match i {
                        0 => cursor.clone(),
                        _ => Some(
                            self.read_index_page_of(
                                table,
                                i,
                                cursor.as_deref(),
                                &request,
                                probe_options(),
                            )?
                            .next_cursor
                            .ok_or_else(|| {
                                TableReadError::Storage("the line is no longer there".into())
                            })?,
                        ),
                    };
                    self.read_index_page_of(table, 1, before.as_deref(), &request, options)
                });
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }
}
