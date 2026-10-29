use std::{io::Write, path::PathBuf};
use yellowcode_core::users;

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

pub(crate) const EXAMPLES: &str = "\
Примеры:
  # Список пользователей
  yellowcode users list base

  # Список в JSON-файл
  yellowcode users list base --format json > users.json

  # Карточка пользователя по имени (регистр не важен)
  yellowcode users show base Администратор

  # Карточка по UUID, если имена повторяются в разных областях данных
  yellowcode users show base 11111111-1111-1111-1111-111111111111";

#[derive(clap::Args)]
pub(crate) struct Base {
    #[arg(
        help = "Каталог информационной базы или её файл 1Cv8.1CD (открывается только для чтения)"
    )]
    base: PathBuf,
    #[arg(
        long,
        value_enum,
        help = "Формат вывода; по умолчанию текст в терминале и JSON при перенаправлении"
    )]
    format: Option<Format>,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Список пользователей базы без служебной строки и секретов",
        after_help = EXAMPLES
    )]
    List(Base),
    #[command(
        about = "Карточка пользователя по имени или UUID; UUID нужен, если имена повторяются в разных областях данных",
        after_help = EXAMPLES
    )]
    Show {
        #[command(flatten)]
        base: Base,
        #[arg(help = "Имя или UUID пользователя")]
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
