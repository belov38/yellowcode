use std::{io::Write, net::SocketAddr, path::PathBuf};
use yellowcode_core::service::memory;
use yellowcode_http::service::serve::{self, Config};

#[derive(clap::Args)]
#[command(after_help = "\
Сервис открывает базу один раз и не видит последующих изменений: запускайте его на неизменной копии
и перезапускайте после её обновления.

Примеры:
  # Сервис на 127.0.0.1:8080
  yellowcode ib serve base

  # Другой адрес и порт, 3 ГиБ памяти (конфигурации уровня ERP)
  yellowcode ib serve base --listen 0.0.0.0:9000 --memory-limit 3GiB

  # Запрос к сервису
  curl http://127.0.0.1:8080/query -H \"Content-Type: application/json\" \\
    --data '{\"text\": \"ВЫБРАТЬ &Число * 2 КАК Ответ\", \"params\": [{\"name\": \"Число\", \"repr\": \"21\"}]}'

  # Страница таблицы справочника
  curl --get http://127.0.0.1:8080/table/Справочник \\
    --data-urlencode \"name=Контрагенты\" --data-urlencode \"limit=100\"")]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    #[arg(
        help = "Неизменяемая копия базы: каталог или файл 1Cv8.1CD; открывается один раз только для чтения"
    )]
    base: PathBuf,
    #[arg(
        long,
        default_value = "127.0.0.1:8080",
        help = "Адрес HTTP (контроль доступа и TLS — на стороне инфраструктуры)"
    )]
    listen: SocketAddr,
    #[arg(
        long,
        help = "Все замечания о расхождении схемы базы с правилами вместо одной итоговой строки (stderr)"
    )]
    schema_notes: bool,
}

pub(crate) fn run(
    args: Args,
    platform_build: String,
    err: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    serve::run(
        Config {
            platform_build,
            memory: args.memory,
            base: args.base,
            listen: args.listen,
            schema_notes: args.schema_notes,
        },
        err,
    )
}
