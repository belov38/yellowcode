use std::{
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};
use yellowcode_core::service::{
    memory,
    query::{self, Format, Request, Source},
};

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["text", "file", "stdin"])))]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    /// Infobase directory or its 1Cv8.1CD file (opened read-only).
    base: PathBuf,
    #[arg(long)]
    text: Option<String>,
    /// UTF-8 query file; an initial UTF-8 BOM is accepted.
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
    /// Canonical parameter representation; repeat for additional parameters.
    #[arg(long = "param", value_name = "NAME=REPR")]
    params: Vec<String>,
    /// UTF-8 JSON array of {"name": "P", "repr": "42"} objects.
    #[arg(long)]
    params_file: Option<PathBuf>,
    /// Defaults to table on a terminal and JSON when redirected.
    #[arg(long, value_enum)]
    format: Option<Format>,
    /// Cooperative execution timeout in seconds; opening the base is outside this limit.
    #[arg(long, default_value = query::DEFAULT_TIMEOUT, value_parser = query::timeout)]
    timeout: Duration,
    /// Output rows per result; engine memory and row limits still apply.
    #[arg(long, default_value = query::DEFAULT_MAX_ROWS, value_parser = query::row_limit)]
    max_rows: usize,
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        Request {
            memory: a.memory,
            base: a.base,
            source: match (a.text, a.file, a.stdin) {
                (Some(text), _, _) => Source::Text(text),
                (None, Some(file), _) => Source::File(file),
                (None, None, _) => Source::Stdin,
            },
            params: a.params,
            params_file: a.params_file,
            format: a.format,
            timeout: a.timeout,
            max_rows: a.max_rows,
        }
    }
}

pub(crate) fn run(
    args: Args,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<i32> {
    let request = Request::from(args);
    query::run(request, input, out, err, tty)
}
