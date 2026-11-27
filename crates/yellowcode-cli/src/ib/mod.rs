use std::io::{Read, Write};

pub(crate) mod query;
pub(crate) mod serve;
pub(crate) mod table;
pub(crate) mod users;

pub(crate) const EXAMPLES: &str = "\
База — каталог файловой базы с 1Cv8.1CD или сам файл 1Cv8.1CD.

Примеры:
  # Выполнить запрос (только чтение)
  yellowcode ib query base --text \"ВЫБРАТЬ ПЕРВЫЕ 10 Наименование ИЗ Справочник.Контрагенты\"

  # Прочитать первую страницу таблицы документа
  yellowcode ib table base Документ Счет --limit 100

  # Список пользователей базы
  yellowcode ib users list base

  # HTTP-сервис запросов на 127.0.0.1:8080
  yellowcode ib serve base

Справка по команде: yellowcode ib <команда> --help";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(about = "Выполнить запрос к информационной базе (только чтение)")]
    Query(query::Args),
    #[command(about = "Прочитать одну страницу таблицы базы, при необходимости по индексу")]
    Table(table::Args),
    #[command(
        subcommand,
        about = "Пользователи информационной базы и их карточки (без паролей и других секретов)",
        after_help = users::EXAMPLES
    )]
    Users(users::Command),
    #[command(about = "HTTP-сервис запросов и постраничного чтения таблиц (только чтение)")]
    Serve(serve::Args),
}

pub(crate) fn run(
    command: Command,
    platform_build: String,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<i32> {
    match command {
        Command::Query(args) => query::run(args, input, out, err, tty),
        Command::Table(args) => table::run(args, out, err, tty),
        Command::Users(command) => users::run(command, out, tty).map(|()| 0),
        Command::Serve(args) => serve::run(args, platform_build, err).map(|()| 0),
    }
}
