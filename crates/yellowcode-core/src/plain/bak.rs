use crate::Result;
use std::{fs::File, io::Write, path::Path};

pub use mssql_backup::config_cf::{CfOptions, CfView};
pub use mssql_backup::sqlcmd::{Interrupt, Request as SqlConvertRequest};
pub use mssql_backup::sqlcmd::{failure_kind, failure_refusal};

fn watch_interrupts(err: &mut dyn Write) -> Option<mssql_backup::sqlcmd::InterruptWatch> {
    match mssql_backup::sqlcmd::watch_interrupts() {
        Ok(watch) => Some(watch),
        Err(error) => {
            let _ = writeln!(
                err,
                "yellowcode: cannot watch for interrupts ({error}); an interrupt would not remove the temporary database"
            );
            None
        }
    }
}

pub fn end_process(interrupt: Interrupt) -> ! {
    mssql_backup::sqlcmd::end_process(interrupt)
}

pub fn interrupted(error: &crate::Error) -> Option<Interrupt> {
    error
        .downcast_ref::<mssql_backup::sqlcmd::Error>()?
        .interrupted()
}

pub fn convert(request: SqlConvertRequest, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    convert_with_backup_set(request, 1, out, err)
}

pub fn convert_with_backup_set(
    request: SqlConvertRequest,
    backup_set: u32,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let _watch = watch_interrupts(err);
    let report = mssql_backup::sqlcmd::convert_cf_with_backup_set(&request, backup_set, err)?;
    publish(&request.output, &report, None, out, err)
}

pub fn dynamic_update_notice(summary: &mssql_backup::dynamic_update::Summary) -> String {
    format!(
        "динамические обновления конфигурации применены: {} (операций: замен {}, добавлений {}, удалений {}); --raw — как хранится",
        summary.generations, summary.replaced, summary.added, summary.deleted,
    )
}

fn cf_status(output: &Path, report: &mssql_backup::sqlcmd::Report) -> serde_json::Value {
    let mut status = serde_json::json!({"schemaVersion": 1, "output": output, "format": "cf", "rows": report.rows, "complete": true});
    if let (Some(summary), Some(fields)) = (&report.dynamic_update, status.as_object_mut()) {
        fields.insert(
            "dynamicUpdate".into(),
            serde_json::json!({
                "generations": summary.generations,
                "replaced": summary.replaced,
                "added": summary.added,
                "deleted": summary.deleted,
            }),
        );
    }
    status
}

pub fn convert_with_options(
    request: SqlConvertRequest,
    backup_set: u32,
    options: &CfOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let _watch = watch_interrupts(err);
    let report = mssql_backup::sqlcmd::convert_cf_with_options(&request, backup_set, options, err)?;
    publish(&request.output, &report, None, out, err)
}

pub fn convert_direct(
    source: &Path,
    output: &Path,
    options: &CfOptions,
    strict: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let _ = writeln!(
        err,
        "yellowcode: direct reading of the backup, without SQL Server"
    );
    let mode = if strict {
        mssql_backup::bak_idle::Mode::Strict
    } else {
        mssql_backup::bak_idle::Mode::Warn
    };
    let outcome = match mssql_backup::bak_idle::convert_cf(source, output, *options, mode) {
        Ok(outcome) => outcome,
        Err(error) => {
            if !strict && let Some(records) = mssql_backup::bak_idle::unapplied_log_records(source)
            {
                let _ = writeln!(err, "yellowcode: {}", unapplied_log_line(records));
            }
            return Err(error.into());
        }
    };
    if !outcome.warnings.is_empty() {
        let _ = writeln!(err, "yellowcode: {}", direct_warning_line(&outcome));
    }
    let report = mssql_backup::sqlcmd::Report {
        rows: outcome.report.rows,
        dynamic_update: outcome.report.dynamic_update,
    };
    publish(
        output,
        &report,
        Some(direct_status_fields(&outcome)),
        out,
        err,
    )
}

fn plural(count: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    if (11..=14).contains(&(count % 100)) {
        return many;
    }
    match count % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

fn unapplied_log_line(records: u64) -> String {
    format!(
        "журнал транзакций копии не применён ({records} {}): отказ может быть следствием незавершённых изменений; точный вид — с --server",
        plural(
            usize::try_from(records).unwrap_or(usize::MAX),
            "запись",
            "записи",
            "записей"
        )
    )
}

fn sql_server_name(major: u8) -> String {
    match major {
        12 => "SQL Server 2014".to_owned(),
        13 => "SQL Server 2016".to_owned(),
        14 => "SQL Server 2017".to_owned(),
        15 => "SQL Server 2019".to_owned(),
        16 => "SQL Server 2022".to_owned(),
        17 => "SQL Server 2025".to_owned(),
        other => format!("SQL Server версии {other}"),
    }
}

fn warning_reason(
    scope: &mssql_backup::bak_direct::Scope,
    outcome: &mssql_backup::bak_idle::Outcome,
) -> String {
    use mssql_backup::bak_direct::Scope;
    let records = |records: Option<u64>| match records {
        Some(n) => format!(
            " ({n} {})",
            plural(
                usize::try_from(n).unwrap_or(usize::MAX),
                "запись",
                "записи",
                "записей"
            )
        ),
        None => String::new(),
    };
    match scope {
        Scope::LogWindowNotIdle { .. } => format!(
            "в базу писали во время копирования, журнал транзакций копии не применён{}",
            records(outcome.log_records)
        ),
        Scope::LogNotProven { .. } => match outcome.log_records {
            Some(_) => format!(
                "журнал транзакций копии не применён и не подтверждает прочитанное{}",
                records(outcome.log_records)
            ),
            None => "журнал транзакций копии не применён и не разобран (сколько в нём записей, неизвестно), в базу могли писать во время копирования".to_owned(),
        },
        Scope::NoLogBlockChecksums => {
            "у блоков журнала нет контрольных сумм, журнал проверен только по структуре".to_owned()
        }
        Scope::Adr => {
            let records = outcome.report.config.unreleased_rows;
            let base = "в базе включено ускоренное восстановление (ADR): строки откаченных транзакций могут остаться в результате, а удалённые ими — пропасть";
            if records > 0 {
                format!(
                    "{base} ({records} {} с неосвобождённой версией прочитано как на странице)",
                    plural(usize::try_from(records).unwrap_or(usize::MAX), "запись", "записи", "записей")
                )
            } else {
                base.to_owned()
            }
        }
        Scope::ServerVersion {
            found: Some((major, _)),
            ..
        } => format!(
            "копию сделал {}, прямое чтение проверено только на SQL Server 2022",
            sql_server_name(*major)
        ),
        Scope::ServerVersion { found: None, .. } => {
            "копия не называет версию SQL Server, прямое чтение проверено только на SQL Server 2022"
                .to_owned()
        }
        other => other.to_string(),
    }
}

fn direct_warning_line(outcome: &mssql_backup::bak_idle::Outcome) -> String {
    let reasons: Vec<String> = outcome
        .warnings
        .iter()
        .map(|scope| warning_reason(scope, outcome))
        .collect();
    format!(
        "предупреждение: CF прочитан без SQL Server и может отличаться от восстановленного SQL Server: {}; точный вид — с --server",
        reasons.join("; ")
    )
}

fn warning_detail(scope: &mssql_backup::bak_direct::Scope) -> String {
    use mssql_backup::bak_direct::Scope;
    match scope {
        Scope::LogWindowNotIdle { what } | Scope::LogNotProven { what } => what.clone(),
        Scope::NoLogBlockChecksums => "the log blocks of the backup carry no checksums".to_owned(),
        Scope::Adr => "the database has accelerated database recovery on".to_owned(),
        Scope::ServerVersion {
            found: Some((major, build)),
            ..
        } => format!("made by SQL Server of major version {major} (build {build})"),
        Scope::ServerVersion { found: None, .. } => {
            "the backup does not state the version of the SQL Server that made it".to_owned()
        }
        other => other.to_string(),
    }
}

fn direct_status_fields(
    outcome: &mssql_backup::bak_idle::Outcome,
) -> serde_json::Map<String, serde_json::Value> {
    let warnings: Vec<serde_json::Value> = outcome
        .warnings
        .iter()
        .map(|scope| serde_json::json!({"code": scope.code(), "detail": warning_detail(scope)}))
        .collect();
    let mut fields = serde_json::Map::new();
    fields.insert("reader".into(), "direct".into());
    fields.insert("logRecords".into(), outcome.log_records.into());
    fields.insert("logApplied".into(), false.into());
    fields.insert("proven".into(), outcome.proven.into());
    fields.insert("warnings".into(), warnings.into());
    fields
}

fn publish(
    output: &Path,
    report: &mssql_backup::sqlcmd::Report,
    extra: Option<serde_json::Map<String, serde_json::Value>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    if let Some(summary) = &report.dynamic_update {
        let _ = writeln!(err, "{}", dynamic_update_notice(summary));
    }
    let mut status = cf_status(output, report);
    if let (Some(extra), Some(fields)) = (extra, status.as_object_mut()) {
        fields.extend(extra);
    }
    if let Err(error) = serde_json::to_writer(&mut *out, &status) {
        return if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            Ok(())
        } else {
            Err(error.into())
        };
    }
    match writeln!(out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

pub fn convert_dt(
    request: SqlConvertRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    convert_dt_with_backup_set(request, 1, out, err)
}

pub fn convert_dt_with_backup_set(
    request: SqlConvertRequest,
    backup_set: u32,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let _watch = watch_interrupts(err);
    let report = mssql_backup::sqlcmd::convert_dt_with_backup_set(&request, backup_set, err)?;
    let status = serde_json::json!({"schemaVersion": 1, "output": request.output, "format": "dt", "tables": report.tables, "rows": report.rows, "files": report.files, "complete": true, "experimental": true});
    if let Err(error) = serde_json::to_writer(&mut *out, &status) {
        return if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            Ok(())
        } else {
            Err(error.into())
        };
    }
    match writeln!(out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

pub fn convert_onecd(
    request: SqlConvertRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    convert_onecd_with_memory(request, None, out, err)
}

pub fn convert_onecd_with_memory(
    request: SqlConvertRequest,
    memory_limit: Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    convert_onecd_with_memory_and_backup_set(request, memory_limit, 1, out, err)
}

pub fn convert_onecd_with_memory_and_backup_set(
    mut request: SqlConvertRequest,
    memory_limit: Option<String>,
    backup_set: u32,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    mssql_backup::sql_server::validate_backup_set(backup_set)?;
    let _ = crate::service::memory::Settings::memory_only(memory_limit.clone()).resolve()?;
    match std::fs::symlink_metadata(&request.output) {
        Ok(_) => return Err("output already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let target = request.output.clone();
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".yellowcode-")
        .tempdir_in(parent)?;
    request.output = staging.path().join("source.dt");
    {
        let _watch = watch_interrupts(err);
        mssql_backup::sqlcmd::convert_dt_with_backup_set(&request, backup_set, err)?;
    }
    let mut result = Vec::new();
    let code = crate::plain::convert::run(
        crate::plain::convert::Request {
            source: request.output,
            to: crate::plain::convert::Format::Infobase,
            output: target,
            users_layout: None,
            memory_limit,
            without_users: false,
        },
        &mut result,
        err,
    )?;
    if code != 0 {
        return Err("1CD conversion failed".into());
    }
    let mut status: serde_json::Value = serde_json::from_slice(&result)?;
    label_onecd_status(&mut status);
    if let Err(error) = serde_json::to_writer(&mut *out, &status) {
        return if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            Ok(())
        } else {
            Err(error.into())
        };
    }
    match writeln!(out) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

fn label_onecd_status(status: &mut serde_json::Value) {
    if let Some(fields) = status.as_object_mut() {
        if let Some(hash) = fields.remove("sourceSha256") {
            fields.insert("intermediateDtSha256".into(), hash);
        }
        fields.insert("format".into(), "1cd".into());
        fields.insert("experimental".into(), true.into());
    }
}

pub fn inspect(path: &Path, out: &mut dyn Write) -> Result<()> {
    let media = mssql_backup::media::inspect(&mut File::open(path)?)?;
    let pages = media.regions.iter().try_fold(0_u64, |sum, region| {
        sum.checked_add(region.pages)
            .ok_or("SQL page count overflow")
    })?;
    writeln!(out, "Размер файла: {} байт", media.file_bytes)?;
    writeln!(out, "Областей данных: {}", media.regions.len())?;
    writeln!(out, "Страниц SQL: {pages}")?;
    writeln!(out, "Не воспроизведённый журнал: {} байт", media.log_bytes)?;
    writeln!(
        out,
        "Проверена только структура носителя; данные и возможность восстановления не проверены"
    )?;
    writeln!(
        out,
        "Тип копии (полная или разностная), шифрование TDE и число файлов набора без SQL Server не определяются: их проверяет `bak convert` до восстановления"
    )?;
    Ok(())
}
