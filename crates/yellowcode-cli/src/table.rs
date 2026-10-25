use std::{io::Write, path::PathBuf};
use yellowcode_core::service::{
    memory,
    query::Format,
    table::{self, Bound, IndexSpec, Order, Request, RowFilter},
};

#[derive(clap::Args)]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    /// Infobase directory or its 1Cv8.1CD file (opened read-only).
    base: PathBuf,
    /// Metadata kind, for example Document or AccumulationRegister.
    kind: String,
    /// Metadata object name, for example РеализацияТоваров.
    name: String,
    /// Read through this index in key order; without it the table is scanned in storage order.
    #[arg(long)]
    index: Option<String>,
    /// Equality condition on the next index field; repeat in index field order.
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true
    )]
    eq: Vec<String>,
    /// Lower bound (exclusive) on the index field after the equalities.
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        conflicts_with = "gte",
        allow_hyphen_values = true
    )]
    gt: Option<String>,
    /// Lower bound (inclusive) on the index field after the equalities.
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true
    )]
    gte: Option<String>,
    /// Upper bound (exclusive) on the index field after the equalities.
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        conflicts_with = "lte",
        allow_hyphen_values = true
    )]
    lt: Option<String>,
    /// Upper bound (inclusive) on the index field after the equalities.
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true
    )]
    lte: Option<String>,
    /// Index key order (default asc).
    #[arg(long, value_enum, requires = "index")]
    order: Option<Order>,
    /// Keep posted (true) or unposted (false) rows; documents and document journals.
    #[arg(long, value_name = "true|false", value_parser = table::filter_value)]
    posted: Option<bool>,
    /// Keep rows marked (true) or not marked (false) for deletion, folders and items alike.
    #[arg(long, value_name = "true|false", value_parser = table::filter_value)]
    deletion_mark: Option<bool>,
    /// Rows per page.
    #[arg(long, default_value = table::DEFAULT_LIMIT, value_parser = table::page_limit)]
    limit: usize,
    /// Continue after the page that reported this cursor; valid while the 1CD file is unchanged.
    #[arg(long, allow_hyphen_values = true)]
    cursor: Option<String>,
    /// Defaults to table on a terminal and JSON when redirected.
    #[arg(long, value_enum)]
    format: Option<Format>,
}

fn bound(exclusive: Option<String>, inclusive: Option<String>) -> Option<Bound> {
    match (exclusive, inclusive) {
        (Some(value), _) => Some(Bound {
            value,
            inclusive: false,
        }),
        (None, Some(value)) => Some(Bound {
            value,
            inclusive: true,
        }),
        (None, None) => None,
    }
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        let index = a.index.map(|name| IndexSpec {
            name,
            eq: a.eq,
            lower: bound(a.gt, a.gte),
            upper: bound(a.lt, a.lte),
            order: a.order,
        });
        Request {
            memory: a.memory,
            base: a.base,
            kind: a.kind,
            name: a.name,
            index,
            limit: a.limit,
            filter: RowFilter {
                posted: a.posted,
                deletion_mark: a.deletion_mark,
            },
            cursor: a.cursor,
            format: a.format,
        }
    }
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<i32> {
    let request = Request::from(args);
    table::run(request, out, err, tty)
}
