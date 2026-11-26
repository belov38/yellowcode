use std::{io::Write, path::PathBuf};

use clap::ValueEnum as _;

pub(crate) const SUMMARY: &str = "Копия восстанавливается во временную базу на вашем SQL Server, результат выгружается, \
временная база удаляется; без --server CF читается прямо из файла BAK. CF: из полной копии, в том числе сжатой; на копиях \
типовых конфигураций записи как в выгрузке Конфигуратора, с --raw — как конфигурация хранится в базе. DT и 1CD: базы с управляемыми блокировками, в том числе типовых конфигураций на БСП с режимом \
совместимости 8.3.14–8.5.1, копии которых записаны платформой 8.5.1; что не поддерживается, команда отклоняет и называет \
причину; приёмка DT и 1CD текущей сборки не закончена (в ответе \"experimental\": true). Разностные, журнальные, шифрованные \
(в том числе TDE), составные и повреждённые копии отклоняются до восстановления. Проверено только на SQL Server 2022. \
Подробности: yellowcode bak convert --help и https://yellowcode.dev/guide/bak-convert/";

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
        about = "Сохранить CF, DT или 1CD из копии MS SQL Server: через сервер (--server) или, только CF, прямым чтением файла",
        after_help = "\
Копия восстанавливается во временную базу на вашем SQL Server, результат выгружается на машину с Yellowcode, временная \
база удаляется. Без --server CF читается прямо из файла BAK, без SQL Server.

Что нужно для работы с --server:
  - Microsoft sqlcmd на ODBC (пакет mssql-tools18 или mssql-tools) на машине с Yellowcode; go-sqlcmd не подходит;
  - логин с ролью sysadmin: команда должна уметь удалить временную базу, поэтому без этой роли она отказывает сразу, \
до создания базы;
  - место на сервере под всю базу, данные и журнал, даже если нужна только конфигурация;
  - SOURCE — путь к BAK так, как его видит служба SQL Server на машине сервера: Yellowcode файл не копирует.

Примеры:
  # CF через SQL Server: Windows, локальный именованный экземпляр, доверенное подключение
  yellowcode bak convert 'C:\\Backup\\base.bak' --server localhost\\SQL2022 --to cf --output base.cf

  # CF без SQL Server: прямое чтение локального файла копии
  yellowcode bak convert base.bak --to cf --output base.cf

  # DT: Linux, SQL Server 2022 в Docker на той же машине, SQL-логин; каталог с копией смонтирован в контейнер как /backup
  SQLCMDPASSWORD='<пароль>' yellowcode bak convert /backup/base.bak --server 127.0.0.1 --user sa --trust-server-certificate --to dt --output base.dt

  # 1CD: новая файловая база (каталог new-base создать заранее), 4 ГиБ памяти на загрузку DT
  SQLCMDPASSWORD='<пароль>' yellowcode bak convert /backup/base.bak --server 127.0.0.1 --user sa --trust-server-certificate --to 1cd --output new-base/1Cv8.1CD --memory-limit 4GiB

Что получается:
  CF — из полной копии, в том числе сжатой. Если конфигурацию обновляли динамически, CF такой же, как выгрузка \
Конфигуратора; с --raw — как конфигурация хранится в базе. На копиях баз типовых конфигураций записи CF совпадают с \
выгрузкой Конфигуратора 8.5.1.
  CF без --server: если копия полная, несжатая, одним файлом, сделана SQL Server 2022 без нагрузки на базу, без \
ускоренного восстановления (ADR) и с контрольными суммами журнала, CF совпадает с CF через сервер (в ответе proven: true). \
Иначе CF всё равно пишется (кроме отклоняемых копий, см. ниже), а в stderr выводится строка предупреждения с причиной: журнал копии в этом режиме не \
применяется. --strict вместо предупреждения отказывает; точный вид даёт --server.
  DT и 1CD — для баз с управляемыми блокировками, в том числе типовых конфигураций на БСП с режимом совместимости \
8.3.14–8.5.1, копии которых записаны платформой 8.5.1. В предварительной проверке на 9 копиях типовых конфигураций DT, \
построенный по этим правилам, совпал с выгрузкой Конфигуратора (/DumpIB) той же копии по содержимому таблиц, и платформа \
его загрузила; приёмка текущей сборки не закончена, поэтому ответ DT и 1CD помечен \"experimental\": true. 1CD — загрузка \
того же DT в новую файловую базу; загрузка открывает базу для проверки под бюджетом --memory-limit, и проверочному \
открытию базам уровня УНФ нужно около 1792 МиБ, уровня ERP около 3 ГиБ.
  Пользователи переносятся. Адреса почты пользователей не переносятся, как и в выгрузке платформы: результат пишется, \
в stderr — предупреждение с числом пользователей с адресом, в ответе — usersWithEmail. В 1CD пользователи конфигурации \
без разделения пользователей пересобираются при загрузке DT; вход таких пользователей платформой ещё не принят.
  Расширения конфигурации (ConfigCAS и блок хранения SchemaStorage SchemaID 1 с таблицами X1) переносятся в DT (число \
таблиц расширений — extensionTables в ответе); в 1CD блок расширений загружается в рамках поддерживаемого профиля восстановления.

Что отклоняется (сообщение называет причину; у отказов DT и 1CD по профилю конфигурации — ещё и код в квадратных \
скобках, например [compatibility_unverified]):
  - разностные, журнальные, шифрованные (в том числе TDE), составные и повреждённые копии — до восстановления, временная \
база не создаётся;
  - без --server: сжатые и зашифрованные копии и копии SQL Server старше 2014;
  - DT и 1CD: неприменённые расширения (ConfigCASSave, незавершённая реструктуризация NGS, схемы расширений кроме SchemaID 1), \
сторонние таблицы SQL (если не задан --ignore-extra-tables; не-dbo схемы и двойники X<N> отклоняются всегда), только старый \
список пользователей (users.usr) при пустой V8USERS, политики паролей и другие служебные данные, которые команда не переносит \
(сообщение перечисляет найденное);
  - DT и 1CD: режим блокировок не управляемый, режим совместимости 8.2.14 и ниже у любой конфигурации \
([compatibility_old]) и вне 8.3.14–8.5.1 у конфигурации с общими реквизитами;
  - 1CD: режим совместимости ниже 8.3.11 (загрузка в 1CD принимает 8.3.11–8.5.1); этот отказ приходит уже после \
выгрузки из SQL Server;
  - DT и 1CD: копия с другой служебной строкой IBVersion; копии, записанные платформой 8.3.x, пока не проверены.
Проверено только на SQL Server 2022.

Подключение к SQL Server.
  --server передаётся sqlcmd как есть (-S): имя или адрес машины, имя\\экземпляр или имя,порт.
  Без --user — доверенное подключение (Windows-аутентификация) под учётной записью, от которой запущен Yellowcode; на Linux \
и macOS без настроенного Kerberos нужен SQL-логин. С --user пароль берётся только из переменной SQLCMDPASSWORD.
  Соединение всегда шифруется; для самоподписанного сертификата (тестовый сервер, Docker) добавьте \
--trust-server-certificate.
  Если sqlcmd нет в PATH, укажите путь через --sqlcmd.

Прерывание и сбои. Имя временной базы — первая строка stderr. Ctrl-C или истёкший срок (одна команда sqlcmd — 30 минут, \
см. --sqlcmd-lifecycle-timeout) останавливают работу: временная база удаляется, результат не публикуется; если удалить её \
не удалось, сообщение называет базу и команду для администратора. После сбоя рядом с --output пишется отчёт \
yellowcode-bak-failure-<время UTC>.log: имена в нём заменены хешами (что скрыто и что нет — https://yellowcode.dev/guide/bak-failure-report/); \
--trace пишет такую трассу и при успехе.

Подробности (проверенные записи подключения, пути с Юникодом, динамическое обновление, части CONFIG, пределы, сигналы, \
очистка вручную, пачки sqlcmd): https://yellowcode.dev/guide/bak-convert/"
    )]
    Convert {
        #[arg(
            help = "Путь к BAK: с --server — на машине SQL Server, без --server — локальный файл"
        )]
        source: String,
        #[arg(
            long,
            help = "Сервер для sqlcmd -S: имя или адрес машины, имя\\экземпляр или имя,порт (см. «Подключение к SQL Server» ниже); без него CF читается из файла напрямую"
        )]
        server: Option<String>,
        #[arg(long, default_value = "1", value_parser = parse_backup_set, help = "Номер набора резервной копии в BAK (позиция из RESTORE HEADERONLY)")]
        backup_set: u32,
        #[arg(long, value_enum, help = "Формат результата")]
        to: Format,
        #[arg(long, help = "Путь к новому файлу на машине с Yellowcode")]
        output: PathBuf,
        #[arg(
            long,
            help = "SQL-логин; пароль только из переменной SQLCMDPASSWORD; без --user — доверенное подключение (Windows-аутентификация)"
        )]
        user: Option<String>,
        #[arg(long, help = "Путь к Microsoft sqlcmd (ODBC); по умолчанию sqlcmd")]
        sqlcmd: Option<PathBuf>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..), help = "Общий срок каждой команды sqlcmd в секундах (по умолчанию 1800 + 120 на остановку); очистка получает отдельный срок")]
        sqlcmd_lifecycle_timeout: Option<u64>,
        #[arg(
            long,
            help = "Доверять сертификату SQL Server без проверки его цепочки (нужно для самоподписанного сертификата, например в Docker)"
        )]
        trust_server_certificate: bool,
        #[arg(
            long,
            value_name = "FILE",
            help = "Записать трассу выполнения в новый файл и при успехе (или задайте переменную YELLOWCODE_TRACE)"
        )]
        trace: Option<PathBuf>,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Только для 1CD: бюджет памяти восстановления DT (auto или B/KiB/MiB/GiB); не ограничивает MS SQL Server"
        )]
        memory_limit: Option<String>,
        #[arg(
            long,
            help = "Только для CF: вернуть CONFIG как он хранится в базе, не применяя остатки динамического обновления"
        )]
        raw: bool,
        #[arg(
            long,
            help = "Только без --server: отказать, если CF нельзя доказать равным восстановленному SQL Server (журнал не применён, ADR, другая версия SQL Server), вместо предупреждения"
        )]
        strict: bool,
        #[arg(
            long,
            help = "Только для DT и 1CD с --server: пропустить сторонние не-1С таблицы в схеме dbo (отсутствующие таблицы 1С и двойники расширений X<N> не пропускаются)"
        )]
        ignore_extra_tables: bool,
    },
}

const TRACE_VARIABLE: &str = "YELLOWCODE_TRACE";

fn direct_conflicts(command: &Command, trace_variable: Option<&std::ffi::OsStr>) -> Vec<String> {
    let Command::Convert {
        server: None,
        backup_set,
        to,
        user,
        sqlcmd,
        sqlcmd_lifecycle_timeout,
        trust_server_certificate,
        trace,
        memory_limit,
        ignore_extra_tables,
        ..
    } = command
    else {
        return Vec::new();
    };
    let mut conflicts = Vec::new();
    if !matches!(to, Format::Cf)
        && let Some(value) = to.to_possible_value()
    {
        conflicts.push(format!("--to {}", value.get_name()));
    }
    if *backup_set != 1 {
        conflicts.push(format!("--backup-set {backup_set}"));
    }
    for (given, name) in [
        (user.is_some(), "--user"),
        (sqlcmd.is_some(), "--sqlcmd"),
        (
            sqlcmd_lifecycle_timeout.is_some(),
            "--sqlcmd-lifecycle-timeout",
        ),
        (*trust_server_certificate, "--trust-server-certificate"),
        (memory_limit.is_some(), "--memory-limit"),
        (*ignore_extra_tables, "--ignore-extra-tables"),
        (trace.is_some(), "--trace"),
        (
            trace_variable.is_some_and(|value| !value.is_empty()),
            TRACE_VARIABLE,
        ),
    ] {
        if given {
            conflicts.push(name.to_owned());
        }
    }
    conflicts
}

impl Command {
    pub(crate) fn usage_error(&self) -> Option<clap::Error> {
        self.usage_error_with(std::env::var_os(TRACE_VARIABLE).as_deref())
    }

    fn usage_error_with(&self, trace_variable: Option<&std::ffi::OsStr>) -> Option<clap::Error> {
        let Command::Convert {
            raw,
            to,
            strict,
            ignore_extra_tables,
            server,
            ..
        } = self
        else {
            return None;
        };
        let message = if let Some(name) = (*raw && !matches!(to, Format::Cf))
            .then(|| to.to_possible_value())
            .flatten()
        {
            format!(
                "the argument '--raw' cannot be used with '--to {}': only a CF has a stored form to return",
                name.get_name()
            )
        } else if *strict && server.is_some() {
            "--strict только без --server: через SQL Server журнал копии применяется, и предупреждать не о чем".to_owned()
        } else if *ignore_extra_tables && server.is_some() && matches!(to, Format::Cf) {
            "--ignore-extra-tables только для --to dt и --to 1cd: для CF таблицы данных SQL не проверяются".to_owned()
        } else {
            let conflicts = direct_conflicts(self, trace_variable);
            if conflicts.is_empty() {
                return None;
            }
            let trace = if conflicts
                .iter()
                .any(|name| name == "--trace" || name == TRACE_VARIABLE)
            {
                "; трасса доступна только с --server"
            } else {
                ""
            };
            format!(
                "без --server CF читается прямо из файла BAK (только --to cf, первый набор), и не допускается: {}{trace}",
                conflicts.join(", ")
            )
        };
        let mut cli = <crate::Cli as clap::CommandFactory>::command();
        cli.build();
        let convert = cli
            .find_subcommand_mut("bak")?
            .find_subcommand_mut("convert")?;
        Some(convert.error(clap::error::ErrorKind::ArgumentConflict, message))
    }
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
            trace: _,
            memory_limit,
            raw,
            strict,
            ignore_extra_tables,
        } => {
            if memory_limit.is_some() && !matches!(to, Format::Onecd) {
                return Err("--memory-limit is only for 1CD conversion".into());
            }
            let cf_options = yellowcode_core::plain::bak::CfOptions {
                view: if raw {
                    yellowcode_core::plain::bak::CfView::Stored
                } else {
                    yellowcode_core::plain::bak::CfView::Applied
                },
            };
            let Some(server) = server else {
                return yellowcode_core::plain::bak::convert_direct(
                    std::path::Path::new(&source),
                    &output,
                    &cf_options,
                    strict,
                    out,
                    err,
                );
            };
            let request = yellowcode_core::plain::bak::SqlConvertRequest {
                backup_path: source,
                server,
                output,
                user,
                sqlcmd: sqlcmd.unwrap_or_else(|| PathBuf::from("sqlcmd")),
                lifecycle_timeout: sqlcmd_lifecycle_timeout.map(std::time::Duration::from_secs),
                trust_server_certificate,
            };
            let dt_options = yellowcode_core::plain::bak::DtOptions {
                ignore_extra_tables,
            };
            match to {
                Format::Cf => yellowcode_core::plain::bak::convert_with_options(
                    request,
                    backup_set,
                    &cf_options,
                    out,
                    err,
                ),
                Format::Dt => yellowcode_core::plain::bak::convert_dt_with_options(
                    request, backup_set, dt_options, out, err,
                ),
                Format::Onecd => yellowcode_core::plain::bak::convert_onecd_with_options(
                    request,
                    memory_limit,
                    backup_set,
                    dt_options,
                    out,
                    err,
                ),
            }
        }
    }
}
