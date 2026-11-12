use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::export;
use yellowcode_core::plain::metadata::Format;

use crate::scope::ScopeFlags;

const PATH_HELP: &str = "Объект и всё под ним, например Справочник.Номенклатура, или один модуль по адресу из `module list`, например Справочник.Номенклатура.МодульОбъекта; флаг можно повторять";

pub(crate) const AFTER_HELP: &str = "\
Файлы лежат как в выгрузке конфигуратора: `Catalogs/Имя/Ext/ObjectModule.bsl`, `Catalogs/Имя/Forms/Форма/Ext/Form/Module.bsl`, \
`Catalogs/Имя/Templates/Макет/Ext/Template.*`; байты — как хранятся в источнике (BOM, CRLF). Каталог `--output` создаётся или \
должен быть пуст; в непустой каталог и поверх файла выгрузка не пишется. Последним пишется `manifest.json` (источник, область, \
числа и список файлов с размерами): он есть только у полной выгрузки. Если выгрузка оборвалась, stderr говорит об этом, код 1, \
манифеста нет. Пустая область — успех с пустым списком файлов. Описания объектов (XML) не выгружаются; внешние источники \
данных выгружаются, только когда в области объект `ExternalDataSource.Имя` целиком. Код возврата: 0 — успех, 1 — ошибка \
источника, области или каталога, 2 — ошибка аргументов.

Примеры:
  # Модули и макеты подсистемы «Пользователи» для поиска обычными средствами
  yellowcode export configuration.cf --subsystem СтандартныеПодсистемы.Пользователи --output users-src
  rg -n 'ПроверитьПрава' users-src

  # Все документы
  yellowcode export base --kind Документ --output docs-src";

#[derive(clap::Args)]
#[command(
    after_help = AFTER_HELP,
    mut_arg("path", |a| a.help(PATH_HELP))
)]
pub(crate) struct Args {
    #[arg(help = "Конфигурация: CF, DT, файл 1Cv8.1CD или каталог базы")]
    source: PathBuf,
    #[arg(
        long,
        value_name = "DIR",
        help = "Каталог выгрузки: создаётся или должен быть пуст"
    )]
    output: PathBuf,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
    )]
    memory_limit: Option<String>,
    #[command(flatten)]
    scope: ScopeFlags,
    #[arg(long, value_enum, default_value = "text", help = "Формат вывода")]
    format: Format,
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    export::run_with_memory_limit(
        args.source,
        args.scope.into(),
        args.output,
        args.format,
        args.memory_limit,
        out,
        err,
    )
}
