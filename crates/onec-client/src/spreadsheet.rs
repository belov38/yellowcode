use roxmltree::{Document, Node};

use crate::answer::{NS_CORE, NS_WS};

pub const NS_SHEET: &str = "http://v8.1c.ru/8.2/data/spreadsheet";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const NS_CFG: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";
const NS_XS: &str = "http://www.w3.org/2001/XMLSchema";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetLimits {
    pub rows: u32,
    pub columns: u32,
    pub area: u64,
    pub cells: usize,
}

pub const DEFAULT_LIMITS: SheetLimits = SheetLimits {
    rows: 100_000,
    columns: 10_000,
    area: 20_000_000,
    cells: 2_000_000,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SheetError {
    #[error("spreadsheet: missing {0}")]
    Missing(&'static str),
    #[error("spreadsheet: bad {field}: {value:?}")]
    Bad { field: &'static str, value: String },
    #[error("spreadsheet: unknown form of the document <{0}>")]
    UnknownForm(String),
    #[error(
        "spreadsheet: the server gave the changes of the document ({0}), not the document; not read"
    )]
    Delta(String),
    #[error(
        "spreadsheet: {what} {got} is more than the limit {limit}; the report is too large to read whole"
    )]
    TooLarge {
        what: &'static str,
        got: u64,
        limit: u64,
    },
    #[error("getRowContents: {0}")]
    Rows(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    pub type_name: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub row: u32,
    pub column: u32,
    pub text: String,
    pub details: Option<Details>,
    pub details_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merge {
    pub row: u32,
    pub column: u32,
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sheet {
    pub height: u32,
    pub width: u32,
    pub cells: Vec<Cell>,
    pub merges: Vec<Merge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetChanges {
    Full(Sheet),
    WithoutContent { key: String, sheet: Sheet },
}

impl Sheet {
    pub fn rows(&self) -> Vec<&[Cell]> {
        let mut out = Vec::with_capacity(self.height as usize);
        let mut rest = self.cells.as_slice();
        for r in 0..self.height {
            let n = rest.iter().take_while(|c| c.row == r).count();
            let (row, tail) = rest.split_at(n);
            out.push(row);
            rest = tail;
        }
        out
    }

    pub fn settle(&mut self, limits: SheetLimits) -> Result<(), SheetError> {
        self.cells.sort_by_key(|c| (c.row, c.column));
        if let Some(last) = self.cells.iter().map(|c| c.column.saturating_add(1)).max() {
            self.width = self.width.max(last);
        }
        check_size(self, limits)
    }
}

fn check_size(s: &Sheet, limits: SheetLimits) -> Result<(), SheetError> {
    let too = |what, got: u64, limit: u64| SheetError::TooLarge { what, got, limit };
    if s.height > limits.rows {
        return Err(too(
            "the number of rows",
            u64::from(s.height),
            u64::from(limits.rows),
        ));
    }
    if s.width > limits.columns {
        return Err(too(
            "the number of columns",
            u64::from(s.width),
            u64::from(limits.columns),
        ));
    }
    let area = u64::from(s.height) * u64::from(s.width);
    if area > limits.area {
        return Err(too("rows × columns", area, limits.area));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct CellCount(usize);

impl CellCount {
    fn add(&mut self, n: usize, limits: SheetLimits) -> Result<(), SheetError> {
        self.0 = self.0.saturating_add(n);
        if self.0 > limits.cells {
            return Err(SheetError::TooLarge {
                what: "the number of cells",
                got: self.0 as u64,
                limit: limits.cells as u64,
            });
        }
        Ok(())
    }
}

pub fn read_changes(
    changes: Node,
    limits: SheetLimits,
    count: &mut CellCount,
) -> Result<SheetChanges, SheetError> {
    let children: Vec<Node> = changes.children().filter(Node::is_element).collect();
    let Some(doc) = children
        .iter()
        .copied()
        .find(|n| is_sheet(*n, "fullDocument") || is_sheet(*n, "fullDocumentWithoutContent"))
    else {
        let delta =
            |n: &Node| is_sheet(*n, "row") || is_sheet(*n, "vg") || is_sheet(*n, "commonPart");
        return Err(match children.iter().find(|n| !delta(n)) {
            _ if children.is_empty() => SheetError::Missing("the document in changes"),
            Some(other) => SheetError::UnknownForm(other.tag_name().name().to_string()),
            None => SheetError::Delta(
                children
                    .iter()
                    .map(|n| n.tag_name().name())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        });
    };
    read_document(doc, is_sheet(doc, "fullDocument"), limits, count)
}

pub fn read_document(
    doc: Node,
    full: bool,
    limits: SheetLimits,
    count: &mut CellCount,
) -> Result<SheetChanges, SheetError> {
    let key = child(doc, "distributedKey").and_then(text);
    let width = doc
        .children()
        .filter(|n| is_sheet(*n, "columns"))
        .filter_map(|n| child(n, "size"))
        .map(|n| number(n, "columns size"))
        .try_fold(0u32, |w, s| s.map(|s| w.max(s)))?;
    let mut sheet = Sheet {
        height: 0,
        width,
        cells: Vec::new(),
        merges: Vec::new(),
    };
    let items: Vec<Node> = doc
        .children()
        .filter(|n| is_sheet(*n, "rowsItem"))
        .collect();
    let mut last = 0u32;
    for item in &items {
        let (_, to) = rows_of(*item)?;
        last = last.max(to.saturating_add(1));
    }
    sheet.height = match child(doc, "height") {
        Some(h) => number(h, "height")?,
        None => last,
    }
    .max(last);
    check_size(&sheet, limits)?;
    for m in doc.children().filter(|n| is_sheet(*n, "merge")) {
        sheet.merges.push(Merge {
            row: number(
                child(m, "r").ok_or(SheetError::Missing("merge r"))?,
                "merge r",
            )?,
            column: number(
                child(m, "c").ok_or(SheetError::Missing("merge c"))?,
                "merge c",
            )?,
            columns: child(m, "w").map_or(Ok(0), |n| number(n, "merge w"))?,
            rows: child(m, "h").map_or(Ok(0), |n| number(n, "merge h"))?,
        });
    }
    let inline = items.iter().any(|item| {
        child(*item, "row").is_some_and(|row| {
            row.children()
                .any(|n| n.is_element() && n.tag_name().name() == "c")
        })
    });
    if !full && !inline {
        let key = key.ok_or(SheetError::Missing("distributedKey"))?;
        return Ok(SheetChanges::WithoutContent { key, sheet });
    }
    for item in items {
        let Some(row) = child(item, "row") else {
            continue;
        };
        let (from, to) = rows_of(item)?;
        let cells = row_cells(row, true, limits)?;
        let n = cells.len().saturating_mul((to - from + 1) as usize);
        count.add(n, limits)?;
        for r in from..=to {
            for (column, text, details, details_id) in &cells {
                sheet.cells.push(Cell {
                    row: r,
                    column: *column,
                    text: text.clone(),
                    details: details.clone(),
                    details_id: *details_id,
                });
            }
        }
    }
    sheet.settle(limits)?;
    Ok(SheetChanges::Full(sheet))
}

pub fn row_contents_request(key: &str, rows: &[u32]) -> Vec<u8> {
    let mut r = String::from(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><key>",
    );
    r.push_str(&crate::xml::escape_text(key));
    r.push_str("</key>");
    for n in rows {
        r.push_str(&format!("<r>{n}</r>"));
    }
    r.push_str("</request>");
    r.into_bytes()
}

pub fn read_row_contents(
    body: &[u8],
    rows: &[u32],
    sheet: &mut Sheet,
    limits: SheetLimits,
    count: &mut CellCount,
) -> Result<(), SheetError> {
    let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body));
    let doc = Document::parse(&text).map_err(|e| SheetError::Rows(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().namespace() != Some(NS_WS) || root.tag_name().name() != "response" {
        return Err(SheetError::Rows(format!(
            "the root is <{}> in {}, not <response> in {NS_WS}",
            root.tag_name().name(),
            root.tag_name().namespace().unwrap_or("no namespace")
        )));
    }
    let got: Vec<Node> = root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "row")
        .collect();
    if got.len() != rows.len() {
        return Err(SheetError::Rows(format!(
            "{} rows asked, {} given",
            rows.len(),
            got.len()
        )));
    }
    for (r, row) in rows.iter().zip(got) {
        let cells = row_cells(row, false, limits)?;
        count.add(cells.len(), limits)?;
        for (column, text, details, details_id) in cells {
            sheet.cells.push(Cell {
                row: *r,
                column,
                text,
                details,
                details_id,
            });
        }
    }
    Ok(())
}

type RowCell = (u32, String, Option<Details>, Option<u32>);

fn row_cells(row: Node, full: bool, limits: SheetLimits) -> Result<Vec<RowCell>, SheetError> {
    let mut out = Vec::new();
    let mut column = 0u32;
    for c in row
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "c")
    {
        if let Some(i) = child_any(c, "i") {
            column = number(i, "cell i")?;
        }
        if column >= limits.columns {
            return Err(SheetError::TooLarge {
                what: "the column of a cell",
                got: u64::from(column) + 1,
                limit: u64::from(limits.columns),
            });
        }
        let cell = if full {
            child_any(c, "c").unwrap_or(c)
        } else {
            c
        };
        let text = child_any(cell, "t")
            .map(|t| t.text().unwrap_or("").to_string())
            .or_else(|| child_any(cell, "tl").map(local_text));
        let details = child_any(cell, "d").map(details_of);
        let details_id = if full {
            child_any(cell, "r")
                .map(|r| number(r, "cell r"))
                .transpose()?
        } else {
            None
        };
        if text.as_deref().is_some_and(|t| !t.is_empty())
            || details.is_some()
            || details_id.is_some()
        {
            out.push((column, text.unwrap_or_default(), details, details_id));
        }
        column = column.checked_add(1).ok_or(SheetError::Bad {
            field: "cell i",
            value: column.to_string(),
        })?;
    }
    Ok(out)
}

fn local_text(tl: Node) -> String {
    tl.descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_CORE)
                && n.tag_name().name() == "content"
        })
        .and_then(|n| n.text())
        .unwrap_or("")
        .to_string()
}

fn details_of(d: Node) -> Details {
    let type_name = d.attribute((NS_XSI, "type")).map(|t| qname(d, t));
    let simple = !d.children().any(|n| n.is_element());
    Details {
        type_name,
        text: if simple { text(d) } else { None },
    }
}

fn qname(n: Node, value: &str) -> String {
    let Some((prefix, local)) = value.split_once(':') else {
        return value.to_string();
    };
    let usual = match n.lookup_namespace_uri(Some(prefix)) {
        Some(NS_CORE) => "v8",
        Some(NS_CFG) => "cfg",
        Some(NS_XS) => "xs",
        _ => return value.to_string(),
    };
    format!("{usual}:{local}")
}

fn rows_of(item: Node) -> Result<(u32, u32), SheetError> {
    let from = number(
        child(item, "index").ok_or(SheetError::Missing("rowsItem index"))?,
        "rowsItem index",
    )?;
    let to = match child(item, "indexTo") {
        Some(n) => number(n, "rowsItem indexTo")?,
        None => from,
    };
    if to < from {
        return Err(SheetError::Bad {
            field: "rowsItem indexTo",
            value: to.to_string(),
        });
    }
    Ok((from, to))
}

fn is_sheet(n: Node, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(NS_SHEET) && n.tag_name().name() == local
}

fn child<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is_sheet(*c, local))
}

fn child_any<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == local)
}

fn text(n: Node) -> Option<String> {
    n.text().filter(|t| !t.is_empty()).map(str::to_string)
}

fn number(n: Node, field: &'static str) -> Result<u32, SheetError> {
    let t = n.text().unwrap_or("");
    crate::answer::unsigned(t).ok_or_else(|| SheetError::Bad {
        field,
        value: t.to_string(),
    })
}
