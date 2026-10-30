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
use clap::{CommandFactory, Parser, Subcommand};
use std::{
    ffi::OsString,
    io::{Read, Write},
};

mod bak;
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
        "help" | "metadata" | "convert" | "bak" | "query" | "serve" | "table" | "users" => true,
        flag if flag.starts_with('-') => true,
        _ => {
            let path = std::path::Path::new(arg);
            !(path.extension().is_some() || path.components().count() > 1 || path.exists())
                || gui_has_cli_flag(args.get(2..).unwrap_or(&[]))
        }
    }
}

const EXAMPLES: &str = "\
Примеры:
  # Выполнить запрос к файловой базе (каталог с 1Cv8.1CD)
  yellowcode query base --text \"ВЫБРАТЬ ПЕРВЫЕ 10 Наименование ИЗ Справочник.Контрагенты\"

  # Прочитать первую страницу таблицы документа
  yellowcode table base Документ Счет --limit 100

  # Показать справочники и документы конфигурации
  yellowcode metadata tree configuration.cf --kind Справочник,Документ --depth 2 --format text

  # Сохранить конфигурацию из DT в CF
  yellowcode convert backup.dt --to cf --output configuration.cf

  # Список пользователей базы
  yellowcode users list base

  # HTTP-сервис запросов на 127.0.0.1:8080
  yellowcode serve base

Справка по команде: yellowcode <команда> --help";

#[derive(Parser)]
#[command(
    name = "yellowcode",
    version = VERSION,
    about = "Yellowcode: конфигурации и информационные базы 1С",
    after_help = EXAMPLES
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "BUILD",
        default_value = yellowcode_core::platform::DEFAULT_BUILD,
        help = "Точная версия эмулируемой платформы (не режим совместимости конфигурации)"
    )]
    platform_build: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(subcommand, about = "Исследовать резервную копию MS SQL Server")]
    Bak(bak::Command),
    #[command(
        about = "Преобразовать конфигурацию или выгрузить информационную базу; исходный файл не меняется"
    )]
    Convert(convert::Args),
    #[command(about = "Открыть конфигурацию или информационную базу в установленном приложении")]
    Open(OpenArgs),
    #[command(about = "Выполнить запрос к информационной базе (только чтение)")]
    Query(query::Args),
    #[command(about = "HTTP-сервис запросов и постраничного чтения таблиц (только чтение)")]
    Serve(serve::Args),
    #[command(about = "Прочитать одну страницу таблицы базы, при необходимости по индексу")]
    Table(table::Args),
    #[command(
        subcommand,
        about = "Пользователи информационной базы и их карточки (без паролей и других секретов)",
        after_help = users::EXAMPLES
    )]
    Users(users::Command),
    #[command(
        subcommand,
        about = "Метаданные конфигурации",
        after_help = metadata::EXAMPLES
    )]
    Metadata(metadata::Command),
}

#[derive(clap::Args)]
#[command(after_help = "\
Примеры:
  # Открыть конфигурацию
  yellowcode open configuration.cf

  # Открыть файловую базу с английским интерфейсом
  yellowcode open base --lang en

  # Открыть DT, выделив 3 ГиБ на чтение конфигурации
  yellowcode open backup.dt --memory-limit 3GiB")]
struct OpenArgs {
    #[arg(help = "Что открыть: CF, DT, файл 1Cv8.1CD или каталог базы")]
    source: Option<std::path::PathBuf>,
    #[arg(long, value_parser = ["ru", "en"], help = "Язык интерфейса")]
    lang: Option<String>,
    #[arg(
        long,
        help = "Готовая выгрузка конфигурации в XML для проверки модулей; без параметра приложение делает её само"
    )]
    names: Option<std::path::PathBuf>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
    )]
    memory_limit: Option<String>,
    #[arg(long, help = "Проверочный запуск: открыть источник и выйти")]
    smoke: bool,
}

pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> i32 {
    let args: Vec<OsString> = args.into_iter().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                let partial = Cli::command()
                    .ignore_errors(true)
                    .try_get_matches_from(&args);
                if partial.ok().is_some_and(|matches| {
                    matches.subcommand_matches("users").is_some_and(|users| {
                        matches!(
                            users.subcommand_name(),
                            Some("create" | "edit" | "delete" | "clear-password")
                        )
                    })
                }) {
                    users::write_notice(err);
                }
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return error.exit_code();
        }
    };
    let user_mutation = matches!(
        &cli.command,
        Some(Command::Users(command)) if command.is_mutation()
    );
    if user_mutation {
        users::write_notice(err);
    }
    if let Err(error) = yellowcode_core::platform::select(&cli.platform_build) {
        let _ = writeln!(err, "yellowcode: {error}");
        return 2;
    }
    let mut output = yellowcode_core::output::Out {
        w: out,
        closed: false,
    };
    let result = match cli.command {
        Some(Command::Bak(command)) => bak::run(command, &mut output, err).map(|()| 0),
        Some(Command::Users(command)) => users::run(command, &mut output, tty).map(|()| 0),
        Some(Command::Metadata(command)) => metadata::run(command, &mut output).map(|()| 0),
        Some(Command::Convert(args)) => convert::run(args, &mut output, err),
        Some(Command::Open(_)) => Err("open requires the installed desktop executable".into()),
        Some(Command::Query(args)) => query::run(args, input, &mut output, err, tty),
        Some(Command::Serve(args)) => serve::run(args, cli.platform_build, err).map(|()| 0),
        Some(Command::Table(args)) => table::run(args, &mut output, err, tty),
        None => Cli::command()
            .write_long_help(&mut output)
            .map(|()| 0)
            .map_err(Into::into),
    }
    .and_then(|code| match output.flush() {
        Err(_) if user_mutation => Err(users::committed_output_error()),
        Err(_) if output.closed => Ok(code),
        Err(error) => Err(error.into()),
        Ok(()) => Ok(code),
    });
    match result {
        Ok(code) => code,
        Err(_) if output.closed && !user_mutation => 0,
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
