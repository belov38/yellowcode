use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::grep::{self, GrepArgs, Output, Where};
use yellowcode_core::plain::metadata::Format;

use crate::scope::ScopeFlags;

const AFTER_HELP: &str = concat!(
            "Вывод: `путь:строка:текст` для совпадения и `путь-строка-текст` для строки контекста; совпадение в имени \
объекта печатается как `путь:имя`. Путь и строку читает `module show --lines`. У запроса схемы компоновки данных \
путь — `ПУТЬ_МАКЕТА#НАБОР`, строки считаются с 1 от начала запроса (`template show --set НАБОР --query`); \
схема больше предела просмотра пропускается, stderr сообщает об этом. Код возврата: 0 — есть совпадения, \
1 — нет, 2 — ошибка (аргументы, источник, выражение, путь области). Результат длиннее `--max-count` обрезается; \
об этом сообщает stderr, код остаётся 0.\n\n",
            "\
Примеры:
  # Где вызывается процедура (регулярное выражение, без учёта регистра)
  yellowcode grep 'ПользователиСлужебный\\.ПроверитьПрава' configuration.cf

  # Подстрока в модулях документов, по 2 строки контекста
  yellowcode grep -F 'НачатьТранзакцию' configuration.cf --kind Документ -C 2

  # Обработчики ПередЗаписью в подсистеме
  yellowcode grep 'Процедура\\s+ПередЗаписью' configuration.cf --subsystem СтандартныеПодсистемы.Пользователи

  # Только пути модулей с совпадениями
  yellowcode grep -l 'ЗаписьЖурналаРегистрации' configuration.cf

  # Объекты, в имени или синониме которых есть «Контрагент»
  yellowcode grep -F Контрагент configuration.cf --in names

  # Где в запросах схем компоновки данных упомянут регистр
  yellowcode grep -F РегистрНакопления.Продажи configuration.cf --in queries"
);

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Place {
    Code,
    Queries,
    Names,
}

impl From<Place> for Where {
    fn from(place: Place) -> Where {
        match place {
            Place::Code => Where::Code,
            Place::Queries => Where::Queries,
            Place::Names => Where::Names,
        }
    }
}

#[derive(clap::Args)]
#[command(after_help = AFTER_HELP)]
pub(crate) struct Args {
    #[arg(
        help = "Регулярное выражение без учёта регистра; с -F — подстрока (`ё` равна `е`). Если шаблон начинается с `-`, поставьте перед ним `--`"
    )]
    pattern: String,
    #[arg(help = "Конфигурация: CF, DT, файл 1Cv8.1CD или каталог базы")]
    source: PathBuf,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
    )]
    memory_limit: Option<String>,
    #[command(flatten)]
    scope: ScopeFlags,
    #[arg(
        long = "in",
        value_name = "МЕСТА",
        value_enum,
        value_delimiter = ',',
        default_values_t = [Place::Code],
        help = "Где искать, через запятую: code — тексты модулей, queries — тексты запросов схем компоновки данных, names — имена и синонимы объектов"
    )]
    places: Vec<Place>,
    #[arg(
        short = 'F',
        long = "fixed-strings",
        help = "Шаблон — подстрока, а не регулярное выражение"
    )]
    fixed: bool,
    #[arg(
        short = 's',
        long = "case-sensitive",
        help = "Различать регистр в текстах модулей и запросов; на `--in names` не влияет, имена 1С регистронезависимы"
    )]
    case_sensitive: bool,
    #[arg(
        short = 'C',
        long,
        value_name = "N",
        default_value_t = 0,
        help = "Строк контекста вокруг совпадения в текстах модулей и запросов"
    )]
    context: usize,
    #[arg(
        short = 'l',
        long = "files-with-matches",
        conflicts_with = "count",
        help = "Печатать только пути с совпадениями"
    )]
    files_with_matches: bool,
    #[arg(short = 'c', long, help = "Печатать для каждого пути число совпадений")]
    count: bool,
    #[arg(
        long,
        value_name = "N",
        default_value_t = 1000,
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Сколько строк-совпадений выдать, не меньше 1 (строки контекста не считаются)"
    )]
    max_count: u64,
    #[arg(long, value_enum, default_value = "text", help = "Формат вывода")]
    format: Format,
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<i32> {
    let output = if args.files_with_matches {
        Output::FilesWithMatches
    } else if args.count {
        Output::Count
    } else {
        Output::Lines
    };
    let grep_args = GrepArgs {
        pattern: args.pattern,
        fixed: args.fixed,
        case_sensitive: args.case_sensitive,
        places: args.places.into_iter().map(Into::into).collect(),
        context: args.context,
        output,
        max_count: usize::try_from(args.max_count).unwrap_or(usize::MAX),
        format: args.format,
    };
    grep::run_with_memory_limit(
        args.source,
        args.scope.into(),
        grep_args,
        args.memory_limit,
        out,
        err,
    )
}
