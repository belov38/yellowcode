mod card;
mod movements;

pub use card::{
    CARD_BYTES, CARD_WHOLE_BYTES, Card, Cut, DataError, TabularPart, VALUE_MAX_BYTES,
    VALUE_TOO_LARGE,
};
pub use movements::{DocumentRegister, DocumentRegisters, MovementsKnob, MovementsPage, NO_INDEX};

use crate::table::{AvailableIndex, PageOptions, available_indexes};
use crate::{Cell, CellKind, Console, IndexOrder, IndexRange, TableIndexRequest, TablePage};
use crate::{TableReadError, TableReadError::Invalid};
use query_ir::QTableId;
use query_ir::schema::TableKind;
use query_testhost::{ConditionKind, StoredTable};
use serde::Serialize;

pub const DEFAULT_INDEXES: [&str; 9] = [
    "ByDocNum",
    "ByDocNumPrefix",
    "Code",
    "ByCode",
    "ByPeriod",
    "ByDocDate",
    "Descr",
    "ByDescr",
    "ByDims",
];

pub const PERIOD_INDEXES: [&str; 2] = ["ByDocDate", "ByPeriod"];

pub const PORTION_CELLS: usize = 50_000;

pub const PORTION_TEXT_BYTES: usize = 8 << 20;

pub const CUT_LEN: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DataTable {
    pub kind: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DroppedTable {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub(crate) struct DataTableList {
    tables: Vec<DataTable>,
    dropped: Vec<DroppedTable>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PeriodAccess {
    pub index: String,
    pub field: String,
    pub prefix: Vec<String>,
    pub several: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataPageRequest {
    pub index: Option<String>,
    pub eq: Vec<String>,
    pub range: IndexRange,
    pub order: IndexOrder,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub filter: crate::table::RowFilter,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPage {
    pub page: TablePage,
    pub index: Option<String>,
    pub limit: usize,
    pub order: IndexOrder,
    pub cut: Vec<(usize, usize, usize)>,
}

#[derive(Debug, Serialize)]
pub struct DataIndexes {
    pub indexes: Vec<AvailableIndex>,
    pub default: Option<String>,
    pub limit: usize,
    pub filters: DataFilters,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataFilters {
    pub posted: bool,
    pub deletion_mark: bool,
}

pub fn default_limit(columns: usize) -> usize {
    PORTION_CELLS
        .checked_div(columns)
        .unwrap_or(usize::MAX)
        .clamp(100, 1000)
}

fn portion() -> PageOptions {
    filtered_portion(crate::table::RowFilter::default())
}

fn filtered_portion(filter: crate::table::RowFilter) -> PageOptions {
    PageOptions {
        max_text_bytes: Some(PORTION_TEXT_BYTES),
        cut_len: Some(CUT_LEN),
        whole_bytes: None,
        ref_pairs: false,
        filter,
    }
}

fn pick_default(indexes: &[AvailableIndex]) -> Option<String> {
    DEFAULT_INDEXES.iter().find_map(|want| {
        indexes
            .iter()
            .find(|ix| ix.name.eq_ignore_ascii_case(want))
            .map(|ix| ix.name.clone())
    })
}

fn condition_text(cell: &Cell) -> Option<String> {
    match cell.k {
        CellKind::Number | CellKind::Date => Some(cell.t.clone()),
        CellKind::String => {
            let inner = cell.t.strip_prefix('"')?.strip_suffix('"')?;
            Some(inner.replace("\"\"", "\""))
        }
        CellKind::Ref => cell.r.as_ref().map(|r| format!("{}({})", r.kind, r.id)),
        _ => None,
    }
}

impl Console {
    fn data_stored(&self, kind: &str, name: &str) -> Result<StoredTable<'_>, TableReadError> {
        if [kind, name]
            .iter()
            .any(|s| s.is_empty() || s.trim() != *s || s.contains(['.', '/', '\0']))
        {
            return Err(Invalid(
                "kind and name must be separate metadata identifiers".into(),
            ));
        }
        let table = self.main_table(kind, name)?;
        self.base
            .stored_table(table)
            .map_err(TableReadError::Storage)?
            .ok_or_else(|| {
                TableReadError::Unsupported(
                    "only mapped, stored main object tables are supported".into(),
                )
            })
    }

    pub fn data_tables(&self) -> &[DataTable] {
        &self.data_table_list().tables
    }

    pub fn data_tables_dropped(&self) -> &[DroppedTable] {
        &self.data_table_list().dropped
    }

    fn data_table_list(&self) -> &DataTableList {
        self.tables.get_or_init(|| self.build_data_tables())
    }

    #[doc(hidden)]
    pub fn data_tables_cached(&self) -> Option<&[DataTable]> {
        self.tables.get().map(|l| l.tables.as_slice())
    }

    #[doc(hidden)]
    pub fn data_table_builds(&self) -> usize {
        self.table_builds.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn build_data_tables(&self) -> DataTableList {
        self.table_builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let schema = self.base.schema();
        let mut list = DataTableList::default();
        for (i, t) in schema.tables().iter().enumerate() {
            if !matches!(t.kind, TableKind::Object(_)) {
                continue;
            }
            let Ok(i) = u32::try_from(i) else { continue };
            let id = QTableId(i);
            let Some((kind, name)) = t.name_en.split_once('.') else {
                continue;
            };
            if name.contains('.') || schema.find_table(&[kind, name]) != Some(id) {
                continue;
            }
            match self.base.stored_table(id) {
                Ok(Some(_)) => list.tables.push(DataTable {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                    path: t.name_en.clone(),
                }),
                Ok(None) => {}
                Err(e) => list.dropped.push(DroppedTable {
                    path: t.name_en.clone(),
                    reason: e,
                }),
            }
        }
        list
    }

    pub fn table_indexes(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Vec<AvailableIndex>, TableReadError> {
        let stored = self.data_stored(kind, name)?;
        let indexes = stored.indexes().map_err(TableReadError::Storage)?;
        Ok(available_indexes(&stored, &indexes))
    }

    pub fn data_indexes(&self, kind: &str, name: &str) -> Result<DataIndexes, TableReadError> {
        let stored = self.data_stored(kind, name)?;
        let indexes = stored.indexes().map_err(TableReadError::Storage)?;
        let indexes = available_indexes(&stored, &indexes);
        let has = |role| {
            matches!(
                stored.record_filter_field(role),
                query_testhost::RecordField::Field(_)
            )
        };
        Ok(DataIndexes {
            default: pick_default(&indexes),
            limit: default_limit(stored.fields.len()),
            filters: DataFilters {
                posted: has(query_ir::schema::StdRole::Posted),
                deletion_mark: has(query_ir::schema::StdRole::DeletionMark),
            },
            indexes,
        })
    }

    pub fn default_index(&self, kind: &str, name: &str) -> Result<Option<String>, TableReadError> {
        Ok(pick_default(&self.table_indexes(kind, name)?))
    }

    pub fn period_access(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Result<PeriodAccess, String>, TableReadError> {
        let stored = self.data_stored(kind, name)?;
        let indexes = stored.indexes().map_err(TableReadError::Storage)?;
        let available = available_indexes(&stored, &indexes);
        let Some((ix, listed)) = PERIOD_INDEXES.iter().find_map(|want| {
            indexes
                .iter()
                .zip(&available)
                .find(|(ix, _)| ix.name.eq_ignore_ascii_case(want))
        }) else {
            return Ok(Err(format!(
                "no date index ({}) in the table",
                PERIOD_INDEXES.join(", ")
            )));
        };
        let Some(at) = ix
            .fields
            .iter()
            .position(|(f, _)| stored.condition_kind(*f) == Some(ConditionKind::Date))
        else {
            return Ok(Err(format!("index {} has no date field", ix.name)));
        };
        let fields = listed.fields.get(..=at).unwrap_or_default();
        if let Some(f) = fields.iter().find(|f| !f.conditions) {
            let reason = f.reason.as_deref().unwrap_or("no conditions");
            return Ok(Err(format!("{}: {reason}", f.name)));
        }
        let keyed = card::key_indexes(&stored, std::slice::from_ref(ix));
        if let Some(keyed) = keyed.first()
            && let Err(reason) = period_leading(keyed, at)
        {
            return Ok(Err(reason));
        }
        let Some((date, leading)) = fields.split_last() else {
            return Ok(Err(format!("index {} has no date field", ix.name)));
        };
        drop(stored);
        let first = |order| {
            let request = TableIndexRequest {
                index: ix.name.clone(),
                eq: Vec::new(),
                range: IndexRange::default(),
                order,
            };
            self.read_index_page_with(kind, name, 1, None, &request, portion())
        };
        let asc = first(IndexOrder::Asc)?;
        let Some(row) = asc.rows.first() else {
            return Ok(Err("the table is empty".into()));
        };
        let column = |page: &TablePage, field: &str| -> Option<usize> {
            page.columns.iter().position(|c| c.name == field)
        };
        let mut prefix = Vec::new();
        for f in leading {
            let value = column(&asc, &f.name)
                .and_then(|c| row.get(c))
                .and_then(condition_text);
            match value {
                Some(v) => prefix.push(v),
                None => {
                    return Ok(Err(format!(
                        "{}: the first entry has no condition value",
                        f.name
                    )));
                }
            }
        }
        let mut several = Vec::new();
        if !leading.is_empty() {
            let desc = first(IndexOrder::Desc)?;
            let last = desc.rows.first();
            for f in leading {
                let a = column(&asc, &f.name).and_then(|c| row.get(c));
                let b = column(&desc, &f.name).and_then(|c| last.and_then(|r| r.get(c)));
                if a != b {
                    several.push(f.name.clone());
                }
            }
        }
        Ok(Ok(PeriodAccess {
            index: ix.name.clone(),
            field: date.name.clone(),
            prefix,
            several,
        }))
    }

    pub fn data_page(
        &self,
        kind: &str,
        name: &str,
        req: &DataPageRequest,
    ) -> Result<DataPage, TableReadError> {
        let (columns, index) = {
            let stored = self.data_stored(kind, name)?;
            let index = match &req.index {
                Some(index) => Some(index.clone()),
                None => {
                    let indexes = stored.indexes().map_err(TableReadError::Storage)?;
                    pick_default(&available_indexes(&stored, &indexes))
                }
            };
            (stored.fields.len(), index)
        };
        let limit = req.limit.unwrap_or_else(|| default_limit(columns));
        let mut page = match &index {
            Some(index) => {
                let request = TableIndexRequest {
                    index: index.clone(),
                    eq: req.eq.clone(),
                    range: req.range.clone(),
                    order: req.order,
                };
                self.read_index_page_with(
                    kind,
                    name,
                    limit,
                    req.cursor.as_deref(),
                    &request,
                    filtered_portion(req.filter),
                )?
            }
            None => {
                if req.order == IndexOrder::Desc {
                    return Err(Invalid(
                        "descending order needs an index; the table has none of the default ones"
                            .into(),
                    ));
                }
                if !req.eq.is_empty() || req.range != IndexRange::default() {
                    return Err(Invalid("conditions need an index".into()));
                }
                self.read_table_page_with(
                    kind,
                    name,
                    limit,
                    req.cursor.as_deref(),
                    filtered_portion(req.filter),
                )?
            }
        };
        let cut = std::mem::take(&mut page.cut);
        let index = page.access.as_ref().map(|a| a.index.clone()).or(index);
        Ok(DataPage {
            page,
            index,
            limit,
            order: req.order,
            cut,
        })
    }
}

fn period_leading(index: &card::KeyIndex, at: usize) -> Result<(), String> {
    let date = index
        .fields
        .get(at)
        .map(|f| f.name.as_str())
        .unwrap_or_default();
    if card::area_prefix(index, date).is_some_and(|(found, _)| found == at) {
        return Ok(());
    }
    let field = index
        .fields
        .iter()
        .take(at)
        .find(|f| !(f.area && f.conditions))
        .map_or(date, |f| f.name.as_str());
    Err(format!(
        "{field}: not a data area in front of the date of index {}",
        index.name
    ))
}
