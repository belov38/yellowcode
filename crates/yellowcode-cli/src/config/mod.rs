use std::io::Write;

pub(crate) mod export;
pub(crate) mod grep;
pub(crate) mod metadata;
pub(crate) mod module;
pub(crate) mod template;

pub(crate) const EXAMPLES: &str = "\
Источник — файл CF или DT, файл 1Cv8.1CD или каталог файловой базы. Источник только читается.

Примеры:
  # Справочники и документы конфигурации
  yellowcode config metadata tree configuration.cf --kind Справочник,Документ --depth 2 --format text

  # Процедуры и функции общего модуля
  yellowcode config module outline configuration.cf ОбщийМодуль.Пользователи

  # Схемы компоновки данных конфигурации
  yellowcode config template list configuration.cf --type dcs

  # Где вызывается процедура
  yellowcode config grep -F 'НачатьТранзакцию' configuration.cf --kind Документ

  # Модули и макеты документов в каталог
  yellowcode config export base --kind Документ --output docs-src

Справка по команде: yellowcode config <команда> --help";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(
        subcommand,
        about = "Метаданные конфигурации",
        after_help = metadata::EXAMPLES
    )]
    Metadata(metadata::Command),
    #[command(
        subcommand,
        about = "Модули: список, текст, процедуры и функции",
        after_help = module::EXAMPLES
    )]
    Module(module::Command),
    #[command(
        subcommand,
        about = "Макеты: список, содержимое, запросы схем компоновки данных",
        after_help = template::EXAMPLES
    )]
    Template(template::Command),
    #[command(
        about = "Поиск в текстах модулей, запросах схем компоновки данных и именах объектов, как grep"
    )]
    Grep(grep::Args),
    #[command(about = "Выгрузить модули и макеты выбранной части конфигурации в каталог")]
    Export(export::Args),
}

pub(crate) fn run(
    command: Command,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<i32> {
    match command {
        Command::Metadata(command) => metadata::run(command, out).map(|()| 0),
        Command::Module(command) => module::run(command, out).map(|()| 0),
        Command::Template(command) => template::run(command, out, err).map(|()| 0),
        Command::Grep(args) => grep::run(args, out, err),
        Command::Export(args) => export::run(args, out, err).map(|()| 0),
    }
}
