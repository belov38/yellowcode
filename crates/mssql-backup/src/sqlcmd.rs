use std::{
    fmt::Write as _,
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{sql_config, sql_server::SqlPlan};

mod lifecycle;

pub struct Request {
    pub backup_path: String,
    pub server: String,
    pub user: Option<String>,
    pub trust_server_certificate: bool,
    pub sqlcmd: PathBuf,
    pub output: PathBuf,
    pub lifecycle_timeout: Option<Duration>,
}

#[derive(Debug)]
pub struct Report {
    pub rows: usize,
}

#[derive(Debug)]
pub struct DtReport {
    pub tables: usize,
    pub rows: u64,
    pub files: u64,
    pub users: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

fn err(message: impl Into<String>) -> Error {
    Error(message.into())
}

fn valid_arg(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_control)
}

fn token() -> Result<String, Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| err(format!("random task identifier: {e}")))?;
    let mut text = String::with_capacity(32);
    for byte in bytes {
        write!(&mut text, "{byte:02x}").map_err(|e| err(e.to_string()))?;
    }
    Ok(text)
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

struct RunError {
    started: bool,
    message: String,
}

fn redact_diagnostic(bytes: &[u8], secret: &[u8], truncated: bool) -> String {
    let mut redacted = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if !secret.is_empty() && remaining.starts_with(secret) {
            redacted.extend_from_slice(b"[redacted]");
            remaining = remaining.get(secret.len()..).unwrap_or_default();
        } else if let Some((&byte, rest)) = remaining.split_first() {
            redacted.push(byte);
            remaining = rest;
        }
    }
    if truncated && secret.len() > 1 {
        let max = (secret.len() - 1).min(redacted.len());
        if let Some(length) = (1..=max).rev().find(|&length| {
            secret
                .get(..length)
                .is_some_and(|prefix| redacted.ends_with(prefix))
        }) {
            redacted.truncate(redacted.len() - length);
            redacted.extend_from_slice(b"[redacted]");
        }
    }
    String::from_utf8_lossy(&redacted).trim().to_owned()
}

fn diagnostic(bytes: &[u8], truncated: bool) -> String {
    let password = std::env::var("SQLCMDPASSWORD").unwrap_or_default();
    redact_diagnostic(bytes, password.as_bytes(), truncated)
}

fn run<T>(
    request: &Request,
    script: &str,
    consume: impl FnOnce(lifecycle::Reader) -> Result<T, String>,
) -> Result<T, RunError> {
    let mut command = Command::new(&request.sqlcmd);
    command
        .args([
            "-b", "-r1", "-I", "-x", "-w", "65535", "-y", "0", "-f", "65001", "-l", "15", "-t",
            "1800", "-d", "master", "-S",
        ])
        .arg(&request.server)
        .arg("-N");
    if request.trust_server_certificate {
        command.arg("-C");
    }
    match &request.user {
        Some(user) => {
            command.arg("-U").arg(user);
        }
        None => {
            command.arg("-E");
        }
    }
    lifecycle::exchange(request, &mut command, script, consume).map_err(|mut error| {
        if error.started && !error.message.contains("server-side completion/cancellation unproven") {
            error.message.push_str("; SQL client exchange uncertain; root reap unconfirmed; server-side completion/cancellation unproven");
        }
        error
    })
}

fn marker(request: &Request, script: &str, expected: &str) -> Result<(), RunError> {
    run(request, script, |stdout| {
        let mut bytes = Vec::new();
        stdout
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("reading SQL response: {e}"))?;
        if bytes.len() > 8192 || std::str::from_utf8(&bytes).ok().map(str::trim) != Some(expected) {
            return Err("unexpected SQL response".into());
        }
        Ok(())
    })
}

fn cleanup(request: &Request, plan: &SqlPlan) -> Result<(), RunError> {
    marker(request, &plan.cleanup_script(), "YC_BAK_GONE")
}

fn fail_with_cleanup(request: &Request, plan: &SqlPlan, message: String) -> Error {
    let message = format!(
        "{message}; temporary database {} (server-side completion/cancellation unproven)",
        plan.database_name()
    );
    match cleanup(request, plan) {
        Ok(()) => err(message),
        Err(cleanup) => err(format!(
            "{message}; could not safely clean temporary database {}: {}",
            plan.database_name(),
            cleanup.message
        )),
    }
}

pub fn convert_cf(request: &Request, progress: &mut dyn Write) -> Result<Report, Error> {
    convert_cf_with_backup_set(request, 1, progress)
}

pub fn convert_cf_with_backup_set(
    request: &Request,
    backup_set: u32,
    progress: &mut dyn Write,
) -> Result<Report, Error> {
    if !valid_arg(&request.server) || request.user.as_deref().is_some_and(|u| !valid_arg(u)) {
        return Err(err("invalid SQL Server connection argument"));
    }
    if request.user.is_some() && std::env::var_os("SQLCMDPASSWORD").is_none() {
        return Err(err(
            "SQL authentication requires SQLCMDPASSWORD in the environment",
        ));
    }
    match std::fs::symlink_metadata(&request.output) {
        Ok(_) => return Err(err("output already exists")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(err(format!("checking output: {e}"))),
    }
    let plan = SqlPlan::new_with_backup_set(&request.backup_path, &token()?, backup_set)
        .map_err(|e| err(e.to_string()))?;
    let mut output = tempfile::Builder::new()
        .prefix(".yellowcode-")
        .tempfile_in(parent(&request.output))
        .map_err(|e| err(format!("creating output staging file: {e}")))?;
    writeln!(
        progress,
        "yellowcode: SQL temporary database {}",
        plan.database_name()
    )
    .map_err(|e| err(format!("writing progress: {e}")))?;
    if let Err(failure) = marker(request, &plan.restore_script(), "YC_BAK_READY") {
        let message = format!("SQL restore failed: {}", failure.message);
        return Err(if failure.started {
            fail_with_cleanup(request, &plan, message)
        } else {
            err(message)
        });
    }
    let result = (|| {
        let year_offset = run(request, &plan.year_offset_script(), |stdout| {
            sql_config::read_year_offset(stdout).map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL year offset failed: {}", e.message)))?;
        let limits = crate::sql_config_spool::Limits {
            max_files: 1_000_000,
            max_parts: 1024,
            max_file_bytes: 16 * 1024 * 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024 * 1024,
        };
        let builder =
            crate::sql_config_spool::Builder::new_in(year_offset, limits, parent(&request.output))
                .map_err(|e| err(format!("creating CONFIG staging file: {e}")))?;
        let mut config = run(request, &plan.framed_config_script(), |stdout| {
            crate::sql_config_spool::read_framed(stdout, builder, 128 * 1024 * 1024)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL configuration export failed: {}", e.message)))?;
        let rows = config.part_count();
        let mut writer = BufWriter::new(output.as_file_mut());
        config
            .write_cf(&mut writer)
            .map_err(|e| err(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| err(format!("writing CF: {e}")))?;
        Ok(Report { rows })
    })();
    let removed = cleanup(request, &plan);
    if let Err(failure) = removed {
        let cleanup = format!(
            "could not safely clean temporary database {}: {}",
            plan.database_name(),
            failure.message
        );
        return Err(match result {
            Err(export) => err(format!("{export}; {cleanup}")),
            Ok(_) => err(cleanup),
        });
    }
    let report = result?;
    output
        .as_file()
        .sync_all()
        .map_err(|e| err(format!("syncing CF: {e}")))?;
    output
        .persist_noclobber(&request.output)
        .map_err(|e| err(format!("publishing CF: {}", e.error)))?;
    Ok(report)
}

pub fn convert_dt(request: &Request, progress: &mut dyn Write) -> Result<DtReport, Error> {
    convert_dt_inner(request, progress, DtMode::Strict)
}

pub fn convert_dt_with_backup_set(
    request: &Request,
    backup_set: u32,
    progress: &mut dyn Write,
) -> Result<DtReport, Error> {
    convert_dt_inner_owned(
        request,
        progress,
        DtMode::Strict,
        None,
        backup_set,
        None,
        SqlCleanup::Automatic,
    )
}

pub fn convert_dt_framed_research(
    request: &Request,
    progress: &mut dyn Write,
) -> Result<DtReport, Error> {
    convert_dt_inner(request, progress, DtMode::StrictFramed)
}

pub fn convert_dt_for_separated_users(
    request: &Request,
    progress: &mut dyn Write,
) -> Result<DtReport, Error> {
    convert_dt_inner(request, progress, DtMode::Separated)
}

#[derive(Clone, Copy)]
pub enum LegacyUsersLayout {
    Users8,
    UsersSpr11,
}

pub fn convert_dt_for_legacy(
    request: &Request,
    progress: &mut dyn Write,
    users: LegacyUsersLayout,
) -> Result<DtReport, Error> {
    convert_dt_inner(request, progress, DtMode::Legacy(users))
}

pub fn convert_dt_for_legacy_framed(
    request: &Request,
    progress: &mut dyn Write,
    users: LegacyUsersLayout,
) -> Result<DtReport, Error> {
    convert_dt_inner(request, progress, DtMode::LegacyFramed(users))
}

pub fn convert_dt_for_owned_legacy_framed(
    request: &Request,
    progress: &mut dyn Write,
    users: LegacyUsersLayout,
    task_token: &str,
) -> Result<DtReport, Error> {
    convert_dt_inner_owned(
        request,
        progress,
        DtMode::LegacyFramed(users),
        Some(task_token),
        1,
        None,
        SqlCleanup::Automatic,
    )
}

pub fn convert_dt_for_owned_legacy_framed_with_incomplete(
    request: &Request,
    progress: &mut dyn Write,
    users: LegacyUsersLayout,
    task_token: &str,
    incomplete: &Path,
) -> Result<DtReport, Error> {
    validate_research_incomplete_path(request, incomplete)?;
    convert_dt_inner_owned(
        request,
        progress,
        DtMode::LegacyFramed(users),
        Some(task_token),
        1,
        Some(incomplete),
        SqlCleanup::Automatic,
    )
}

pub fn convert_dt_for_owned_legacy_framed_keep_sql(
    request: &Request,
    progress: &mut dyn Write,
    users: LegacyUsersLayout,
    task_token: &str,
    incomplete: &Path,
) -> Result<DtReport, Error> {
    validate_research_incomplete_path(request, incomplete)?;
    convert_dt_inner_owned(
        request,
        progress,
        DtMode::LegacyFramed(users),
        Some(task_token),
        1,
        Some(incomplete),
        SqlCleanup::CallerOwned,
    )
}

pub fn validate_research_incomplete_path(
    request: &Request,
    incomplete: &Path,
) -> Result<(), Error> {
    if !request
        .output
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("dt"))
        || incomplete == request.output
        || incomplete.extension() != Some(std::ffi::OsStr::new("incomplete"))
        || parent(incomplete) != parent(&request.output)
    {
        return Err(err(
            "output must be .dt and incomplete must be a distinct .incomplete file beside it",
        ));
    }
    match std::fs::symlink_metadata(incomplete) {
        Ok(_) => return Err(err("incomplete output already exists")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(err(format!("checking incomplete output: {e}"))),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SqlCleanup {
    Automatic,
    CallerOwned,
}

#[derive(Clone, Copy)]
enum DtMode {
    Strict,
    StrictFramed,
    Separated,
    Legacy(LegacyUsersLayout),
    LegacyFramed(LegacyUsersLayout),
}

fn convert_dt_inner(
    request: &Request,
    progress: &mut dyn Write,
    mode: DtMode,
) -> Result<DtReport, Error> {
    convert_dt_inner_owned(
        request,
        progress,
        mode,
        None,
        1,
        None,
        SqlCleanup::Automatic,
    )
}

fn convert_dt_inner_owned(
    request: &Request,
    progress: &mut dyn Write,
    mode: DtMode,
    task_token: Option<&str>,
    backup_set: u32,
    incomplete: Option<&Path>,
    cleanup_policy: SqlCleanup,
) -> Result<DtReport, Error> {
    use crate::{
        sql_blob, sql_catalog, sql_dt, sql_folder, sql_inventory, sql_metadata, sql_schema_wire,
        sql_support, sql_users,
    };
    use brace::binary::{Atom, BinNode};
    use dtfile::{
        DtWriter, Source, TableHead,
        container::{DtHeader, DtVersion},
    };

    if !valid_arg(&request.server) || request.user.as_deref().is_some_and(|u| !valid_arg(u)) {
        return Err(err("invalid SQL Server connection argument"));
    }
    if request.user.is_some() && std::env::var_os("SQLCMDPASSWORD").is_none() {
        return Err(err(
            "SQL authentication requires SQLCMDPASSWORD in the environment",
        ));
    }
    match std::fs::symlink_metadata(&request.output) {
        Ok(_) => return Err(err("output already exists")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(err(format!("checking output: {e}"))),
    }
    let generated;
    let token = match task_token {
        Some(token) => token,
        None => {
            generated = token()?;
            &generated
        }
    };
    let plan = SqlPlan::new_with_backup_set(&request.backup_path, token, backup_set)
        .map_err(|e| err(e.to_string()))?;
    let mut output = tempfile::Builder::new()
        .prefix(".yellowcode-")
        .tempfile_in(parent(&request.output))
        .map_err(|e| err(format!("creating output staging file: {e}")))?;
    writeln!(
        progress,
        "yellowcode: SQL temporary database {}",
        plan.database_name()
    )
    .map_err(|e| err(format!("writing progress: {e}")))?;
    if task_token.is_none()
        && let Err(failure) = marker(request, &plan.restore_script(), "YC_BAK_READY")
    {
        let message = format!("SQL restore failed: {}", failure.message);
        return Err(if failure.started {
            fail_with_cleanup(request, &plan, message)
        } else {
            err(message)
        });
    }
    let result = (|| {
        let legacy = matches!(mode, DtMode::Legacy(_) | DtMode::LegacyFramed(_));
        let framed = matches!(mode, DtMode::LegacyFramed(_) | DtMode::StrictFramed);
        let schema_script = if legacy {
            plan.legacy_schema_script()
        } else {
            plan.schema_script()
        };
        let schema = run(request, &schema_script, |stdout| {
            if legacy {
                sql_schema_wire::read_schema_with_legacy_indexes(
                    stdout,
                    sql_schema_wire::Limits::default(),
                )
            } else {
                sql_schema_wire::read_schema(stdout, sql_schema_wire::Limits::default())
            }
            .map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL schema export failed: {}", e.message)))?;
        let inventory = run(request, &plan.inventory_script(), |stdout| {
            sql_inventory::read_inventory(stdout, sql_inventory::Limits::default())
                .map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL inventory export failed: {}", e.message)))?;
        if legacy {
            let report = sql_inventory::inspect_legacy_inventory(&schema.schema, &inventory)
                .map_err(|e| err(e.to_string()))?;
            let expected = [
                "BinaryData",
                "BinaryDataStorageContent",
                "BinaryDataStorageVersion",
                "ExternalBinDataStrgsBList",
                "ExternalBinDataStrgsList",
                "V8CMSDPWDS",
                "V8USERPWDPLCS",
                "V8USERSMATKEYS",
            ];
            if report.missing_system_tables != expected {
                return Err(err("unsupported legacy SQL system inventory"));
            }
            for table in report.extra_tables {
                let script = plan
                    .extra_table_presence_script(&table)
                    .map_err(|e| err(e.to_string()))?;
                let present = run(request, &script, |stdout| {
                    sql_inventory::read_extra_table_presence(stdout).map_err(|e| e.to_string())
                })
                .map_err(|e| err(format!("SQL extra table check failed: {}", e.message)))?;
                if present {
                    return Err(err("unsupported nonempty SQL table"));
                }
            }
        } else {
            sql_inventory::validate_inventory(&schema.schema, &inventory)
                .map_err(|e| err(e.to_string()))?;
        }
        let support_script = if legacy {
            plan.legacy_support_script()
        } else {
            plan.support_script()
        };
        let presence = run(request, &support_script, |stdout| {
            sql_support::read_support_for_separated_users(stdout).map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL system data check failed: {}", e.message)))?;
        let strict = matches!(mode, DtMode::Strict | DtMode::StrictFramed);
        let mut staged = if strict {
            let limits = crate::sql_config_spool::Limits {
                max_files: 1_000_000,
                max_parts: 1024,
                max_file_bytes: 16 * 1024 * 1024 * 1024,
                max_total_bytes: 16 * 1024 * 1024 * 1024,
            };
            let builder = crate::sql_config_spool::Builder::new_in(
                schema.year_offset,
                limits,
                parent(&request.output),
            )
            .map_err(|e| err(format!("creating CONFIG staging file: {e}")))?;
            Some(
                run(request, &plan.framed_config_script(), |stdout| {
                    crate::sql_config_spool::read_framed(stdout, builder, 128 * 1024 * 1024)
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| err(format!("SQL configuration export failed: {}", e.message)))?,
            )
        } else {
            None
        };
        let config = if strict {
            None
        } else {
            Some(
                run(request, &plan.export_script(), |stdout| {
                    sql_config::read_sqlcmd_config(stdout, sql_config::Limits::default())
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| err(format!("SQL configuration export failed: {}", e.message)))?,
            )
        };
        let memory_config = || {
            config
                .as_ref()
                .ok_or_else(|| err("missing SQL configuration"))
        };
        let mut strict_compatibility = None;
        let layout = match mode {
            DtMode::Strict | DtMode::StrictFramed => {
                let spool = staged
                    .as_mut()
                    .ok_or_else(|| err("missing CONFIG staging"))?;
                match sql_metadata::strict_compatibility_staged(spool, 512 * 1024 * 1024) {
                    Ok(compatibility) => {
                        strict_compatibility = Some(compatibility);
                        sql_metadata::UsersLayout::Users8
                    }
                    Err(original) => {
                        match sql_metadata::users_layout_staged(spool, 512 * 1024 * 1024) {
                            Ok(sql_metadata::UsersLayout::UsersSpr11) => {
                                sql_metadata::UsersLayout::UsersSpr11
                            }
                            _ => return Err(err(original.to_string())),
                        }
                    }
                }
            }
            DtMode::Separated => sql_metadata::users_layout(memory_config()?, 512 * 1024 * 1024)
                .map_err(|e| err(e.to_string()))?,
            DtMode::Legacy(LegacyUsersLayout::Users8)
            | DtMode::LegacyFramed(LegacyUsersLayout::Users8) => sql_metadata::UsersLayout::Users8,
            DtMode::Legacy(LegacyUsersLayout::UsersSpr11)
            | DtMode::LegacyFramed(LegacyUsersLayout::UsersSpr11) => {
                sql_metadata::UsersLayout::UsersSpr11
            }
        };
        let (compatibility, use_mvcc) = if legacy {
            let facts = sql_metadata::inspect_separation_facts(memory_config()?, 512 * 1024 * 1024)
                .map_err(|e| err(e.to_string()))?;
            if facts.compatibility <= 80214 {
                return Err(err("F2 compatibility is unsupported"));
            }
            let separated = facts.users_separated != 0 || facts.authentication_separated != 0;
            if matches!(layout, sql_metadata::UsersLayout::UsersSpr11) != separated {
                return Err(err(
                    "explicit legacy users layout conflicts with separation facts",
                ));
            }
            if matches!(layout, sql_metadata::UsersLayout::Users8)
                && (presence.users_present || presence.legacy_users_present)
            {
                return Err(err("unsupported legacy ordinary users"));
            }
            if presence.legacy_users_present && !presence.users_present {
                return Err(err("legacy users without user rows"));
            }
            (
                facts.compatibility,
                matches!(
                    facts.data_lock_control_mode,
                    mdconf::DataLockControlMode::Managed
                ),
            )
        } else {
            match layout {
                sql_metadata::UsersLayout::Users8 => {
                    if presence.users_present || presence.legacy_users_present {
                        return Err(err("unsupported SQL system data: users"));
                    }
                    (
                        if strict {
                            strict_compatibility
                                .ok_or_else(|| err("missing verified compatibility"))?
                        } else {
                            sql_metadata::compatibility(memory_config()?, 512 * 1024 * 1024)
                                .map_err(|e| err(e.to_string()))?
                        },
                        true,
                    )
                }
                sql_metadata::UsersLayout::UsersSpr11 => {
                    if presence.legacy_users_present && !presence.users_present {
                        return Err(err("legacy users without user rows"));
                    }
                    (80501, true)
                }
            }
        };
        drop(config);
        let catalog = |table: &str| -> Result<Vec<sql_catalog::SqlColumn>, Error> {
            let script = plan.catalog_script(table).map_err(|e| err(e.to_string()))?;
            run(request, &script, |stdout| {
                sql_catalog::read_catalog(stdout, sql_catalog::Limits::default())
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| err(format!("SQL catalog export failed: {}", e.message)))
        };
        let storage_catalog = if legacy {
            None
        } else {
            Some(catalog("BINARYDATA")?)
        };
        let mut accounting = sql_blob::StorageAccounting::new(100_000);
        let mut buffered = BufWriter::new(output.as_file_mut());
        let mut dt = DtWriter::new(
            &mut buffered,
            DtHeader {
                version: DtVersion::F3,
                ev: false,
            },
        )
        .map_err(|e| err(e.to_string()))?;
        dt.compatibility_mode(i64::from(compatibility))
            .map_err(|e| err(e.to_string()))?;
        for (flag, value) in [
            ("IndexCompatibility", 1),
            ("UseMVCC", if use_mvcc { 1 } else { 0 }),
        ] {
            dt.custom_flag(flag, &BinNode::Atom(Atom::int(value)))
                .map_err(|e| err(e.to_string()))?;
        }
        let mut files = 0_u64;
        for name in [
            "Config",
            "ConfigSave",
            "Params",
            "Files",
            "DepotFiles",
            "ConfigCAS",
            "ConfigCASSave",
        ] {
            let head = TableHead::system(&Source::Folder(name.into()))
                .ok_or_else(|| err("missing DT folder layout"))?;
            let actual = catalog(&head.phys_name)?;
            let columns = sql_folder::source_columns(&head).map_err(|e| err(e.to_string()))?;
            if strict && name == "Config" {
                if actual.len() != columns.len() {
                    return Err(err("SQL folder catalog failed: unexpected column count"));
                }
                sql_catalog::validate_columns(&columns, &actual)
                    .map_err(|e| err(format!("SQL folder catalog failed: {e}")))?;
                let mut source = staged.take().ok_or_else(|| err("missing CONFIG staging"))?;
                let count = sql_folder::write_staged_config(&mut dt, &mut source)
                    .map_err(|e| err(format!("SQL folder export failed: {e}")))?;
                files = files
                    .checked_add(count)
                    .ok_or_else(|| err("folder row count overflow"))?;
                continue;
            }
            let script = if legacy {
                plan.legacy_data_table_script(&head.phys_name, &columns, &actual)
            } else {
                plan.folder_table_script(&head, &actual)
            }
            .map_err(|e| err(e.to_string()))?;
            let count = run(request, &script, |stdout| {
                if legacy {
                    sql_folder::write_legacy_sql_folder(
                        &mut dt,
                        &head,
                        &actual,
                        stdout,
                        schema.year_offset,
                    )
                } else {
                    sql_folder::write_sql_folder_multipart(
                        &mut dt,
                        &head,
                        &actual,
                        stdout,
                        schema.year_offset,
                    )
                }
                .map_err(|e| e.to_string())
            })
            .map_err(|e| err(format!("SQL folder export failed: {}", e.message)))?;
            files = files
                .checked_add(count)
                .ok_or_else(|| err("folder row count overflow"))?;
        }
        let users = match layout {
            sql_metadata::UsersLayout::Users8 => {
                let head = TableHead::system(&Source::Users)
                    .ok_or_else(|| err("missing DT users layout"))?;
                dt.begin_table(&head)
                    .and_then(|()| dt.end_table())
                    .map_err(|e| err(e.to_string()))?;
                0
            }
            sql_metadata::UsersLayout::UsersSpr11 => {
                let head = TableHead::system(&Source::UsersSpr)
                    .ok_or_else(|| err("missing DT separated users layout"))?;
                let actual = catalog(&head.phys_name)?;
                let (columns, email) =
                    sql_users::users_spr_catalog(&actual).map_err(|e| err(e.to_string()))?;
                if email {
                    run(request, &plan.users_email_script(), |stdout| {
                        sql_support::read_empty_user_email(stdout).map_err(|e| e.to_string())
                    })
                    .map_err(|e| err(format!("SQL user email check failed: {}", e.message)))?;
                }
                let script = plan
                    .data_table_script(&head.phys_name, &head.columns, &columns)
                    .map_err(|e| err(e.to_string()))?;
                let count = run(request, &script, |stdout| {
                    sql_users::write_users_spr(&mut dt, &head, stdout, schema.year_offset)
                        .map_err(|e| e.to_string())
                })
                .map_err(|e| err(format!("SQL users export failed: {}", e.message)))?;
                if (count != 0) != presence.users_present {
                    return Err(err("SQL user count mismatch"));
                }
                count
            }
        };
        for source in [Source::PasswordPolicies, Source::CompromisedPasswords] {
            let head = TableHead::system(&source).ok_or_else(|| err("missing DT system layout"))?;
            dt.begin_table(&head)
                .and_then(|()| dt.end_table())
                .map_err(|e| err(e.to_string()))?;
        }
        dt.begin_database(&schema.schema)
            .map_err(|e| err(e.to_string()))?;
        let heads = TableHead::database(&schema.schema).map_err(|e| err(e.to_string()))?;
        let mut rows = 0_u64;
        for (index, head) in heads.iter().enumerate() {
            let actual = catalog(&head.phys_name)?;
            let script = if legacy && framed {
                plan.legacy_framed_data_table_script(&head.phys_name, &head.columns, &actual)
            } else if legacy {
                plan.legacy_data_table_script(&head.phys_name, &head.columns, &actual)
            } else if framed {
                plan.framed_data_table_script(&head.phys_name, &head.columns, &actual)
            } else {
                plan.data_table_script(&head.phys_name, &head.columns, &actual)
            }
            .map_err(|e| err(e.to_string()))?;
            let count = run(request, &script, |stdout| {
                if legacy && framed {
                    return sql_dt::write_framed_sql_table(
                        &mut dt,
                        head,
                        stdout,
                        schema.year_offset,
                    )
                    .map_err(|e| e.to_string());
                }
                if legacy {
                    return sql_dt::write_sql_table(&mut dt, head, stdout, schema.year_offset)
                        .map_err(|e| e.to_string());
                }
                let resolve = |reference: &sql_blob::StorageRef| {
                    let script = plan
                        .blob_script(
                            reference,
                            storage_catalog.as_ref().ok_or("missing storage catalog")?,
                        )
                        .map_err(|e| e.to_string())?;
                    let parts = run(request, &script, |out| {
                        sql_blob::read_blob_parts(out, reference).map_err(|e| e.to_string())
                    })
                    .map_err(|e| e.message)?;
                    let data = accounting
                        .resolve(reference, parts)
                        .map_err(|e| e.to_string())?;
                    Ok(vec![sql_blob::BlobPart {
                        key: reference.key,
                        offset: 0,
                        number: 0,
                        data,
                    }])
                };
                if framed {
                    sql_dt::write_framed_sql_table_with_resolver(
                        &mut dt,
                        head,
                        stdout,
                        schema.year_offset,
                        resolve,
                    )
                } else {
                    sql_dt::write_sql_table_with_resolver(
                        &mut dt,
                        head,
                        stdout,
                        schema.year_offset,
                        resolve,
                    )
                }
                .map_err(|e| e.to_string())
            })
            .map_err(|e| err(format!("SQL data export failed: {}", e.message)))?;
            rows = rows
                .checked_add(count)
                .ok_or_else(|| err("data row count overflow"))?;
            if task_token.is_some() {
                writeln!(
                    progress,
                    "yellowcode: SQL data tables {}/{}; rows {rows}",
                    index + 1,
                    heads.len()
                )
                .map_err(|e| err(format!("writing progress: {e}")))?;
            }
        }
        dt.end_database().map_err(|e| err(e.to_string()))?;
        if !legacy {
            let count = run(request, &plan.blob_count_script(), |out| {
                sql_blob::read_storage_count(out).map_err(|e| e.to_string())
            })
            .map_err(|e| err(format!("SQL binary storage count failed: {}", e.message)))?;
            accounting.finish(count).map_err(|e| err(e.to_string()))?;
        }
        dt.finish().map_err(|e| err(e.to_string()))?;
        buffered
            .flush()
            .map_err(|e| err(format!("writing DT: {e}")))?;
        Ok(DtReport {
            tables: heads.len(),
            rows,
            files,
            users,
        })
    })();
    let result = match cleanup_policy {
        SqlCleanup::CallerOwned => result,
        SqlCleanup::Automatic => match cleanup(request, &plan) {
            Ok(()) => result,
            Err(failure) => {
                let cleanup = format!(
                    "could not safely clean temporary database {}: {}",
                    plan.database_name(),
                    failure.message
                );
                match result {
                    Err(export) => Err(err(format!("{export}; {cleanup}"))),
                    _ => Err(err(cleanup)),
                }
            }
        },
    };
    let report = match result {
        Ok(report) => report,
        Err(failure) => return Err(retain_incomplete(output, incomplete, failure)),
    };
    if let Err(e) = output.as_file().sync_all() {
        return Err(retain_incomplete(
            output,
            incomplete,
            err(format!("syncing DT: {e}")),
        ));
    }
    if let Err(e) = output.persist_noclobber(&request.output) {
        return Err(retain_incomplete(
            e.file,
            incomplete,
            err(format!("publishing DT: {}", e.error)),
        ));
    }
    Ok(report)
}

fn retain_incomplete(file: tempfile::NamedTempFile, path: Option<&Path>, failure: Error) -> Error {
    let Some(path) = path else { return failure };
    if let Err(e) = file.as_file().sync_all() {
        return err(format!("{failure}; could not sync incomplete DT: {e}"));
    }
    if let Err(e) = file.persist_noclobber(path) {
        return err(format!(
            "{failure}; could not retain incomplete DT: {}",
            e.error
        ));
    }
    failure
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn truncated_sql_diagnostics_hide_password_prefixes() {
        let secret = "пароль-secret".as_bytes();
        let mut truncated = b"SQL error: ".to_vec();
        truncated.extend_from_slice(&secret[..7]);
        assert_eq!(
            redact_diagnostic(&truncated, secret, true),
            "SQL error: [redacted]"
        );
        assert_eq!(
            redact_diagnostic(b"failure: abc", b"abcdef", false),
            "failure: abc"
        );
        assert_eq!(
            redact_diagnostic(b"abcdef: abcdef", b"abcdef", false),
            "[redacted]: [redacted]"
        );
        assert_eq!(
            redact_diagnostic(b"failure: abcd", b"abcdef", true),
            "failure: [redacted]"
        );
    }

    #[test]
    fn preserves_the_parser_error_when_stopping_a_rejected_sql_stream() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd");
        std::fs::write(&sqlcmd, b"#!/bin/sh\nprintf invalid\nexec 1>&-\nsleep 3\n").unwrap();
        std::fs::set_permissions(&sqlcmd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: None,
            output: dir.path().join("out.dt"),
        };
        let error = run(&request, "", |stdout| {
            crate::sql_support::read_support(stdout).map_err(|e| e.to_string())
        })
        .err()
        .unwrap();
        assert!(
            error.message.contains("invalid SQL support check output"),
            "{}",
            error.message
        );
    }

    #[test]
    fn reads_sql_output_while_sending_a_large_query() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd");
        std::fs::write(
            &sqlcmd,
            b"#!/usr/bin/env python3\nimport os, signal, sys\nsignal.alarm(4)\nsys.stdout.buffer.write(b'x'*1048576)\nsys.stdout.flush()\nquery = sys.stdin.buffer.read()\nsys.stdout.buffer.write(str(len(query)).encode())\n",
        )
        .unwrap();
        std::fs::set_permissions(&sqlcmd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: None,
            output: dir.path().join("out.dt"),
        };
        let script = "q".repeat(1_048_576);
        let output = run(&request, &script, |mut stdout| {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        })
        .unwrap_or_else(|e| panic!("SQL exchange failed: {}", e.message));
        assert_eq!(output.len(), 1_048_576 + "1048576".len());
        assert!(output[..1_048_576].iter().all(|&byte| byte == b'x'));
        assert_eq!(&output[1_048_576..], b"1048576");
    }

    #[test]
    fn preserves_rejected_output_error_when_query_write_is_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd");
        std::fs::write(
            &sqlcmd,
            b"#!/usr/bin/env python3\nimport signal, sys, time\nsignal.alarm(4)\nsys.stdout.write('invalid')\nsys.stdout.flush()\ntime.sleep(30)\n",
        )
        .unwrap();
        std::fs::set_permissions(&sqlcmd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: None,
            output: dir.path().join("out.dt"),
        };
        let error = run(&request, &"q".repeat(1_048_576), |mut stdout| {
            let mut data = [0; 7];
            stdout.read_exact(&mut data).map_err(|e| e.to_string())?;
            Err::<(), _>("invalid SQL response".into())
        })
        .err()
        .unwrap();
        assert!(
            error.message.contains("invalid SQL response"),
            "{}",
            error.message
        );
    }

    #[test]
    fn interrupted_query_stops_a_client_holding_stdout_open() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd");
        std::fs::write(
            &sqlcmd,
            b"#!/usr/bin/env python3\nimport os, time\nos.close(0)\nwhile True: time.sleep(1)\n",
        )
        .unwrap();
        std::fs::set_permissions(&sqlcmd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: Some(std::time::Duration::from_secs(6)),
            output: dir.path().join("out.dt"),
        };
        let mut completion = None;
        let error = run(&request, &"q".repeat(1_048_576), |mut stdout| {
            completion = Some(lifecycle::test_support::completion(&stdout));
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            Ok(())
        })
        .err()
        .unwrap();
        assert!(
            error.message.contains("sending SQL command"),
            "{}",
            error.message
        );
        completion.unwrap().assert_finished(false);
    }

    #[test]
    fn parser_panic_does_not_wait_for_a_blocked_query_writer() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd");
        std::fs::write(
            &sqlcmd,
            b"#!/usr/bin/env python3\nimport sys, time\nsys.stdout.write('x')\nsys.stdout.flush()\nwhile True: time.sleep(1)\n",
        )
        .unwrap();
        std::fs::set_permissions(&sqlcmd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: Some(std::time::Duration::from_secs(6)),
            output: dir.path().join("out.dt"),
        };
        let mut completion = None;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = run(&request, &"q".repeat(1_048_576), |mut stdout| {
                completion = Some(lifecycle::test_support::completion(&stdout));
                let mut byte = [0; 1];
                stdout.read_exact(&mut byte).unwrap();
                panic!("parser panic control");
                #[allow(unreachable_code)]
                Ok::<(), String>(())
            });
        }));
        assert!(panic.is_err());
        completion.unwrap().assert_finished(false);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn reads_sql_output_while_sending_a_large_query() {
        let dir = tempfile::tempdir().unwrap();
        let sqlcmd = dir.path().join("fake-sqlcmd.cmd");
        std::fs::write(
            &sqlcmd,
            b"@echo off\r\npython.exe \"%~dp0client.py\"\r\nexit /b %errorlevel%\r\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("client.py"),
            b"import os,sys,threading\ntimer=threading.Timer(15,lambda:os._exit(23))\ntimer.start()\nsys.stdout.buffer.write(b'x'*1048576)\nsys.stdout.flush()\nquery=sys.stdin.buffer.read()\nsys.stdout.buffer.write(str(len(query)).encode())\nsys.stdout.flush()\ntimer.cancel()\n",
        )
        .unwrap();
        let request = Request {
            backup_path: "sample.bak".into(),
            server: "local".into(),
            user: None,
            trust_server_certificate: false,
            sqlcmd,
            lifecycle_timeout: None,
            output: dir.path().join("out.dt"),
        };
        let output = run(&request, &"q".repeat(1_048_576), |mut stdout| {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        })
        .unwrap_or_else(|e| panic!("SQL exchange failed: {}", e.message));
        assert_eq!(output.len(), 1_048_576 + "1048576".len());
        assert!(output[..1_048_576].iter().all(|&byte| byte == b'x'));
        assert_eq!(&output[1_048_576..], b"1048576");
    }
}
