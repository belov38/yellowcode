use crate::Console;
use crate::cell::{Cell, CellKind, Column};
use crate::run::{ErrorOut, Lang, Mode, ParamIn, Phase, Report, RunRequest, Stmt};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;

pub const MAX_REFS_PER_KIND: usize = 200;

pub const NESTED_LATER: &str = "вложенные таблицы";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Presented {
    pub texts: Vec<Vec<Option<PresentedCell>>>,
    #[serde(rename = "present_errors")]
    pub errors: Vec<String>,
    pub queries: usize,
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
        let mut groups: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
        for c in rows.iter().flatten() {
            if let Some(r) = c.r.as_ref().filter(|r| !is_empty_id(&r.id)) {
                let g = groups.entry(r.kind.as_str()).or_default();
                if !g.iter().any(|x| x.t == c.t) {
                    g.push(c);
                }
            }
        }
        let mut out = Presented::default();
        let by_presentation = self.presentation_works(cancel);
        let mut found: HashMap<(String, String), (String, Source)> = HashMap::new();
        for (kind, cells) in &groups {
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
                self.enum_names(kind, &list, &mut found);
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
            out.queries += 1;
            match self.follow_up(&follow_up_text(class, name, shape), &list, cancel) {
                Ok(rows) => {
                    for (k, id, text) in map_rows(shape, &rows) {
                        found.insert((k, id), (text, source));
                    }
                }
                Err(e) => out.errors.push(format!("{kind}: {e}")),
            }
        }
        out.texts = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| {
                        let r = c.r.as_ref()?;
                        Some(match found.get(&(r.kind.clone(), r.id.clone())) {
                            Some((text, source)) => PresentedCell {
                                text: text.clone(),
                                source: *source,
                            },
                            None => PresentedCell {
                                text: c.t.clone(),
                                source: Source::Canonical,
                            },
                        })
                    })
                    .collect()
            })
            .collect();
        out
    }

    fn presentation_works(&self, cancel: &AtomicBool) -> bool {
        if let Some(v) = self.presentation.get() {
            return *v;
        }
        let rep = self.run(
            &RunRequest {
                text: "SELECT PRESENTATION(1)".to_string(),
                params: Vec::new(),
                mode: Mode::Batch,
                lang: Lang::En,
            },
            cancel,
        );
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
    ) {
        let Some(number) = self
            .base
            .ref_kinds()
            .into_iter()
            .find(|(_, n)| *n == kind)
            .map(|(n, _)| n)
        else {
            return;
        };
        let names = self.base.named_refs(number);
        for r in list.iter().filter_map(|c| c.r.as_ref()) {
            if let Some((_, n)) = names
                .iter()
                .find(|(id, _)| query_ir::uuid_of(id).to_string() == r.id)
            {
                found.insert((r.kind.clone(), r.id.clone()), (n.clone(), Source::Fields));
            }
        }
    }

    fn follow_up(
        &self,
        text: &str,
        list: &[&Cell],
        cancel: &AtomicBool,
    ) -> Result<Vec<Vec<Cell>>, String> {
        let texts: Vec<&str> = list.iter().map(|c| c.t.as_str()).collect();
        let rep = self.run(
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
        );
        if let Some(e) = &rep.error {
            return Err(e.chain.join(": "));
        }
        Ok(rep.rows.slice(0, 0, usize::MAX).to_vec())
    }

    pub fn nested(
        &self,
        report: &Report,
        stmt: usize,
        row: usize,
        col: usize,
    ) -> Result<NestedOut, ErrorOut> {
        nested_of(report, stmt, row, col)
    }
}

fn nested_of(report: &Report, stmt: usize, row: usize, col: usize) -> Result<NestedOut, ErrorOut> {
    let bad = |t: &str| ErrorOut::new(Phase::Runtime, t);
    if !matches!(report.statements.get(stmt), Some(Stmt::Result { .. })) {
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
        .cloned()
        .ok_or_else(|| bad("the nested table is not kept"))
}
