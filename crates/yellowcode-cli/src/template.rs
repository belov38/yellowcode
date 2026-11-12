use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::metadata::Format;
use yellowcode_core::plain::template::{self, TypeFilter};

use crate::scope::ScopeFlags;

macro_rules! examples {
    () => {
        "\
Примеры:
  # Схемы компоновки данных отчётов подсистемы
  yellowcode template list configuration.cf --type dcs --subsystem СтандартныеПодсистемы.Пользователи

  # Содержимое схемы: наборы, запросы, параметры, варианты
  yellowcode template show configuration.cf Отчет.СведенияОПользователях.Макет.ОсновнаяСхемаКомпоновкиДанных

  # Только текст запроса одного набора
  yellowcode template show configuration.cf Отчет.СведенияОПользователях.Макет.ОсновнаяСхемаКомпоновкиДанных --set Пользователи --query"
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
        about = "Список макетов конфигурации с типами, по одному на строку",
        after_help = concat!(
            "Строка текста: `путь<TAB>тип`, путь вида `Вид.Объект.Template.Имя` у макета объекта и \
`CommonTemplate.Имя` у общего; тип — `DataCompositionSchema`, `SpreadsheetDocument`, `TextDocument`, `HTMLDocument`, \
`BinaryData` и т. д., `Unknown` — если конфигурация тип не называет. Путь из списка принимает `template show`. \
Макеты без данных не перечисляются. `--format json`: объект с `schemaVersion: 1` и `templates[]` (`path`, `type`).\n\n",
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
        #[arg(
            long = "type",
            value_name = "ТИПЫ",
            value_enum,
            hide_possible_values = true,
            value_delimiter = ',',
            help = "Только макеты этих типов через запятую: dcs — схема компоновки данных, sheet — табличный документ, text, html, binary — двоичные данные, other — остальные и неизвестные"
        )]
        types: Vec<TypeFilter>,
        #[arg(long, value_enum, default_value = "text", help = FORMAT_HELP)]
        format: Format,
    },
    #[command(
        about = "Содержимое макета; для схемы компоновки данных — наборы, запросы, параметры, варианты",
        after_help = concat!(
            "Макет — путь из `template list` (виды и слова `Макет`, `ОбщийМакет` — русские или английские). \
Схема компоновки данных показывается разделами: наборы данных с запросами, связи, ресурсы, вычисляемые поля, \
параметры, варианты. Отборы и другие значения схемы в тексте печатаются сырыми токенами схемы (`Equal`, `true`), \
а не русскими названиями. `--set НАБОР --query` печатает только текст запроса набора, как он записан в схеме, \
с завершающим переводом строки, если его нет. Текст запроса схемы содержит расширения языка компоновки данных \
(`{ГДЕ ...}`, `ХАРАКТЕРИСТИКИ`) и в общем случае не выполняется командой `yellowcode query`. Табличный документ \
показывается окном в 500 строк, при обрезке stderr говорит `yellowcode: shown rows 1–500 of N`; ячейки в тексте \
нумеруются с 1 (`R3C2`), а в `--format json` строки и колонки считаются с 0. Остальные макеты: текст или список \
файлов с размерами. `--format json`: объект с `schemaVersion: 1`, `path`, `type` и `view`; с `--query` — `path`, \
`dataSet`, `query`. Нет такого набора с запросом — ошибка со списком наборов, у которых запрос есть.\n\n",
            examples!()
        )
    )]
    Show {
        #[arg(help = SOURCE_HELP)]
        source: PathBuf,
        #[arg(long, allow_hyphen_values = true, help = MEMORY_LIMIT_HELP)]
        memory_limit: Option<String>,
        #[arg(
            help = "Путь макета из `template list`, например Отчет.СведенияОПользователях.Макет.ОсновнаяСхемаКомпоновкиДанных или ОбщийМакет.Печать"
        )]
        template: String,
        #[arg(
            long = "set",
            value_name = "НАБОР",
            requires = "query",
            help = "Набор данных схемы, чей запрос печатать; вместе с --query"
        )]
        set: Option<String>,
        #[arg(
            long,
            requires = "set",
            help = "Печатать только текст запроса набора из --set"
        )]
        query: bool,
        #[arg(long, value_enum, default_value = "text", help = FORMAT_HELP)]
        format: Format,
    },
}

fn split(command: Command) -> (template::Command, Option<String>) {
    match command {
        Command::List {
            source,
            memory_limit,
            scope,
            types,
            format,
        } => (
            template::Command::List {
                source,
                scope: scope.into(),
                types,
                format,
            },
            memory_limit,
        ),
        Command::Show {
            source,
            memory_limit,
            template,
            set,
            query,
            format,
        } => (
            template::Command::Show {
                source,
                template,
                query_of: set.filter(|_| query),
                format,
            },
            memory_limit,
        ),
    }
}

pub(crate) fn run(
    command: Command,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    let (command, limit) = split(command);
    template::run_with_memory_limit(command, limit, out, err)
}
