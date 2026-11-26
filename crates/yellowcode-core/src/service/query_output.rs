use super::query::Format;
use query_console::{Cell, CellKind, Column, ErrorOut, Report, Stmt};
use serde::Serialize;
use std::io::Write;

#[derive(Clone, Copy)]
struct NestedTable<'a> {
    columns: &'a [Column],
    total: u64,
    kept: usize,
    truncated: bool,
    rows: &'a [Vec<Cell>],
}
impl Serialize for NestedTable<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("NestedTable", 5)?;
        state.serialize_field("columns", self.columns)?;
        state.serialize_field("total", &self.total)?;
        state.serialize_field("kept", &self.kept)?;
        state.serialize_field("truncated", &self.truncated)?;
        state.serialize_field("rows", self.rows)?;
        state.end()
    }
}

fn nested_table(report: &Report, stmt: usize, row: usize, col: usize) -> Option<NestedTable<'_>> {
    let nested = report.rows.nested(stmt, row, col)?;
    let total = report.rows.nested_total(stmt, row, col)?;
    let kept = nested.rows.len();
    Some(NestedTable {
        columns: &nested.columns,
        total,
        kept,
        truncated: (kept as u64) < total,
        rows: &nested.rows,
    })
}

#[derive(Clone, Copy)]
struct CellView<'a> {
    cell: &'a Cell,
    nested: Option<NestedTable<'a>>,
}
impl Serialize for CellView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let len = 2 + usize::from(self.cell.r.is_some()) + usize::from(self.nested.is_some());
        let mut state = serializer.serialize_struct("Cell", len)?;
        state.serialize_field("k", &self.cell.k)?;
        state.serialize_field("t", &self.cell.t)?;
        if let Some(ref_out) = &self.cell.r {
            state.serialize_field("r", ref_out)?;
        }
        if let Some(nested) = &self.nested {
            state.serialize_field("nested", nested)?;
        }
        state.end()
    }
}

#[derive(Clone, Copy)]
struct RowView<'a> {
    report: &'a Report,
    stmt: usize,
    row_index: usize,
    cells: &'a [Cell],
}
impl Serialize for RowView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.cells.len()))?;
        for (col_index, cell) in self.cells.iter().enumerate() {
            let nested = if cell.k == CellKind::Nested {
                nested_table(self.report, self.stmt, self.row_index, col_index)
            } else {
                None
            };
            seq.serialize_element(&CellView { cell, nested })?;
        }
        seq.end()
    }
}

#[derive(Clone, Copy)]
struct RowsView<'a> {
    report: &'a Report,
    stmt: usize,
    rows: &'a [Vec<Cell>],
}
impl Serialize for RowsView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.rows.len()))?;
        for (row_index, cells) in self.rows.iter().enumerate() {
            seq.serialize_element(&RowView {
                report: self.report,
                stmt: self.stmt,
                row_index,
                cells,
            })?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Statement<'a> {
    Result {
        columns: &'a [Column],
        total: u64,
        kept: usize,
        truncated: bool,
        ordered: bool,
        rows: RowsView<'a>,
    },
    Count {
        name: &'a str,
        n: u64,
    },
    Dropped,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    schema_version: u32,
    statements: Statements<'a>,
    elapsed_ms: u64,
    error: Option<&'a ErrorOut>,
    cancelled: bool,
    timed_out: bool,
}

struct CountingWriter {
    bytes: u64,
    limit: u64,
    exceeded: bool,
}
impl CountingWriter {
    fn new(limit: u64) -> Self {
        Self {
            bytes: 0,
            limit,
            exceeded: false,
        }
    }
}
impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(next) = self.bytes.checked_add(bytes.len() as u64) {
            self.bytes = next;
            self.exceeded |= next > self.limit;
        } else {
            self.exceeded = true;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Statements<'a> {
    report: &'a Report,
    max_rows: usize,
}
impl<'a> Statements<'a> {
    fn iter(self) -> impl Iterator<Item = Statement<'a>> {
        self.report
            .statements()
            .iter()
            .enumerate()
            .map(move |(index, stmt)| match stmt {
                Stmt::Result {
                    columns,
                    total,
                    kept,
                    truncated,
                    ordered,
                } => {
                    let rows = self.report.rows.slice(index, 0, self.max_rows);
                    Statement::Result {
                        columns,
                        total: *total,
                        kept: rows.len(),
                        truncated: *truncated || (rows.len() as u64) < *kept,
                        ordered: *ordered,
                        rows: RowsView {
                            report: self.report,
                            stmt: index,
                            rows,
                        },
                    }
                }
                Stmt::Count { name, n } => Statement::Count { name, n: *n },
                Stmt::Dropped => Statement::Dropped,
            })
    }
}
impl Serialize for Statements<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.report.statements().len()))?;
        for stmt in self.iter() {
            seq.serialize_element(&stmt)?;
        }
        seq.end()
    }
}

fn field(out: &mut dyn Write, text: &str) -> std::io::Result<()> {
    if text.contains([',', '"', '\r', '\n']) {
        out.write_all(b"\"")?;
        for (index, part) in text.split('"').enumerate() {
            if index != 0 {
                out.write_all(b"\"\"")?;
            }
            out.write_all(part.as_bytes())?;
        }
        out.write_all(b"\"")
    } else {
        out.write_all(text.as_bytes())
    }
}
pub fn line<'a>(out: &mut dyn Write, fields: impl Iterator<Item = &'a str>) -> std::io::Result<()> {
    for (index, text) in fields.enumerate() {
        if index > 0 {
            out.write_all(b",")?;
        }
        field(out, text)?;
    }
    out.write_all(b"\r\n")
}
fn write_visible(out: &mut dyn Write, text: &str) -> std::io::Result<()> {
    for ch in text.chars() {
        match ch {
            '\n' => out.write_all(b"\\n")?,
            '\r' => out.write_all(b"\\r")?,
            '\t' => out.write_all(b"\\t")?,
            c if c.is_control() => {
                for escaped in c.escape_default() {
                    let mut bytes = [0; 4];
                    out.write_all(escaped.encode_utf8(&mut bytes).as_bytes())?;
                }
            }
            c => {
                let mut bytes = [0; 4];
                out.write_all(c.encode_utf8(&mut bytes).as_bytes())?;
            }
        }
    }
    Ok(())
}
pub fn visible_line<'a>(
    out: &mut dyn Write,
    fields: impl Iterator<Item = &'a str>,
) -> std::io::Result<()> {
    for (index, text) in fields.enumerate() {
        if index > 0 {
            out.write_all(b" | ")?;
        }
        write_visible(out, text)?;
    }
    out.write_all(b"\n")
}

fn write_nested_visible(out: &mut dyn Write, nested: NestedTable<'_>) -> std::io::Result<()> {
    out.write_all(b"[")?;
    for (row_index, row) in nested.rows.iter().enumerate() {
        if row_index > 0 {
            out.write_all(b", ")?;
        }
        out.write_all(b"(")?;
        for (col_index, cell) in row.iter().enumerate() {
            if col_index > 0 {
                out.write_all(b", ")?;
            }
            write_visible(out, &cell.t)?;
        }
        out.write_all(b")")?;
    }
    if nested.truncated {
        if !nested.rows.is_empty() {
            out.write_all(b", ")?;
        }
        write!(
            out,
            "... ({} of {} rows, truncated)",
            nested.kept, nested.total
        )?;
    }
    out.write_all(b"]")
}

fn validate_nested_table(nested: Option<NestedTable<'_>>) -> crate::Result<()> {
    let Some(nested) = nested else {
        return Err(
            "nested tabular result completeness is unavailable in public query output".into(),
        );
    };
    if nested.total < nested.kept as u64 {
        return Err(
            "nested tabular result completeness is inconsistent in public query output".into(),
        );
    }
    if nested
        .columns
        .iter()
        .any(|column| column.kind == CellKind::Nested)
        || nested
            .rows
            .iter()
            .flatten()
            .any(|inner| inner.k == CellKind::Nested)
    {
        return Err(
            "multi-level nested tabular results are unsupported in public query output".into(),
        );
    }
    Ok(())
}

fn has_nested_column_or_cell(columns: &[Column], rows: &[Vec<Cell>]) -> bool {
    columns.iter().any(|column| column.kind == CellKind::Nested)
        || rows.iter().flatten().any(|cell| cell.k == CellKind::Nested)
}

pub fn write_report(
    report: &Report,
    max_rows: usize,
    timed_out: bool,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    if matches!(format, Format::Csv) {
        if report.error().is_none() && !matches!(report.statements(), [Stmt::Result { .. }]) {
            return Err(
                "CSV requires exactly one tabular result and no other batch statements".into(),
            );
        }
        for (index, statement) in report.statements().iter().enumerate() {
            if let Stmt::Result { columns, .. } = statement
                && has_nested_column_or_cell(
                    columns,
                    report.rows.slice(index, 0, report.rows.kept(index)),
                )
            {
                return Err(
                    "CSV does not support nested tabular results; use JSON or table format".into(),
                );
            }
        }
    } else {
        for (stmt_index, statement) in report.statements().iter().enumerate() {
            if !matches!(statement, Stmt::Result { .. }) {
                continue;
            }
            for (row_index, row) in report
                .rows
                .slice(stmt_index, 0, max_rows)
                .iter()
                .enumerate()
            {
                for (col_index, cell) in row.iter().enumerate() {
                    if cell.k != CellKind::Nested {
                        continue;
                    }
                    validate_nested_table(nested_table(report, stmt_index, row_index, col_index))?;
                }
            }
        }
    }
    let statements = Statements { report, max_rows };
    for (index, stmt) in statements.iter().enumerate() {
        if let Statement::Result {
            total,
            kept,
            truncated,
            rows,
            ..
        } = stmt
        {
            if truncated {
                let _ = writeln!(
                    err,
                    "yellowcode: statement {} truncated: {} of {total} rows emitted",
                    index + 1,
                    kept
                );
            }
            for (row_index, row) in rows.rows.iter().enumerate() {
                for (col_index, cell) in row.iter().enumerate() {
                    if cell.k == CellKind::Nested
                        && let Some(nested) = nested_table(report, index, row_index, col_index)
                        && nested.truncated
                    {
                        let _ = writeln!(
                            err,
                            "yellowcode: statement {} row {} col {} nested table truncated: {} of {} rows emitted",
                            index + 1,
                            row_index + 1,
                            col_index + 1,
                            nested.kept,
                            nested.total,
                        );
                    }
                }
            }
        }
    }
    encode_report(report, statements, timed_out, format, out)
}

#[cfg(test)]
std::thread_local! {
    static AFTER_PREFLIGHT: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

pub fn write_report_limited(
    report: &Report,
    max_rows: usize,
    timed_out: bool,
    format: Format,
    limit: u64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    write_limited(
        limit,
        out,
        |count| write_report(report, max_rows, timed_out, format, count, err),
        |output| {
            encode_report(
                report,
                Statements { report, max_rows },
                timed_out,
                format,
                output,
            )
        },
    )
}

pub fn write_limited(
    limit: u64,
    out: &mut dyn Write,
    preflight: impl FnOnce(&mut dyn Write) -> crate::Result<()>,
    encode: impl FnOnce(&mut dyn Write) -> crate::Result<()>,
) -> crate::Result<()> {
    let mut count = CountingWriter::new(limit);
    preflight(&mut count)?;
    if count.exceeded {
        return Err(memory_budget::ResponseLimit.into());
    }
    #[cfg(test)]
    AFTER_PREFLIGHT.with(|slot| {
        if let Some(hook) = slot.get() {
            hook();
        }
    });
    let mut output = OutputSink {
        output: out,
        error: None,
    };
    encode(&mut output)?;
    output.flush()?;
    match output.error {
        None => Ok(()),
        Some(std::io::ErrorKind::BrokenPipe) => Err(memory_budget::OutputClosed.into()),
        Some(std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {
            Err(memory_budget::OutputDisconnected.into())
        }
        Some(std::io::ErrorKind::TimedOut) => Err(memory_budget::OutputTimedOut.into()),
        Some(_) => Err(memory_budget::OutputFailed.into()),
    }
}

pub fn write_json(out: &mut dyn Write, value: &impl Serialize) -> crate::Result<()> {
    serde_json::to_writer(&mut *out, value).map_err(|e| match e.io_error_kind() {
        Some(kind) => crate::Error::from(std::io::Error::new(kind, e)),
        None => e.into(),
    })?;
    writeln!(out)?;
    Ok(())
}

struct OutputSink<'a> {
    output: &'a mut dyn Write,
    error: Option<std::io::ErrorKind>,
}
impl Write for OutputSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.error.is_some() {
            return Ok(bytes.len());
        }
        loop {
            match self.output.write(bytes) {
                Ok(0) if !bytes.is_empty() => {
                    self.error = Some(std::io::ErrorKind::WriteZero);
                    return Ok(bytes.len());
                }
                Ok(written) => return Ok(written),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.error = Some(error.kind());
                    return Ok(bytes.len());
                }
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.error.is_none()
            && let Err(error) = self.output.flush()
        {
            self.error = Some(error.kind());
        }
        Ok(())
    }
}

fn encode_report(
    report: &Report,
    statements: Statements<'_>,
    timed_out: bool,
    format: Format,
    out: &mut dyn Write,
) -> crate::Result<()> {
    if matches!(format, Format::Json) {
        let output = Output {
            schema_version: 1,
            statements,
            elapsed_ms: report.elapsed_ms,
            error: report.error(),
            cancelled: report.cancelled,
            timed_out,
        };
        write_json(out, &output)?;
    } else if report.error().is_none() {
        for statement in statements.iter() {
            match statement {
                Statement::Result { columns, rows, .. } if matches!(format, Format::Csv) => {
                    line(out, columns.iter().map(|c| c.name.as_str()))?;
                    for row in rows.rows {
                        line(out, row.iter().map(|c| c.t.as_str()))?;
                    }
                }
                Statement::Result { columns, rows, .. } => {
                    visible_line(out, columns.iter().map(|c| c.name.as_str()))?;
                    for (row_index, row) in rows.rows.iter().enumerate() {
                        for (col_index, cell) in row.iter().enumerate() {
                            if col_index > 0 {
                                out.write_all(b" | ")?;
                            }
                            if cell.k == CellKind::Nested
                                && let Some(nested) =
                                    nested_table(rows.report, rows.stmt, row_index, col_index)
                            {
                                write_nested_visible(out, nested)?;
                            } else {
                                write_visible(out, &cell.t)?;
                            }
                        }
                        out.write_all(b"\n")?;
                    }
                    writeln!(out, "({} rows)", rows.rows.len())?;
                }
                Statement::Count { name, n } => {
                    write_visible(out, name)?;
                    writeln!(out, ": {n} rows")?;
                }
                Statement::Dropped => {
                    writeln!(out, "temporary table dropped")?;
                }
            }
        }
    }
    Ok(())
}
