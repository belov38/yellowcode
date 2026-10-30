use std::{io::Write, path::PathBuf};

fn parse_backup_set(value: &str) -> Result<u32, String> {
    let position = value
        .parse::<u32>()
        .map_err(|_| "ожидается номер от 1 до 2147483647".to_owned())?;
    if !(1..=i32::MAX as u32).contains(&position) {
        return Err("ожидается номер от 1 до 2147483647".into());
    }
    Ok(position)
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Format {
    Cf,
    Dt,
    #[value(name = "1cd")]
    Onecd,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(about = "Проверить структуру резервной копии без её восстановления")]
    Inspect {
        #[arg(help = "Файл резервной копии MS SQL Server")]
        source: PathBuf,
    },
    #[command(
        about = "Восстановить копию через MS SQL Server и сохранить CF, DT или 1CD",
        after_help = "\
Путь SOURCE должен быть доступен службе SQL Server на машине сервера.\n\
Если BAK содержит несколько наборов, выберите позицию через --backup-set; по умолчанию используется первая.\n\
CF содержит основную конфигурацию, но не прикладные данные или расширения.\n\
Пока поддерживаются одна data- и одна log-часть, без TDE.\n\
CF и DT: CONFIG до 16 ГиБ, не более 1 млн файлов и 1024 частей на файл; строка ответа sqlcmd до 128 МиБ. Имена должны быть ASCII, Attributes — нулевым.\n\
1CD: CONFIG проходит через DT и загружается частями; действуют ограничения памяти и формата 1CD. Поддерживаемые локали: ru_RU, en_US.\n\
DT/1CD: остальные файловые разделы — до 1024 частей и 64 МиБ на файл, с нулевыми Attributes; действуют ограничения на данные и память.\n\
DT и 1CD — экспериментальный ограниченный перенос с управляемыми блокировками; обычные непустые пользователи и неподтверждённые хранилища не поддерживаются.\n\
Разделённые пользователи поддерживаются при одном общем реквизите с разделением данных и совместимости 80501; остальные общие реквизиты не поддерживаются.\n\
DT требует проверенный служебный профиль и целиком учитывает локальное двоичное хранилище.\n\
При аварийном прерывании SQL может оставить временную базу и её файлы; имя базы выводится в stderr.\n\
Для SQL-аутентификации задайте --user и переменную SQLCMDPASSWORD; без --user используется доверенное подключение.\n\
Нужен Microsoft sqlcmd (ODBC) и права на создание временной базы.\n\
Пример: yellowcode bak convert 'C:\\Backup\\base.bak' --server localhost --to cf --output base.cf"
    )]
    Convert {
        #[arg(help = "Путь к BAK на машине SQL Server")]
        source: String,
        #[arg(long, help = "Имя экземпляра SQL Server")]
        server: String,
        #[arg(long, default_value = "1", value_parser = parse_backup_set, help = "Номер набора резервной копии в BAK (позиция из RESTORE HEADERONLY)")]
        backup_set: u32,
        #[arg(long, value_enum, help = "Формат результата")]
        to: Format,
        #[arg(long, help = "Путь к новому файлу на машине с Yellowcode")]
        output: PathBuf,
        #[arg(long, help = "Логин SQL Server; пароль берётся из SQLCMDPASSWORD")]
        user: Option<String>,
        #[arg(
            long,
            default_value = "sqlcmd",
            help = "Путь к Microsoft sqlcmd (ODBC)"
        )]
        sqlcmd: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..), help = "Общий срок каждой команды sqlcmd в секундах (по умолчанию 1800 + 120 на остановку); очистка получает отдельный срок")]
        sqlcmd_lifecycle_timeout: Option<u64>,
        #[arg(
            long,
            help = "Доверять сертификату SQL Server без проверки его цепочки"
        )]
        trust_server_certificate: bool,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Только для 1CD: бюджет памяти восстановления DT (auto или B/KiB/MiB/GiB); не ограничивает MS SQL Server"
        )]
        memory_limit: Option<String>,
    },
}

pub(crate) fn run(
    command: Command,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    match command {
        Command::Inspect { source } => yellowcode_core::plain::bak::inspect(&source, out),
        Command::Convert {
            source,
            server,
            backup_set,
            to,
            output,
            user,
            sqlcmd,
            sqlcmd_lifecycle_timeout,
            trust_server_certificate,
            memory_limit,
        } => {
            if memory_limit.is_some() && !matches!(to, Format::Onecd) {
                return Err("--memory-limit is only for 1CD conversion".into());
            }
            let request = yellowcode_core::plain::bak::SqlConvertRequest {
                backup_path: source,
                server,
                output,
                user,
                sqlcmd,
                lifecycle_timeout: sqlcmd_lifecycle_timeout.map(std::time::Duration::from_secs),
                trust_server_certificate,
            };
            match to {
                Format::Cf => yellowcode_core::plain::bak::convert_with_backup_set(
                    request, backup_set, out, err,
                ),
                Format::Dt => yellowcode_core::plain::bak::convert_dt_with_backup_set(
                    request, backup_set, out, err,
                ),
                Format::Onecd => {
                    yellowcode_core::plain::bak::convert_onecd_with_memory_and_backup_set(
                        request,
                        memory_limit,
                        backup_set,
                        out,
                        err,
                    )
                }
            }
        }
    }
}
