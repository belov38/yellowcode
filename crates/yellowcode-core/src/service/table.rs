use super::query::Format;
pub use query_console::RowFilter;
use query_console::{
    DataIndexes, IndexOrder, IndexRange, TableIndexRequest, TablePage, TableReadError,
};
use std::{io::Write, path::PathBuf};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Order {
    Asc,
    Desc,
}

pub struct Bound {
    pub value: String,
    pub inclusive: bool,
}

pub struct IndexSpec {
    pub name: String,
    pub eq: Vec<String>,
    pub lower: Option<Bound>,
    pub upper: Option<Bound>,
    pub order: Option<Order>,
}

pub const DEFAULT_LIMIT: &str = "1000";

pub struct Request {
    pub memory: super::memory::Settings,
    pub base: PathBuf,
    pub kind: String,
    pub name: String,
    pub index: Option<IndexSpec>,
    pub limit: usize,
    pub filter: RowFilter,
    pub cursor: Option<String>,
    pub format: Option<Format>,
    pub list_indexes: bool,
}

impl Request {
    pub fn check(&self) -> Result<(), String> {
        page_limit(&self.limit.to_string())?;
        let filtered = self.filter.posted.is_some() || self.filter.deletion_mark.is_some();
        if self.list_indexes && (self.index.is_some() || self.cursor.is_some() || filtered) {
            return Err("listing the indexes takes no index, filter or cursor".into());
        }
        Ok(())
    }
}

pub fn filter_value(text: &str) -> Result<bool, String> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("expected true or false".into()),
    }
}

pub fn page_limit(text: &str) -> Result<usize, String> {
    match text.parse::<usize>() {
        Ok(n) if (1..=10_000).contains(&n) => Ok(n),
        _ => Err("limit must be between 1 and 10000".into()),
    }
}

#[derive(serde::Serialize)]
pub struct Rejected<'a, T: serde::Serialize> {
    pub error: &'a str,
    #[serde(rename = "availableIndexes")]
    pub available: &'a T,
}

pub fn run(
    args: Request,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> crate::Result<i32> {
    args.check()?;
    let (memory, reserve) = args.memory.resolve()?;
    super::query::recorded(
        super::control::CliMemory::new(memory, reserve),
        out,
        err,
        |out, err, memory, diagnostic| run_with_memory(args, out, err, tty, memory, diagnostic),
    )
}

fn run_with_memory(
    args: Request,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
    memory: &mut super::control::CliMemory,
    diagnostic: &mut super::memory_diagnostic::Record,
) -> crate::Result<i32> {
    if let Some(refusal) = memory.admission() {
        diagnostic.reason = refusal.reason();
        return Err(refusal.text.into());
    }
    let mut console = memory.open(&args.base)?;
    super::control::trim_heap();
    for note in &console.info().notes {
        let _ = writeln!(err, "yellowcode: schema note: {note}");
    }
    if args.list_indexes {
        return list_indexes(&console, &args, out, err, tty, memory, diagnostic);
    }
    if !console.bind_cursors_to_file()? {
        let _ = writeln!(
            err,
            "yellowcode: note: cursors are valid only within this process (file modification time unavailable)"
        );
    }
    let cursor = args.cursor.as_deref();
    let result = match args.index {
        Some(spec) => {
            let (lower, upper) = (spec.lower, spec.upper);
            let (gte, gt) = match lower {
                Some(Bound {
                    value,
                    inclusive: true,
                }) => (Some(value), None),
                Some(Bound {
                    value,
                    inclusive: false,
                }) => (None, Some(value)),
                None => (None, None),
            };
            let (lte, lt) = match upper {
                Some(Bound {
                    value,
                    inclusive: true,
                }) => (Some(value), None),
                Some(Bound {
                    value,
                    inclusive: false,
                }) => (None, Some(value)),
                None => (None, None),
            };
            let request = TableIndexRequest {
                index: spec.name,
                eq: spec.eq,
                range: IndexRange { gt, gte, lt, lte },
                order: match spec.order {
                    None | Some(Order::Asc) => IndexOrder::Asc,
                    Some(Order::Desc) => IndexOrder::Desc,
                },
            };
            console.read_index_page_filtered(
                &args.kind,
                &args.name,
                args.limit,
                cursor,
                &request,
                &args.filter,
            )
        }
        None => console.read_table_page_filtered(
            &args.kind,
            &args.name,
            args.limit,
            cursor,
            &args.filter,
        ),
    };
    let page = match result {
        Ok(page) => page,
        Err(error) => {
            diagnostic.reader = console.reader_stats();
            return refused(error, err, diagnostic);
        }
    };
    diagnostic.reason = "ok";
    let format = args
        .format
        .unwrap_or(if tty { Format::Table } else { Format::Json });
    let result = super::query_output::write_limited(
        memory.response_limit(),
        out,
        |count| encode(&page, format, count),
        |output| encode(&page, format, output),
    );
    diagnostic.reader = console.reader_stats();
    if result.is_ok()
        && !matches!(format, Format::Json)
        && let Some(next) = &page.next_cursor
    {
        let _ = writeln!(err, "yellowcode: next cursor: {next}");
    }
    super::query::finish_output(result, 0, diagnostic)
}

fn list_indexes(
    console: &query_console::Console,
    args: &Request,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
    memory: &mut super::control::CliMemory,
    diagnostic: &mut super::memory_diagnostic::Record,
) -> crate::Result<i32> {
    let listed = match console.data_indexes(&args.kind, &args.name) {
        Ok(listed) => listed,
        Err(error) => {
            diagnostic.reader = console.reader_stats();
            return refused(error, err, diagnostic);
        }
    };
    diagnostic.reason = "ok";
    let format = args
        .format
        .unwrap_or(if tty { Format::Table } else { Format::Json });
    let result = super::query_output::write_limited(
        memory.response_limit(),
        out,
        |count| encode_indexes(&listed, format, count),
        |output| encode_indexes(&listed, format, output),
    );
    diagnostic.reader = console.reader_stats();
    super::query::finish_output(result, 0, diagnostic)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexList<'a> {
    schema_version: u32,
    window_default: Option<&'a str>,
    filters: &'a query_console::DataFilters,
    indexes: &'a [query_console::AvailableIndex],
}

fn encode_indexes(listed: &DataIndexes, format: Format, out: &mut dyn Write) -> crate::Result<()> {
    use super::query_output::{line, visible_line};
    let reason = |f: &query_console::IndexField| f.reason.clone().unwrap_or_default();
    match format {
        Format::Json => super::query_output::write_json(
            out,
            &IndexList {
                schema_version: 1,
                window_default: listed.default.as_deref(),
                filters: &listed.filters,
                indexes: &listed.indexes,
            },
        )?,
        Format::Csv => {
            line(
                out,
                ["index", "position", "field", "conditions", "reason"].into_iter(),
            )?;
            for ix in &listed.indexes {
                for (at, f) in ix.fields.iter().enumerate() {
                    let (position, why) = ((at + 1).to_string(), reason(f));
                    let conditions = if f.conditions { "true" } else { "false" };
                    line(
                        out,
                        [ix.name.as_str(), &position, &f.name, conditions, &why].into_iter(),
                    )?;
                }
            }
        }
        Format::Table => {
            visible_line(out, ["index", "fields"].into_iter())?;
            for ix in &listed.indexes {
                let fields = ix
                    .fields
                    .iter()
                    .map(|f| match (&f.reason, f.conditions) {
                        (_, true) => f.name.clone(),
                        (Some(why), false) => format!("{} (no conditions: {why})", f.name),
                        (None, false) => format!("{} (no conditions)", f.name),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let name = if listed.default.as_deref() == Some(ix.name.as_str()) {
                    format!("{} (window default)", ix.name)
                } else {
                    ix.name.clone()
                };
                visible_line(out, [name.as_str(), &fields].into_iter())?;
            }
            let yes = |b: bool| if b { "yes" } else { "no" };
            writeln!(
                out,
                "({} indexes; --posted: {}, --deletion-mark: {})",
                listed.indexes.len(),
                yes(listed.filters.posted),
                yes(listed.filters.deletion_mark)
            )?;
        }
    }
    Ok(())
}

fn refused(
    error: TableReadError,
    err: &mut dyn Write,
    diagnostic: &mut super::memory_diagnostic::Record,
) -> crate::Result<i32> {
    match &error {
        TableReadError::IndexRejected {
            error,
            available_indexes,
        } => {
            let _ = super::query_output::write_json(
                err,
                &Rejected {
                    error,
                    available: available_indexes,
                },
            );
            let _ = writeln!(
                err,
                "yellowcode: hint: --list-indexes shows the indexes, their fields and which fields take conditions"
            );
            return Ok(1);
        }
        TableReadError::Memory(limit) => {
            diagnostic.reason = super::memory::Refusal::limit(*limit).reason();
        }
        _ => {}
    }
    let _ = writeln!(err, "yellowcode: {error}");
    Ok(if matches!(error, TableReadError::Invalid(_)) {
        2
    } else {
        1
    })
}

fn encode(page: &TablePage, format: Format, out: &mut dyn Write) -> crate::Result<()> {
    use super::query_output::{line, visible_line};
    let names = || page.columns.iter().map(|c| c.name.as_str());
    match format {
        Format::Json => super::query_output::write_json(out, page)?,
        Format::Csv => {
            line(out, names())?;
            for row in &page.rows {
                line(out, row.iter().map(|c| c.t.as_str()))?;
            }
        }
        Format::Table => {
            visible_line(out, names())?;
            for row in &page.rows {
                visible_line(out, row.iter().map(|c| c.t.as_str()))?;
            }
            match page.scanned {
                Some(scanned) => writeln!(out, "({} rows, {scanned} scanned)", page.rows.len())?,
                None => writeln!(out, "({} rows)", page.rows.len())?,
            }
        }
    }
    Ok(())
}
