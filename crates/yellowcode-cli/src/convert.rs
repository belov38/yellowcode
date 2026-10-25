use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::convert::{self, Format, Layout, Request};

#[derive(clap::Args)]
pub(crate) struct Args {
    source: PathBuf,
    #[arg(long, value_enum)]
    to: Format,
    #[arg(long)]
    output: PathBuf,
    /// Explicit DT users layout; automatic selection awaits D4b.
    #[arg(long, value_enum)]
    users_layout: Option<Layout>,
    /// DT to 1CD only: managed memory budget, auto or integer bytes with B/KiB/MiB/GiB suffix
    /// (as for query and serve; YELLOWCODE_MEMORY_LIMIT).
    #[arg(long, allow_hyphen_values = true)]
    memory_limit: Option<String>,
    /// DT to 1CD only: restore without users (needed when they cannot be transferred as they are).
    /// The base then opens without login, with full rights.
    #[arg(long)]
    without_users: bool,
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        Request {
            source: a.source,
            to: a.to,
            output: a.output,
            users_layout: a.users_layout,
            memory_limit: a.memory_limit,
            without_users: a.without_users,
        }
    }
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<i32> {
    convert::run(args.into(), out, err)
}
