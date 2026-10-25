use std::{io::Write, path::PathBuf};
use yellowcode_core::users;

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(clap::Args)]
pub(crate) struct Base {
    /// Infobase directory or its 1Cv8.1CD file (opened read-only).
    base: PathBuf,
    /// Defaults to text on a terminal and JSON when redirected.
    #[arg(long, value_enum)]
    format: Option<Format>,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// List infobase users without the service row or secrets.
    List(Base),
    /// Show a user by name or UUID; use UUID when names repeat across data areas.
    Show {
        #[command(flatten)]
        base: Base,
        /// User name or UUID.
        name: String,
    },
}

pub(crate) fn run(command: Command, out: &mut dyn Write, tty: bool) -> yellowcode_core::Result<()> {
    let (base, name) = match command {
        Command::List(base) => (base, None),
        Command::Show { base, name } => (base, Some(name)),
    };
    let cards = users::list(&base.base)?;
    let card = name
        .as_deref()
        .map(|name| users::find(&cards, name))
        .transpose()?;
    let json = matches!(
        base.format
            .unwrap_or(if tty { Format::Text } else { Format::Json }),
        Format::Json
    );
    if json {
        if let Some(card) = card {
            users::write_json(out, card)?;
        } else {
            users::write_json(out, &cards)?;
        }
    } else if let Some(card) = card {
        users::write_card(out, card)?;
    } else {
        users::write_list(out, &cards)?;
    }
    Ok(())
}
