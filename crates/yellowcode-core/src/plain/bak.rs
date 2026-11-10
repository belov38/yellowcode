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
    publish(&request, &report, out, err)
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

pub fn dynamic_update_notice(summary: &mssql_backup::dynamic_update::Summary) -> String {
    format!(
        "динамическое обновление: применено {} {} (операций: замен {}, добавлений {}, удалений {}); --raw — как хранится",
        summary.generations,
        plural(summary.generations, "поколение", "поколения", "поколений"),
        summary.replaced,
        summary.added,
        summary.deleted,
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
    publish(&request, &report, out, err)
}

fn publish(
    request: &SqlConvertRequest,
    report: &mssql_backup::sqlcmd::Report,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    if let Some(summary) = &report.dynamic_update {
        let _ = writeln!(err, "{}", dynamic_update_notice(summary));
    }
    let status = cf_status(&request.output, report);
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
