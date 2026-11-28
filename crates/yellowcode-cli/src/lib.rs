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
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, parser::ValueSource};
use std::{
    ffi::OsString,
    io::{Read, Write},
};

mod bak;
mod bak_report;
mod config;
mod convert;
pub mod env;
mod ext;
mod ib;
mod open;
mod progress;
mod readonly;
mod remote;
mod scope;

pub const VERSION: &str = yellowcode_core::VERSION;

const EXAMPLES: &str = "\
Примеры:
  # Показать справочники и документы конфигурации
  yellowcode config metadata tree configuration.cf --kind Справочник,Документ --depth 2 --format text

  # Перехваты методов в расширении
  yellowcode ext intercepts extension.cfe --format text

  # Выполнить запрос к файловой базе (каталог с 1Cv8.1CD)
  yellowcode ib query base --text \"ВЫБРАТЬ ПЕРВЫЕ 10 Наименование ИЗ Справочник.Контрагенты\"

  # Прочитать первую страницу таблицы документа
  yellowcode ib table base Документ Счет --limit 100

  # Сохранить конфигурацию из DT в CF
  yellowcode convert backup.dt --to cf --output configuration.cf

  # Получить CF из резервной копии MS SQL Server
  yellowcode bak convert base.bak --to cf --output configuration.cf

  # Сводка сервера 1С при входе без пользователя
  yellowcode remote info http://server/base

  # Открыть конфигурацию в окне приложения
  yellowcode open configuration.cf

Коды выхода: 0 — готово, 1 — ошибка выполнения, 2 — ошибка в аргументах, 3 — у remote create, write, form:
запрос отправлен, итог неизвестен, 4 — отказ по YELLOWCODE_READONLY (команда меняет данные, ничего не отправлено);
у yellowcode config grep, как у grep, 1 — нет совпадений, 2 — ошибка.

Справка по команде: yellowcode <команда> --help";

#[derive(Parser)]
#[command(
    name = "yellowcode",
    version = VERSION,
    about = "Yellowcode: конфигурации и информационные базы 1С"
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
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "MODE",
        help = "Ход работы в stderr: auto — строка на терминале, always — строки для журнала, never — без него \
                (по умолчанию переменная YELLOWCODE_PROGRESS, иначе auto)"
    )]
    progress: Option<progress::Mode>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Открыть источник в окне приложения (yellowcode-desktop)")]
    Open(open::OpenArgs),
    #[command(
        subcommand,
        about = "Конфигурация из CF, DT, 1CD или базы (только чтение): метаданные, модули, макеты, поиск, \
                 выгрузка",
        after_help = config::EXAMPLES
    )]
    Config(config::Command),
    #[command(
        subcommand,
        about = "Расширение из файла CFE: свойства, объекты, модули, перехваты; только чтение",
        after_help = ext::EXAMPLES
    )]
    Ext(ext::Command),
    #[command(
        subcommand,
        about = "Данные файловой информационной базы: запросы, таблицы, пользователи, HTTP-сервис",
        after_help = ib::EXAMPLES
    )]
    Ib(ib::Command),
    #[command(about = "Преобразовать файл в другой формат; исходный файл не меняется")]
    Convert(convert::Args),
    #[command(
        subcommand,
        about = "Резервные копии MS SQL Server: осмотр, получение CF, DT, 1CD",
        after_help = bak::SUMMARY
    )]
    Bak(bak::Command),
    #[command(
        subcommand,
        about = "Сервер 1С по сети, как тонкий клиент: сводка, метаданные, вызовы, модули, объекты, отчёты, запись, формы, печать",
        after_help = remote::EXAMPLES
    )]
    Remote(remote::Command),
}

fn cli_command() -> clap::Command {
    Cli::command().after_help(format!("{EXAMPLES}\n\n{}", env::help_section()))
}

static ENDED_BY: std::sync::Mutex<Option<yellowcode_core::plain::bak::Interrupt>> =
    std::sync::Mutex::new(None);

pub fn exit(code: i32) -> ! {
    let ended_by = ENDED_BY.lock().ok().and_then(|slot| *slot);
    match ended_by {
        Some(interrupt) if code == interrupt.exit_code() => {
            yellowcode_core::plain::bak::end_process(interrupt)
        }
        _ => std::process::exit(code),
    }
}

pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> i32 {
    use std::io::IsTerminal;
    run_with_terminals(
        args,
        input,
        out,
        err,
        tty,
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    )
}

pub fn run_with_stdin_terminal(
    args: impl IntoIterator<Item = OsString>,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
    stdin_terminal: bool,
) -> i32 {
    run_with_terminals(args, input, out, err, tty, stdin_terminal, false)
}

pub fn run_with_terminals(
    args: impl IntoIterator<Item = OsString>,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
    stdin_terminal: bool,
    stderr_terminal: bool,
) -> i32 {
    let args: Vec<OsString> = args.into_iter().collect();
    let parsed = cli_command()
        .try_get_matches_from(&args)
        .and_then(|matches| {
            Cli::from_arg_matches(&matches)
                .map(|cli| (cli, matches))
                .map_err(|error| error.format(&mut cli_command()))
        });
    let (mut cli, matches) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            if error.use_stderr() {
                let partial = cli_command()
                    .ignore_errors(true)
                    .try_get_matches_from(&args);
                if partial.ok().is_some_and(|matches| {
                    matches
                        .subcommand_matches("ib")
                        .and_then(|ib| ib.subcommand_matches("users"))
                        .is_some_and(|users| {
                            matches!(
                                users.subcommand_name(),
                                Some("create" | "edit" | "delete" | "clear-password")
                            )
                        })
                }) {
                    ib::users::write_notice(err);
                }
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return error.exit_code();
        }
    };
    let readonly_value = std::env::var_os(readonly::ENV);
    match readonly::readonly_from_env(readonly_value.as_deref()) {
        Err(message) => {
            let _ = writeln!(err, "yellowcode: {message}");
            return 2;
        }
        Ok(true) => {
            if let (Some(command), Some(value)) = (&cli.command, &readonly_value)
                && command.effect() == readonly::Effect::Change
            {
                let path = readonly::command_path(&matches);
                let _ = writeln!(err, "yellowcode: {}", readonly::refusal(&path, value));
                return readonly::REFUSED;
            }
        }
        Ok(false) => {}
    }
    if let Some(Command::Remote(command)) = &mut cli.command
        && let Some(("remote", m)) = matches.subcommand()
    {
        remote::with_order(command, m);
    }
    if let Some(Command::Bak(command)) = &cli.command
        && let Some(error) = command.usage_error()
    {
        let _ = write!(err, "{error}");
        return error.exit_code();
    }
    let user_mutation = matches!(
        &cli.command,
        Some(Command::Ib(ib::Command::Users(command))) if command.is_mutation()
    );
    if user_mutation {
        ib::users::write_notice(err);
    }
    if let Err(error) = yellowcode_core::platform::select(&cli.platform_build) {
        let _ = writeln!(err, "yellowcode: {error}");
        return 2;
    }
    let progress_style = match progress::style(
        cli.progress,
        std::env::var_os(progress::ENV).as_deref(),
        stderr_terminal,
    ) {
        Ok(style) => style,
        Err(message) => {
            let _ = writeln!(err, "yellowcode: {message}");
            return 2;
        }
    };
    let shows_progress = match &cli.command {
        Some(Command::Config(_) | Command::Ext(_)) => true,
        Some(Command::Convert(args)) => !args.restores(),
        _ => false,
    };
    let reporter = progress_style
        .filter(|_| shows_progress)
        .and_then(|style| progress::Reporter::start(style, Box::new(std::io::stderr())));
    let mut out = progress::StopOnWrite::new(out, reporter.as_ref(), tty);
    let mut err = progress::ClearOnWrite::new(err, reporter.as_ref());
    let err: &mut dyn Write = &mut err;
    let mut output = yellowcode_core::output::Out {
        w: &mut out,
        closed: false,
    };
    let report = match bak_report::begin(cli.command.as_ref()) {
        Ok(report) => report,
        Err(message) => {
            let _ = writeln!(err, "yellowcode: {message}");
            return 2;
        }
    };
    let grep_command = matches!(cli.command, Some(Command::Config(config::Command::Grep(_))));
    let result = match cli.command {
        Some(Command::Open(args)) => {
            let explicit = matches.value_source("platform_build") == Some(ValueSource::CommandLine);
            open::run(args, explicit.then_some(cli.platform_build.as_str()), err)
        }
        Some(Command::Config(command)) => config::run(command, &mut output, err),
        Some(Command::Ext(command)) => ext::run(command, &mut output).map(|()| 0),
        Some(Command::Ib(command)) => {
            ib::run(command, cli.platform_build, input, &mut output, err, tty)
        }
        Some(Command::Convert(args)) => convert::run(args, &mut output, err),
        Some(Command::Bak(command)) => bak::run(command, &mut output, err).map(|()| 0),
        Some(Command::Remote(command)) => {
            remote::run(command, input, &mut output, err, stdin_terminal)
        }
        None => cli_command()
            .write_long_help(&mut output)
            .map(|()| 0)
            .map_err(Into::into),
    }
    .and_then(|code| match output.flush() {
        Err(_) if user_mutation => Err(ib::users::committed_output_error()),
        Err(_) if output.closed => Ok(code),
        Err(error) => Err(error.into()),
        Ok(()) => Ok(code),
    });
    if let Some(reporter) = &reporter {
        reporter.stop();
    }
    match result {
        Ok(code) => {
            bak_report::end(report, code, None, err);
            code
        }
        Err(_) if output.closed && !user_mutation => {
            bak_report::end(report, 0, None, err);
            0
        }
        Err(error) => {
            let _ = writeln!(err, "yellowcode: {error}");
            let code = if let Some(interrupt) = yellowcode_core::plain::bak::interrupted(&error)
                .or_else(|| yellowcode_core::plain::convert::interrupted(&error))
            {
                if let Ok(mut slot) = ENDED_BY.lock() {
                    *slot = Some(interrupt);
                }
                interrupt.exit_code()
            } else if grep_command
                || error
                    .downcast_ref::<memory_budget::policy::ConfigError>()
                    .is_some()
            {
                2
            } else {
                1
            };
            bak_report::end(report, code, Some(&error.to_string()), err);
            code
        }
    }
}
