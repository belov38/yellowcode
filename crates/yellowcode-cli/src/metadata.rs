use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::metadata::{self, Format};

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// List metadata, optionally rooted at one object.
    Tree {
        source: PathBuf,
        /// Managed DT CONFIG extraction budget; decoded model memory is additional.
        #[arg(long, allow_hyphen_values = true)]
        memory_limit: Option<String>,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        flat: bool,
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
    },
    /// Show the typed properties of a metadata object.
    Show {
        source: PathBuf,
        /// Managed DT CONFIG extraction budget; decoded model memory is additional.
        #[arg(long, allow_hyphen_values = true)]
        memory_limit: Option<String>,
        object: String,
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
    },
}

impl From<Command> for metadata::Command {
    fn from(c: Command) -> metadata::Command {
        match c {
            Command::Tree {
                memory_limit: _,
                source,
                depth,
                path,
                kind,
                flat,
                format,
            } => metadata::Command::Tree {
                source,
                depth,
                path,
                kind,
                flat,
                format,
            },
            Command::Show {
                memory_limit: _,
                source,
                object,
                format,
            } => metadata::Command::Show {
                source,
                object,
                format,
            },
        }
    }
}

pub(crate) fn run(command: Command, out: &mut dyn Write) -> yellowcode_core::Result<()> {
    let limit = match &command {
        Command::Tree { memory_limit, .. } | Command::Show { memory_limit, .. } => {
            memory_limit.clone()
        }
    };
    metadata::run_with_memory_limit(command.into(), limit, out)
}
