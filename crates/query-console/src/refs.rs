use crate::Console;
use crate::cell::{Cell, CellKind, Column};
use crate::run::{ErrorOut, Lang, Mode, ParamIn, Phase, Report, RunRequest, Stmt};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_REFS_PER_KIND: usize = 200;

pub const NESTED_LATER: &str = "вложенные таблицы";

#[derive(Debug, Default, Serialize)]
pub struct Presented {
    texts: Vec<Vec<Option<PresentedCell>>>,
    #[serde(rename = "present_errors")]
    errors: Vec<String>,
    pub queries: usize,
    #[serde(skip)]
    retained: Option<memory_budget::Reservation>,
}

impl Presented {
    pub fn texts(&self) -> &[Vec<Option<PresentedCell>>] {
        &self.texts
    }
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
    fn interrupted() -> Self {
        Self {
            errors: vec!["Выполнение прервано".into()],
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Presentation,
    Fields,
    Canonical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PresentedCell {
    pub text: String,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NestedOut {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
}

fn unquote(t: &str) -> Option<String> {
    let inner = t.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\"\"", "\""))
}

const CLASSES: [&str; 9] = [
    "Catalog",
    "Document",
    "Enum",
    "ChartOfAccounts",
    "ChartOfCharacteristicTypes",
    "ChartOfCalculationTypes",
    "ExchangePlan",
    "BusinessProcess",
    "Task",
];

fn parse_kind(kind: &str) -> Result<(&'static str, &str), String> {
    let (class, name) = kind
        .split_once('.')
        .ok_or_else(|| "unknown reference class".to_string())?;
    let class = class
        .strip_suffix("Ref")
        .and_then(|c| CLASSES.iter().find(|k| **k == c))
        .ok_or_else(|| "unknown reference class".to_string())?;
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err("invalid object name".to_string());
    }
    Ok((class, name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Presentation,
    Field(&'static str),
    NumberDate,
}

fn follow_up_text(class: &str, name: &str, shape: Shape) -> String {
    let cols = match shape {
        Shape::Presentation => "PRESENTATION(T.Ref)".to_string(),
        Shape::Field(f) => format!("T.{f}"),
        Shape::NumberDate => "T.Number, T.Date".to_string(),
    };
    format!("SELECT T.Ref, {cols} FROM {class}.{name} AS T WHERE T.Ref IN (&P)")
}

fn map_rows(shape: Shape, rows: &[Vec<Cell>]) -> Vec<(String, String, String)> {
    rows.iter()
        .filter_map(|row| {
            let r = row.first()?.r.as_ref()?;
            let a = scalar_text(row.get(1)?)?;
            let text = match shape {
                Shape::NumberDate => format!("{a} от {}", scalar_text(row.get(2)?)?),
                _ => a,
            };
            Some((r.kind.clone(), r.id.clone(), text))
        })
        .collect()
}

fn is_empty_id(id: &str) -> bool {
    id.chars().all(|c| c == '0' || c == '-')
}

fn scalar_text(c: &Cell) -> Option<String> {
    let t = match c.k {
        CellKind::String => unquote(&c.t)?,
        CellKind::Date => c.t.trim_matches('\'').to_string(),
        CellKind::Number => c.t.clone(),
        _ => return None,
    };
    (!t.is_empty()).then_some(t)
}

impl Console {
    pub fn present(&self, rows: &[Vec<Cell>], cancel: &AtomicBool) -> Presented {
        self.independent(cancel, |cancel| self.present_active(rows, cancel))
            .unwrap_or_else(|e| Presented {
                errors: e.chain.to_vec(),
                ..Presented::default()
            })
    }

    fn present_active(&self, rows: &[Vec<Cell>], cancel: &AtomicBool) -> Presented {
        if cancel.load(Ordering::Acquire) {
            return Presented::interrupted();
        }
        let mut scratch_bytes = 4096u64;
        for row in rows {
            for cell in row {
                if cancel.load(Ordering::Acquire) {
                    return Presented::interrupted();
                }
                scratch_bytes = scratch_bytes
                    .saturating_add(crate::cell::row_bytes(std::slice::from_ref(cell)) as u64);
            }
        }
        let scratch_bytes = scratch_bytes.saturating_mul(8);
        let scratch = match self
            .base
            .memory_budget()
            .map(|budget| budget.try_reserve(memory_budget::Category::Execution, scratch_bytes))
            .transpose()
        {
            Ok(scratch) => scratch,
            Err(error) => {
                return Presented {
                    errors: vec![error.public_text().into()],
                    ..Presented::default()
                };
            }
        };
        let mut groups: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
        for c in rows.iter().flatten() {
            if cancel.load(Ordering::Acquire) {
                return Presented::interrupted();
            }
            if let Some(r) = c.r.as_ref().filter(|r| !is_empty_id(&r.id)) {
                let g = groups.entry(r.kind.as_str()).or_default();
                if g.len() <= MAX_REFS_PER_KIND {
                    let mut duplicate = false;
                    for previous in g.iter() {
                        if cancel.load(Ordering::Acquire) {
                            return Presented::interrupted();
                        }
                        if previous.t == c.t {
                            duplicate = true;
                            break;
                        }
                    }
                    if !duplicate {
                        g.push(c);
                    }
                }
            }
        }
        let mut out = Presented::default();
        if cancel.load(Ordering::Acquire) {
            return Presented::interrupted();
        }
        let by_presentation = self.presentation_works(cancel);
        if cancel.load(Ordering::Acquire) {
            return Presented::interrupted();
        }
        let mut found: HashMap<(String, String), (String, Source)> = HashMap::new();
        for (kind, cells) in &groups {
            if cancel.load(Ordering::Acquire) {
                return Presented::interrupted();
            }
            let (class, name) = match parse_kind(kind) {
                Ok(x) => x,
                Err(e) => {
                    out.errors.push(format!("{kind}: {e}"));
                    continue;
                }
            };
            if cells.len() > MAX_REFS_PER_KIND {
                out.errors.push(format!(
                    "{kind}: more than {MAX_REFS_PER_KIND} references in the window; the rest keep the canonical text"
                ));
            }
            let list: Vec<&Cell> = cells.iter().take(MAX_REFS_PER_KIND).copied().collect();
            if class == "Enum" && !by_presentation {
                if let Err(error) =
                    self.enum_names(kind, &list, &mut found, scratch.as_ref(), cancel)
                {
                    out.errors.push(error);
                }
                if cancel.load(Ordering::Acquire) {
                    return Presented::interrupted();
                }
                continue;
            }
            let (shape, source) = if by_presentation {
                (Shape::Presentation, Source::Presentation)
            } else if matches!(class, "Document" | "BusinessProcess" | "Task") {
                (Shape::NumberDate, Source::Fields)
            } else {
                match self.title_field(class, name) {
                    Some(f) => (Shape::Field(f), Source::Fields),
                    None => {
                        out.errors
                            .push(format!("{kind}: no Description or Code to show"));
                        continue;
                    }
                }
            };
            if cancel.load(Ordering::Acquire) {
                return Presented::interrupted();
            }
            out.queries += 1;
            match self.follow_up(&follow_up_text(class, name, shape), &list, cancel) {
                Ok(report) => {
                    if let Some(scratch) = &scratch
                        && let Err(error) = scratch.try_grow(
                            (report.rows.approx_bytes() as u64)
                                .saturating_mul(8)
                                .saturating_add(4096),
                        )
                    {
                        out.errors.push(error.public_text().into());
                        continue;
                    }
                    for row in report.rows.slice(0, 0, usize::MAX) {
                        if cancel.load(Ordering::Acquire) {
                            return Presented::interrupted();
                        }
                        for (k, id, text) in map_rows(shape, std::slice::from_ref(row)) {
                            found.insert((k, id), (text, source));
                        }
                    }
                }
                Err(e) => out.errors.push(format!("{kind}: {e}")),
            }
        }
        if cancel.load(Ordering::Acquire) {
            return Presented::interrupted();
        }
        let mut output_bytes = out.errors.iter().fold(256u64, |bytes, error| {
            bytes
                .saturating_add((error.capacity() as u64).saturating_mul(2))
                .saturating_add(64)
        });
        for row in rows {
            output_bytes = output_bytes.saturating_add(64);
            for cell in row {
                if cancel.load(Ordering::Acquire) {
                    return Presented::interrupted();
                }
                let text_len = cell.r.as_ref().map_or(0, |r| {
                    found
                        .get(&(r.kind.clone(), r.id.clone()))
                        .map_or(cell.t.len(), |(text, _)| text.len())
                });
                output_bytes = output_bytes
                    .saturating_add(128)
                    .saturating_add((text_len as u64).saturating_mul(2));
            }
        }
        out.retained = match self
            .base
            .memory_budget()
            .map(|budget| budget.try_reserve(memory_budget::Category::Result, output_bytes))
            .transpose()
        {
            Ok(retained) => retained,
            Err(error) => {
                return Presented {
                    errors: vec![error.public_text().into()],
                    ..Presented::default()
                };
            }
        };
        for row in rows {
            let mut texts = Vec::with_capacity(row.len());
            for cell in row {
                if cancel.load(Ordering::Acquire) {
                    return Presented::interrupted();
                }
                texts.push(cell.r.as_ref().map(
                    |r| match found.get(&(r.kind.clone(), r.id.clone())) {
                        Some((text, source)) => PresentedCell {
                            text: text.clone(),
                            source: *source,
                        },
                        None => PresentedCell {
                            text: cell.t.clone(),
                            source: Source::Canonical,
                        },
                    },
                ));
            }
            out.texts.push(texts);
        }
        if cancel.load(Ordering::Acquire) {
            return Presented::interrupted();
        }
        out
    }

    fn presentation_works(&self, cancel: &AtomicBool) -> bool {
        if let Some(v) = self.presentation.get() {
            return *v;
        }
        let Ok(rep) = self.run_budgeted_active(
            &RunRequest {
                text: "SELECT PRESENTATION(1)".to_string(),
                params: Vec::new(),
                mode: Mode::Batch,
                lang: Lang::En,
            },
            cancel,
        ) else {
            return false;
        };
        match rep.error.as_ref().map(|e| e.phase) {
            None => *self.presentation.get_or_init(|| true),
            Some(Phase::Later) => *self.presentation.get_or_init(|| false),
            Some(_) => false,
        }
    }

    fn title_field(&self, class: &str, name: &str) -> Option<&'static str> {
        let schema = self.base.schema();
        let t = schema.find_table(&[class, name])?;
        ["Description", "Code"]
            .into_iter()
            .find(|f| schema.find_field(t, f).is_some())
    }

    fn enum_names(
        &self,
        kind: &str,
        list: &[&Cell],
        found: &mut HashMap<(String, String), (String, Source)>,
        scratch: Option<&memory_budget::Reservation>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let mut number = None;
        for (candidate, name) in self.base.ref_kinds_borrowed() {
            if cancel.load(Ordering::Acquire) {
                return Err("Выполнение прервано".into());
            }
            if name == kind {
                number = Some(candidate);
                break;
            }
        }
        let Some(number) = number else {
            return Ok(());
        };
        for r in list.iter().filter_map(|c| c.r.as_ref()) {
            for (id, name) in self.base.named_refs_borrowed(number) {
                #[cfg(test)]
                self.query_checkpoint("enum-entry", cancel);
                if cancel.load(Ordering::Acquire) {
                    return Err("Выполнение прервано".into());
                }
                if query_ir::uuid_of(&id).to_string() != r.id {
                    continue;
                }
                if let Some(scratch) = scratch {
                    scratch
                        .try_grow((name.len() as u64).saturating_mul(4).saturating_add(256))
                        .map_err(|error| error.public_text().to_string())?;
                }
                found.insert(
                    (r.kind.clone(), r.id.clone()),
                    (name.to_string(), Source::Fields),
                );
                break;
            }
        }
        Ok(())
    }

    fn follow_up(&self, text: &str, list: &[&Cell], cancel: &AtomicBool) -> Result<Report, String> {
        #[cfg(test)]
        self.query_checkpoint("followup", cancel);
        if cancel.load(Ordering::Acquire) {
            return Err("Выполнение прервано".into());
        }
        let texts: Vec<&str> = list.iter().map(|c| c.t.as_str()).collect();
        let rep = self
            .run_budgeted_active(
                &RunRequest {
                    text: text.to_string(),
                    params: vec![ParamIn {
                        name: "P".to_string(),
                        repr: format!("Массив[{}]", texts.join(", ")),
                    }],
                    mode: Mode::Batch,
                    lang: Lang::En,
                },
                cancel,
            )
            .map_err(|error| error.to_string())?;
        if let Some(e) = &rep.error {
            return Err(e.chain.join(": "));
        }
        Ok(rep)
    }

    pub fn nested<'a>(
        &self,
        report: &'a Report,
        stmt: usize,
        row: usize,
        col: usize,
    ) -> Result<&'a NestedOut, ErrorOut> {
        nested_of(report, stmt, row, col)
    }
}

fn nested_of(report: &Report, stmt: usize, row: usize, col: usize) -> Result<&NestedOut, ErrorOut> {
    let bad = |t: &str| ErrorOut::new(Phase::Runtime, t);
    if !matches!(report.statements().get(stmt), Some(Stmt::Result { .. })) {
        return Err(bad("no such result statement"));
    }
    let cell = report
        .rows
        .slice(stmt, row, 1)
        .first()
        .and_then(|r| r.get(col))
        .ok_or_else(|| bad("no such cell"))?;
    if cell.k != CellKind::Nested {
        return Err(bad("the cell is not a nested table"));
    }
    report
        .rows
        .nested(stmt, row, col)
        .ok_or_else(|| bad("the nested table is not kept"))
}
