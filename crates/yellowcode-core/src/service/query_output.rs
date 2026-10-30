use super::query::Format;
use query_console::{Cell, CellKind, Column, ErrorOut, Report, Stmt};
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Statement<'a> {
    Result {
        columns: &'a [Column],
        total: u64,
        kept: usize,
        truncated: bool,
        ordered: bool,
        rows: &'a [Vec<Cell>],
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
                        rows,
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

pub fn write_report(
    report: &Report,
    max_rows: usize,
    timed_out: bool,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    for (index, statement) in report.statements().iter().enumerate() {
        if let Stmt::Result { columns, .. } = statement
            && (columns.iter().any(|column| column.kind == CellKind::Nested)
                || report
                    .rows
                    .slice(index, 0, report.rows.kept(index))
                    .iter()
                    .flatten()
                    .any(|cell| cell.k == CellKind::Nested))
        {
            return Err("nested tabular results are unsupported in public query output (completeness is unavailable)".into());
        }
    }
    if matches!(format, Format::Csv)
        && report.error().is_none()
        && !matches!(report.statements(), [Stmt::Result { .. }])
    {
        return Err("CSV requires exactly one tabular result and no other batch statements".into());
    }
    let statements = Statements { report, max_rows };
    for (index, stmt) in statements.iter().enumerate() {
        if let Statement::Result {
            total,
            kept,
            truncated: true,
            ..
        } = stmt
        {
            let _ = writeln!(
                err,
                "yellowcode: statement {} truncated: {} of {total} rows emitted",
                index + 1,
                kept
            );
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
                    for row in rows {
                        line(out, row.iter().map(|c| c.t.as_str()))?;
                    }
                }
                Statement::Result { columns, rows, .. } => {
                    visible_line(out, columns.iter().map(|c| c.name.as_str()))?;
                    for row in rows {
                        visible_line(out, row.iter().map(|c| c.t.as_str()))?;
                    }
                    writeln!(out, "({} rows)", rows.len())?;
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
