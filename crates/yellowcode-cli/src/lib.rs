#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
use clap::{Parser, Subcommand};
use std::{
    ffi::OsString,
    io::{Read, Write},
};

mod convert;
mod metadata;
mod query;
mod serve;
mod table;
mod users;

pub const VERSION: &str = yellowcode_core::VERSION;

fn gui_has_cli_flag(args: &[OsString]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "--lang" | "--names" | "--memory-limit" => {
                args.next();
            }
            "--smoke" => {}
            option if option.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}

pub fn is_cli(args: &[OsString]) -> bool {
    let mut filtered = Vec::new();
    let mut tail = args.iter();
    if let Some(program) = tail.next() {
        filtered.push(program.clone());
    }
    while let Some(arg) = tail.next() {
        if arg == "--lang" || arg == "--names" || arg == "--memory-limit" {
            filtered.push(arg.clone());
            if let Some(value) = tail.next() {
                filtered.push(value.clone());
            }
        } else if arg == "--platform-build" {
            if tail.next().is_none() {
                return true;
            }
        } else if !arg.to_string_lossy().starts_with("--platform-build=") {
            filtered.push(arg.clone());
        }
    }
    let args = filtered.as_slice();
    let Some(arg) = args.get(1) else {
        return false;
    };
    let text = arg.to_string_lossy();
    match text.as_ref() {
        "--lang" | "--names" | "--memory-limit" | "--smoke" => {
            gui_has_cli_flag(args.get(1..).unwrap_or(&[]))
        }
        "open" => gui_has_cli_flag(args.get(2..).unwrap_or(&[])),
        "help" | "metadata" | "convert" | "query" | "serve" | "table" | "users" => true,
        flag if flag.starts_with('-') => true,
        _ => {
            let path = std::path::Path::new(arg);
            !(path.extension().is_some() || path.components().count() > 1 || path.exists())
                || gui_has_cli_flag(args.get(2..).unwrap_or(&[]))
        }
    }
}

#[derive(Parser)]
#[command(name = "yellowcode", version = VERSION, about = "Yellowcode configuration and infobase tools")]
struct Cli {
    /// Exact emulated platform build (not the configuration compatibility mode).
    #[arg(long, global = true, value_name = "BUILD", default_value = yellowcode_core::platform::DEFAULT_BUILD)]
    platform_build: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a configuration or export an infobase without changing the source.
    Convert(convert::Args),
    /// Open a configuration or infobase in the installed desktop application.
    Open(OpenArgs),
    /// Execute a read-only query against an infobase.
    Query(query::Args),
    /// Serve read-only queries and paged stored tables over HTTP.
    Serve(serve::Args),
    /// Read one page of a stored table, optionally through an index.
    Table(table::Args),
    /// Read infobase users and their cards without secrets.
    #[command(subcommand)]
    Users(users::Command),
    /// Inspect configuration metadata.
    #[command(subcommand)]
    Metadata(metadata::Command),
}

#[derive(clap::Args)]
struct OpenArgs {
    /// Configuration or infobase source to open.
    source: Option<std::path::PathBuf>,
    #[arg(long, value_parser = ["ru", "en"])]
    lang: Option<String>,
    #[arg(long)]
    names: Option<std::path::PathBuf>,
    /// Managed DT CONFIG extraction budget; decoded model memory is additional.
    #[arg(long, allow_hyphen_values = true)]
    memory_limit: Option<String>,
    #[arg(long)]
    smoke: bool,
}

pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return error.exit_code();
        }
    };
    if let Err(error) = yellowcode_core::platform::select(&cli.platform_build) {
        let _ = writeln!(err, "yellowcode: {error}");
        return 2;
    }
    let mut output = yellowcode_core::output::Out {
        w: out,
        closed: false,
    };
    let result = match cli.command {
        Some(Command::Users(command)) => users::run(command, &mut output, tty).map(|()| 0),
        Some(Command::Metadata(command)) => metadata::run(command, &mut output).map(|()| 0),
        Some(Command::Convert(args)) => convert::run(args, &mut output, err),
        Some(Command::Open(_)) => Err("open requires the installed desktop executable".into()),
        Some(Command::Query(args)) => query::run(args, input, &mut output, err, tty),
        Some(Command::Serve(args)) => serve::run(args, cli.platform_build, err).map(|()| 0),
        Some(Command::Table(args)) => table::run(args, &mut output, err, tty),
        None => {
            use clap::CommandFactory;
            Cli::command()
                .write_long_help(&mut output)
                .map(|()| 0)
                .map_err(Into::into)
        }
    }
    .and_then(|code| match output.flush() {
        Err(_) if output.closed => Ok(code),
        Err(error) => Err(error.into()),
        Ok(()) => Ok(code),
    });
    match result {
        Ok(code) => code,
        Err(_) if output.closed => 0,
        Err(error) => {
            let _ = writeln!(err, "yellowcode: {error}");
            if error
                .downcast_ref::<memory_budget::policy::ConfigError>()
                .is_some()
            {
                2
            } else {
                1
            }
        }
    }
}
