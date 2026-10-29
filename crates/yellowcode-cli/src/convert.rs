use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::convert::{self, Format, Layout, Request};

#[derive(clap::Args)]
#[command(after_help = "\
Поддерживаемые преобразования: DT -> CF, 1CD -> CF, 1CD -> DT, DT -> 1CD.

Примеры:
  # Сохранить конфигурацию из DT в CF
  yellowcode convert backup.dt --to cf --output configuration.cf

  # Сохранить конфигурацию файловой базы в CF
  yellowcode convert base --to cf --output configuration.cf

  # Выгрузить файловую базу в DT
  yellowcode convert base/1Cv8.1CD --to dt --output backup.dt --users-layout users8

  # Загрузить DT в новую файловую базу (каталог new-base создать заранее), выделив 4 ГиБ памяти
  yellowcode convert backup.dt --to 1cd --output new-base/1Cv8.1CD --memory-limit 4GiB

  # Загрузить DT без пользователей (база откроется без входа, с полными правами)
  yellowcode convert backup.dt --to 1cd --output new-base/1Cv8.1CD --without-users")]
pub(crate) struct Args {
    #[arg(
        help = "Исходный файл: CF, DT, файл 1Cv8.1CD или каталог базы; формат определяется по содержимому"
    )]
    source: PathBuf,
    #[arg(long, value_enum, help = "Формат результата")]
    to: Format,
    #[arg(
        long,
        help = "Путь к новому файлу; его каталог должен существовать, существующий файл не перезаписывается"
    )]
    output: PathBuf,
    #[arg(
        long,
        value_enum,
        help = "Формат таблицы пользователей в DT при выгрузке 1CD -> DT (обязателен, сам не выбирается): \
                users8 — таблица Users из 8 колонок, users-spr11 — таблица UsersSpr из 11 колонок \
                (пользователи разделены по областям данных)"
    )]
    users_layout: Option<Layout>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Бюджет памяти для преобразования с DT: auto или целое число байт с суффиксом B/KiB/MiB/GiB \
                (как у query и serve; переменная YELLOWCODE_MEMORY_LIMIT)"
    )]
    memory_limit: Option<String>,
    #[arg(
        long,
        help = "Только DT -> 1CD: загрузить базу без пользователей (если их нельзя перенести как есть). \
                Такая база открывается без входа, с полными правами"
    )]
    without_users: bool,
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        Request {
            source: a.source,
            to: a.to,
            output: a.output,
            users_layout: a.users_layout,
            memory_limit: a.memory_limit,
            without_users: a.without_users,
        }
    }
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<i32> {
    convert::run(args.into(), out, err)
}
