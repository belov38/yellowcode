use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::metadata::Format;
use yellowcode_core::plain::module::{self, Selection, parse_lines};

use crate::scope::ScopeFlags;

macro_rules! examples {
    () => {
        "\
Примеры:
  # Модули справочников подсистемы «Пользователи»
  yellowcode module list configuration.cf --kind Справочник --subsystem СтандартныеПодсистемы.Пользователи

  # Модуль объекта справочника
  yellowcode module show configuration.cf Справочник.Пользователи.МодульОбъекта

  # Одна процедура с номерами строк
  yellowcode module show configuration.cf Справочник.Пользователи.МодульОбъекта --procedure ПередЗаписью -n

  # Строки 100–160 модуля формы
  yellowcode module show configuration.cf Справочник.Пользователи.Форма.ФормаЭлемента.МодульФормы --lines 100:160

  # Процедуры и функции общего модуля
  yellowcode module outline configuration.cf ОбщийМодуль.Пользователи"
    };
}

pub(crate) const EXAMPLES: &str = examples!();

const SOURCE_HELP: &str = "Конфигурация: CF, DT, файл 1Cv8.1CD или каталог базы";
const MEMORY_LIMIT_HELP: &str =
    "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него";
const FORMAT_HELP: &str = "Формат вывода";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Список модулей конфигурации, по одному пути на строку",
        after_help = concat!(
            "Путь из списка принимают `module show` и `module outline`. Модули объектов внешних источников \
данных не перечисляются и по путям модулей не адресуются.\n\n",
            examples!()
        )
    )]
    List {
        #[arg(help = SOURCE_HELP)]
        source: PathBuf,
        #[arg(long, allow_hyphen_values = true, help = MEMORY_LIMIT_HELP)]
        memory_limit: Option<String>,
        #[command(flatten)]
        scope: ScopeFlags,
        #[arg(long, value_enum, default_value = "text", help = FORMAT_HELP)]
        format: Format,
    },
    #[command(
        about = "Текст модуля, целиком, по строкам или одна процедура",
        after_help = EXAMPLES
    )]
    Show {
        #[arg(help = SOURCE_HELP)]
        source: PathBuf,
        #[arg(long, allow_hyphen_values = true, help = MEMORY_LIMIT_HELP)]
        memory_limit: Option<String>,
        #[arg(
            help = "Путь модуля из `module list` или объект с единственным модулем, например Справочник.Пользователи.МодульОбъекта"
        )]
        module: String,
        #[arg(
            long,
            value_name = "A:B",
            value_parser = parse_lines,
            conflicts_with = "procedure",
            help = "Только строки с A по B (с единицы); `A:` — до конца модуля"
        )]
        lines: Option<Selection>,
        #[arg(
            long,
            value_name = "ИМЯ",
            help = "Только процедура или функция с этим именем, с комментариями и директивами над ней"
        )]
        procedure: Option<String>,
        #[arg(
            short = 'n',
            long = "line-numbers",
            help = "Начинать каждую строку с её номера в модуле"
        )]
        numbers: bool,
        #[arg(long, value_enum, default_value = "text", help = FORMAT_HELP)]
        format: Format,
    },
    #[command(
        about = "Процедуры и функции модуля с номерами строк",
        after_help = EXAMPLES
    )]
    Outline {
        #[arg(help = SOURCE_HELP)]
        source: PathBuf,
        #[arg(long, allow_hyphen_values = true, help = MEMORY_LIMIT_HELP)]
        memory_limit: Option<String>,
        #[arg(
            help = "Путь модуля из `module list` или объект с единственным модулем, например ОбщийМодуль.Пользователи"
        )]
        module: String,
        #[arg(long, value_enum, default_value = "text", help = FORMAT_HELP)]
        format: Format,
    },
}

fn split(command: Command) -> (module::Command, Option<String>) {
    match command {
        Command::List {
            source,
            memory_limit,
            scope,
            format,
        } => (
            module::Command::List {
                source,
                scope: scope.into(),
                format,
            },
            memory_limit,
        ),
        Command::Show {
            source,
            memory_limit,
            module,
            lines,
            procedure,
            numbers,
            format,
        } => {
            let selection = match (lines, procedure) {
                (Some(lines), _) => lines,
                (None, Some(name)) => Selection::Procedure(name),
                (None, None) => Selection::Whole,
            };
            (
                module::Command::Show {
                    source,
                    module,
                    selection,
                    numbers,
                    format,
                },
                memory_limit,
            )
        }
        Command::Outline {
            source,
            memory_limit,
            module,
            format,
        } => (
            module::Command::Outline {
                source,
                module,
                format,
            },
            memory_limit,
        ),
    }
}

pub(crate) fn run(command: Command, out: &mut dyn Write) -> yellowcode_core::Result<()> {
    let (command, limit) = split(command);
    module::run_with_memory_limit(command, limit, out)
}
