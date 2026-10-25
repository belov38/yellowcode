use super::card::{DataError, KeyIndex, ObjectRow, area_prefix, key_indexes, values_of};
use super::{CUT_LEN, PORTION_TEXT_BYTES, default_limit, portion};
use crate::table::PageOptions;
use crate::{
    Cell, CellKind, Console, IndexOrder, IndexRange, Lang, Mode, ParamIn, RunRequest,
    TableIndexRequest, TablePage, TableReadError,
};
use query_ir::QTableId;
use query_ir::schema::{FieldRole, StdRole, TableKind};
use serde::Serialize;

pub const NO_INDEX: &str = "чтение без индекса";

const LINE_FIELD: &str = "НомерСтроки";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRegister {
    pub register: String,
    pub title: String,
    pub has_records: bool,
    pub index: Option<String>,
    pub access: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DocumentRegisters {
    pub path: String,
    pub reference: String,
    pub registers: Vec<DocumentRegister>,
}

pub struct MovementsPage {
    pub page: TablePage,
    pub register: String,
    pub index: Option<String>,
    pub limit: usize,
    pub cut: Vec<(usize, usize, usize)>,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct MovementsKnob {
    pub text_bytes: usize,
    pub by_query: bool,
}

impl Default for MovementsKnob {
    fn default() -> Self {
        Self {
            text_bytes: PORTION_TEXT_BYTES,
            by_query: false,
        }
    }
}

struct Register {
    table: QTableId,
    path: String,
    title: String,
    recorder: String,
}

#[derive(Debug, PartialEq, Eq)]
struct RecorderIndex {
    index: String,
    leading: Vec<String>,
}

impl Console {
    fn registers_of(&self, main: QTableId) -> Vec<Register> {
        let schema = self.base.schema();
        let Some(atom) = schema.table(main).reference else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, t) in schema.tables().iter().enumerate() {
            if !matches!(t.kind, TableKind::Object(_)) {
                continue;
            }
            let Some((kind, name)) = t.name_en.split_once('.') else {
                continue;
            };
            if !kind.ends_with("Register") || name.contains('.') {
                continue;
            }
            let Some(recorder) = t
                .fields
                .iter()
                .find(|f| f.role == FieldRole::Std(StdRole::Recorder) && f.ty.refs.contains(&atom))
            else {
                continue;
            };
            let Ok(i) = u32::try_from(i) else { continue };
            out.push(Register {
                table: QTableId(i),
                path: t.name_en.clone(),
                title: t.name_ru.clone(),
                recorder: recorder.name_ru.clone(),
            });
        }
        out
    }

    fn recorder_index(
        &self,
        table: QTableId,
        recorder: &str,
        row: &ObjectRow,
    ) -> Result<Option<RecorderIndex>, TableReadError> {
        let values = row.page.rows.first().map(Vec::as_slice).unwrap_or_default();
        let stored = self.stored_of(table)?;
        let indexes = stored.indexes().map_err(TableReadError::Storage)?;
        Ok(choose_recorder_index(
            &key_indexes(&stored, &indexes),
            recorder,
            |name| values_of(&[name.to_owned()], &row.page.columns, values).is_ok(),
        ))
    }

    fn recorder_request(
        index: &RecorderIndex,
        row: &ObjectRow,
        reference: &str,
    ) -> Result<TableIndexRequest, TableReadError> {
        let values = row.page.rows.first().map(Vec::as_slice).unwrap_or_default();
        let mut eq = values_of(&index.leading, &row.page.columns, values)?;
        eq.push(reference.to_owned());
        Ok(TableIndexRequest {
            index: index.index.clone(),
            eq,
            range: IndexRange::default(),
            order: IndexOrder::Asc,
        })
    }

    fn movement_options(text_bytes: usize) -> PageOptions {
        PageOptions {
            max_text_bytes: Some(text_bytes),
            ref_pairs: true,
            ..portion()
        }
    }

    pub fn document_registers(
        &self,
        reference: &str,
    ) -> Result<Option<DocumentRegisters>, DataError> {
        let (main, reference) = self.card_ref(reference)?;
        let Some(row) = self.object_row(main, &reference, portion(), false)? else {
            return Ok(None);
        };
        let registers = self
            .registers_of(main)
            .into_iter()
            .map(|r| self.document_register(&r, &row, &reference))
            .collect();
        Ok(Some(DocumentRegisters {
            path: self.base.schema().table(main).name_en.clone(),
            reference,
            registers,
        }))
    }

    fn document_register(
        &self,
        r: &Register,
        row: &ObjectRow,
        reference: &str,
    ) -> DocumentRegister {
        let mut out = DocumentRegister {
            register: r.path.clone(),
            title: r.title.clone(),
            has_records: false,
            index: None,
            access: NO_INDEX.to_owned(),
            error: None,
        };
        let checked = match self.recorder_index(r.table, &r.recorder, row) {
            Ok(Some(index)) => {
                out.access = format!("индекс {}", index.index);
                out.index = Some(index.index.clone());
                Self::recorder_request(&index, row, reference)
                    .and_then(|request| {
                        self.index_has_entry_of(
                            r.table,
                            &request,
                            Self::movement_options(PORTION_TEXT_BYTES),
                        )
                    })
                    .map_err(|e| e.to_string())
            }
            Ok(None) => self
                .movements_query(r, reference, None, 1)
                .map(|(_, rows)| !rows.is_empty())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match checked {
            Ok(has) => out.has_records = has,
            Err(e) => out.error = Some(format!("Регистр не проверен: {e}")),
        }
        out
    }

    pub fn document_movements_page(
        &self,
        reference: &str,
        register: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Option<MovementsPage>, DataError> {
        self.document_movements_page_with(
            reference,
            register,
            cursor,
            limit,
            MovementsKnob::default(),
        )
    }

    #[doc(hidden)]
    pub fn document_movements_page_with(
        &self,
        reference: &str,
        register: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
        knob: MovementsKnob,
    ) -> Result<Option<MovementsPage>, DataError> {
        let (main, reference) = self.card_ref(reference)?;
        let r = self
            .registers_of(main)
            .into_iter()
            .find(|r| r.path == register)
            .ok_or_else(|| {
                DataError::Invalid(format!("{register} is not a register of the document"))
            })?;
        if let Some(limit) = limit
            && !(1..=10_000).contains(&limit)
        {
            return Err(DataError::Invalid(
                "limit must be between 1 and 10000".into(),
            ));
        }
        let Some(row) = self.object_row(main, &reference, portion(), false)? else {
            return Ok(None);
        };
        let columns = self
            .stored_of(r.table)
            .map_err(DataError::Table)?
            .fields
            .len();
        let limit = limit.unwrap_or_else(|| default_limit(columns));
        let index = if knob.by_query {
            None
        } else {
            self.recorder_index(r.table, &r.recorder, &row)
                .map_err(DataError::Table)?
        };
        let Some(index) = index else {
            return self
                .movements_by_query(&r, &reference, cursor, limit, knob.text_bytes)
                .map(Some);
        };
        let request = Self::recorder_request(&index, &row, &reference).map_err(DataError::Table)?;
        drop(row);
        let mut page = self
            .read_index_page_of(
                r.table,
                limit,
                cursor,
                &request,
                Self::movement_options(knob.text_bytes),
            )
            .map_err(DataError::Table)?;
        let cut = std::mem::take(&mut page.cut);
        Ok(Some(MovementsPage {
            page,
            register: r.path,
            index: Some(index.index),
            limit,
            cut,
        }))
    }

    fn movements_query(
        &self,
        r: &Register,
        reference: &str,
        after: Option<u64>,
        n: usize,
    ) -> Result<(Vec<crate::Column>, Vec<Vec<Cell>>), DataError> {
        let schema = self.base.schema();
        if schema.find_field(r.table, LINE_FIELD).is_none() {
            return Err(DataError::Table(TableReadError::Unsupported(format!(
                "{}: no {LINE_FIELD}",
                r.title
            ))));
        }
        let mut text = format!(
            "ВЫБРАТЬ ПЕРВЫЕ {n} * ИЗ {} КАК Т ГДЕ Т.{} = &Д",
            r.title, r.recorder
        );
        let mut params = vec![ParamIn {
            name: "Д".into(),
            repr: reference.to_owned(),
        }];
        if let Some(after) = after {
            text.push_str(&format!(" И Т.{LINE_FIELD} > &Н"));
            params.push(ParamIn {
                name: "Н".into(),
                repr: after.to_string(),
            });
        }
        text.push_str(&format!(" УПОРЯДОЧИТЬ ПО Т.{LINE_FIELD}"));
        let report = self.run(
            &RunRequest {
                text,
                params,
                mode: Mode::One,
                lang: Lang::En,
            },
            &std::sync::atomic::AtomicBool::new(false),
        );
        if let Some(e) = report.error {
            return Err(DataError::Query(e));
        }
        let last = report.rows.stmt_count().saturating_sub(1);
        Ok((
            report.rows.columns(last).to_vec(),
            report.rows.slice(last, 0, n).to_vec(),
        ))
    }

    fn movements_by_query(
        &self,
        r: &Register,
        reference: &str,
        cursor: Option<&str>,
        limit: usize,
        text_bytes: usize,
    ) -> Result<MovementsPage, DataError> {
        let prefix = format!("m1:{}:{}:{reference}:", self.cursor_generation, r.table.0);
        let after = cursor
            .map(|c| {
                c.strip_prefix(&prefix)
                    .and_then(|n| n.parse::<u64>().ok())
                    .ok_or_else(|| {
                        DataError::Invalid(
                            "cursor belongs to another request/opening or is invalid; restart the scan"
                                .into(),
                        )
                    })
            })
            .transpose()?;
        let (columns, found) =
            self.movements_query(r, reference, after, limit.saturating_add(1))?;
        let line = columns
            .iter()
            .position(|c| c.name == LINE_FIELD)
            .ok_or_else(|| {
                DataError::Table(TableReadError::Storage(format!(
                    "{}: the query gave no {LINE_FIELD}",
                    r.title
                )))
            })?;
        let mut rows = Vec::new();
        let mut cut = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        let mut last = None;
        let count = found.len();
        for (i, mut row) in found.into_iter().enumerate() {
            if i == limit {
                more = true;
                break;
            }
            bytes = row.iter().fold(bytes, |n, c| n.saturating_add(c.t.len()));
            last = row.get(line).and_then(|c| c.t.parse::<u64>().ok()).or(last);
            for (c, length) in cut_text(&mut row) {
                cut.push((i, c, length));
            }
            rows.push(row);
            if bytes >= text_bytes {
                more = i + 1 < count;
                break;
            }
        }
        let next_cursor = match (more, last) {
            (true, Some(n)) => Some(format!("{prefix}{n}")),
            (true, None) => {
                return Err(DataError::Table(TableReadError::Storage(format!(
                    "{}: a row without {LINE_FIELD}",
                    r.title
                ))));
            }
            _ => None,
        };
        Ok(MovementsPage {
            page: TablePage {
                schema_version: 1,
                columns,
                rows,
                next_cursor,
                access: None,
                filter: None,
                scanned: None,
                _retained: None,
                cut: Vec::new(),
            },
            register: r.path.clone(),
            index: None,
            limit,
            cut,
        })
    }
}

fn choose_recorder_index(
    indexes: &[KeyIndex],
    recorder: &str,
    has_value: impl Fn(&str) -> bool,
) -> Option<RecorderIndex> {
    indexes
        .iter()
        .filter_map(|ix| {
            let (at, leading) = area_prefix(ix, recorder)?;
            if !leading.iter().all(|name| has_value(name)) {
                return None;
            }
            let key = ix.fields.get(at)?;
            (key.conditions || key.pair).then(|| (at, ix.fields.len(), ix.name.clone(), leading))
        })
        .min_by_key(|(at, len, _, _)| (*at, *len))
        .map(|(_, _, index, leading)| RecorderIndex { index, leading })
}

fn cut_text(row: &mut [Cell]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (c, cell) in row.iter_mut().enumerate() {
        let length = match cell.k {
            CellKind::String => cell
                .t
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .map(|s| s.replace("\"\"", "\"").encode_utf16().count()),
            CellKind::Binary => cell
                .t
                .strip_prefix("ДвоичныеДанные(")
                .and_then(|s| s.strip_suffix(')'))
                .map(|hex| hex.len() / 2),
            _ => continue,
        };
        let Some((at, _)) = cell.t.char_indices().nth(CUT_LEN) else {
            continue;
        };
        let length = length.unwrap_or_else(|| cell.t.chars().count());
        cell.t.truncate(at);
        cell.t.shrink_to_fit();
        out.push((c, length));
    }
    out
}
