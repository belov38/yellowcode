use super::{Request, diag::Retained};
use bak_trace::{CommandShape, Event, Refusal, ScanSummary, StepRecord, TempDbName, Trace};
use std::{
    cell::Cell,
    io,
    process::ExitStatus,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};

const HEAD_BYTES: usize = 8192;

pub(super) fn step_for(script: &str) -> &'static str {
    const KNOWN: &[(&str, &str)] = &[
        ("RESTORE LABELONLY", "restore"),
        ("YC_BAK_NEEDS_SYSADMIN", "remove_unfinished"),
        ("DROP DATABASE", "cleanup"),
        ("MIN(CONVERT(int,[Offset]))", "year_offset"),
        ("FROM dbo.CONFIG ORDER BY", "config_read"),
        ("FROM dbo.CONFIG FOR JSON", "config_read"),
        ("AS [schema_unstable]", "support"),
        ("Email IS NOT NULL", "users_email"),
        ("AS [nonempty]", "extra_table"),
        ("FROM dbo.BINARYDATA FOR JSON", "blob_count"),
        ("dbo.[BINARYDATA] WHERE", "blob"),
        ("FROM dbo.[BINARYDATA] FOR JSON", "blob"),
        ("JOIN sys.columns c", "catalog"),
        ("ORDER BY s.name,t.name", "inventory"),
        ("FROM dbo.DBSCHEMA", "schema"),
        ("COLLATE Latin1_General_100_BIN2", "folder"),
        ("ELSE SELECT N''[]''", "data_table"),
        ("YC_ROWS_BEGIN", "data_table"),
    ];
    KNOWN
        .iter()
        .find(|(marker, _)| script.contains(marker))
        .map_or("other", |(_, step)| step)
}

pub(super) fn command_shape(request: &Request) -> CommandShape {
    CommandShape {
        trusted: request.user.is_none(),
        trust_certificate: request.trust_server_certificate,
        query_timeout_secs: super::query_timeout_seconds(request.lifecycle_timeout),
    }
}

pub(super) fn database_in(script: &str) -> Option<TempDbName> {
    const NAME: usize = "yc_bak_".len() + 32;
    script
        .match_indices("yc_bak_")
        .find_map(|(at, _)| script.get(at..at + NAME).and_then(TempDbName::parse))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RestoreMarkers {
    pub phase: &'static str,
    pub refused: Option<String>,
}

pub(super) fn restore_markers(stdout: &[u8]) -> RestoreMarkers {
    let text = String::from_utf8_lossy(stdout);
    let (mut phase, mut refused) = ("unreached", None);
    for line in text.lines().map(str::trim) {
        match line {
            "YC_BAK_INSPECTING" if phase == "unreached" => phase = "inspecting",
            "YC_BAK_RESTORING" => phase = "restoring",
            _ => {
                if refused.is_none()
                    && let Some(code) = line.strip_prefix("YC_BAK_REFUSED:")
                    && valid_code(code)
                {
                    refused = Some(code.to_owned());
                }
            }
        }
    }
    RestoreMarkers { phase, refused }
}

fn valid_code(code: &str) -> bool {
    let (name, detail) = code
        .split_once(':')
        .map_or((code, None), |(n, d)| (n, Some(d)));
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && detail.is_none_or(|d| digits(d, 9).is_some())
}

fn digits(text: &str, longest: usize) -> Option<u64> {
    (!text.is_empty() && text.len() <= longest && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

pub(super) fn refusal_of(code: &str) -> (&'static str, Option<i64>) {
    let (name, detail) = code.split_once(':').unwrap_or((code, ""));
    let field = match name {
        "not_full" => "not_full",
        "encrypted" => "encrypted",
        "tde" => "tde",
        "striped" => "striped",
        "snapshot" => "snapshot",
        "snapshot_url" => "snapshot_url",
        "damaged" => "damaged",
        "incomplete" => "incomplete",
        "layout" => "layout",
        "header_shape" => "header_shape",
        "not_sysadmin" => "not_sysadmin",
        "set_not_found" => "set_not_found",
        _ => "other",
    };
    (field, digits(detail, 9).and_then(|n| i64::try_from(n).ok()))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CleanupAnswer {
    Gone,
    NotRemoved {
        step: &'static str,
        error: u32,
        megabytes: u64,
    },
    Unknown,
}

pub(super) fn cleanup_answer(stdout: &[u8]) -> CleanupAnswer {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return CleanupAnswer::Unknown;
    };
    if line == "YC_BAK_GONE" {
        return CleanupAnswer::Gone;
    }
    let mut fields = line
        .strip_prefix("YC_BAK_NOT_REMOVED:")
        .unwrap_or_default()
        .splitn(4, ':');
    let step = match fields.next() {
        Some("alter") => "alter",
        Some("drop") => "drop",
        _ => return CleanupAnswer::Unknown,
    };
    let error = fields
        .next()
        .and_then(|f| digits(f, 9))
        .and_then(|n| u32::try_from(n).ok());
    let megabytes = fields.next().and_then(|f| digits(f, 12));
    match (error, megabytes, fields.next()) {
        (Some(error), Some(megabytes), Some(text)) if !text.is_empty() => {
            CleanupAnswer::NotRemoved {
                step,
                error,
                megabytes,
            }
        }
        _ => CleanupAnswer::Unknown,
    }
}

pub(super) struct StdoutTap {
    bytes: AtomicU64,
    lines: AtomicU64,
    keep: usize,
    head: Mutex<Vec<u8>>,
}

impl StdoutTap {
    fn new(keep: usize) -> StdoutTap {
        StdoutTap {
            bytes: AtomicU64::new(0),
            lines: AtomicU64::new(0),
            keep,
            head: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn seen(&self, result: io::Result<usize>, buffer: &[u8]) -> io::Result<usize> {
        if let Ok(read) = &result {
            let chunk = buffer.get(..*read).unwrap_or_default();
            self.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            let lines = chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
            self.lines.fetch_add(lines, Ordering::Relaxed);
            if self.keep > 0 {
                let mut head = self.head.lock().unwrap_or_else(PoisonError::into_inner);
                let room = self.keep.saturating_sub(head.len());
                head.extend_from_slice(chunk.get(..room).unwrap_or(chunk));
            }
        }
        result
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.bytes.load(Ordering::Relaxed),
            self.lines.load(Ordering::Relaxed),
        )
    }

    fn head(&self) -> Vec<u8> {
        self.head
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(super) struct Watch {
    trace: Option<Trace>,
    begun: Instant,
    started_ms: u64,
    step: &'static str,
    nested: bool,
    command: CommandShape,
    database: Option<TempDbName>,
    tap: Arc<StdoutTap>,
    ok: bool,
    started: bool,
    exit: Option<ExitStatus>,
    timed_out: bool,
    stderr: ScanSummary,
}

impl Watch {
    pub(super) fn begin(request: &Request, script: &str) -> Watch {
        let trace = Trace::current();
        let nested = DEPTH.with(|depth| {
            depth.set(depth.get() + 1);
            depth.get() > 1
        });
        if let Some(trace) = &trace {
            trace.mark_sql_started();
        }
        let step = step_for(script);
        let database = database_in(script);
        if let (Some(trace), Some(database)) = (&trace, &database) {
            trace.temp_db(database);
        }
        let keep = if matches!(step, "restore" | "cleanup") {
            HEAD_BYTES
        } else {
            0
        };
        Watch {
            started_ms: trace.as_ref().map_or(0, Trace::elapsed_ms),
            begun: Instant::now(),
            step,
            nested,
            command: command_shape(request),
            database,
            tap: Arc::new(StdoutTap::new(keep)),
            ok: false,
            started: false,
            exit: None,
            timed_out: false,
            stderr: ScanSummary::default(),
            trace,
        }
    }

    pub(super) fn tap(&self) -> Arc<StdoutTap> {
        Arc::clone(&self.tap)
    }

    pub(super) fn set_started(&mut self, started: bool) {
        self.started = started;
    }

    pub(super) fn joined<U>(
        &mut self,
        status: &thread::Result<Result<ExitStatus, String>>,
        timed_out: bool,
        diagnostics: &mut thread::Result<(Retained, U)>,
    ) {
        self.started = true;
        if let Ok(Ok(exit)) = status {
            self.exit = Some(*exit);
        }
        self.timed_out = timed_out;
        if let Ok((kept, _)) = diagnostics {
            self.stderr = kept.scan_summary();
        }
    }

    pub(super) fn succeeded(&mut self) {
        self.ok = true;
    }
}

fn signal_of(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        let Some(trace) = &self.trace else {
            return;
        };
        let (stdout_bytes, stdout_lines) = self.tap.counts();
        trace.step(&StepRecord {
            step: self.step,
            nested: self.nested,
            ok: self.ok,
            started: self.started,
            started_ms: self.started_ms,
            duration_ms: u64::try_from(self.begun.elapsed().as_millis()).unwrap_or(u64::MAX),
            exit_code: self.exit.and_then(|status| status.code()),
            signal: self.exit.as_ref().and_then(signal_of),
            timed_out: self.timed_out,
            stdout_bytes,
            stdout_lines,
            command: self.command,
            stderr: std::mem::take(&mut self.stderr),
            os_error: None,
            io_kind: None,
        });
        match (self.step, &self.database) {
            ("restore", _) => {
                let markers = restore_markers(&self.tap.head());
                trace.emit(Event::new("restore_phase").tag("phase", markers.phase));
                if let Some(code) = &markers.refused {
                    let (field, observed) = refusal_of(code);
                    trace.refusal(&Refusal {
                        class: "backup_refused",
                        field,
                        part: None,
                        expected: None,
                        observed,
                    });
                }
            }
            ("cleanup", Some(database)) => match cleanup_answer(&self.tap.head()) {
                CleanupAnswer::Gone => trace.cleanup(database, None, self.ok, false),
                CleanupAnswer::NotRemoved {
                    step,
                    error,
                    megabytes,
                } => {
                    trace.cleanup(database, Some(megabytes), false, true);
                    trace.cleanup_error(step, error);
                }
                CleanupAnswer::Unknown => trace.cleanup(database, None, false, false),
            },
            _ => {}
        }
    }
}

pub fn failure_kind(message: &str) -> (&'static str, &'static str) {
    const KINDS: &[(&str, &str, &str)] = &[
        ("unsupported backup [", "backup_refused", "inspect"),
        (
            "SQL Server could not read the backup",
            "backup_unreadable",
            "inspect",
        ),
        (
            "SQL restore script stopped before RESTORE",
            "restore_script_stopped",
            "inspect",
        ),
        (
            "SQL command stopped before RESTORE",
            "client_stopped_before_restore",
            "inspect",
        ),
        ("SQL restore failed", "restore_failed", "restore"),
        (
            "SQL year offset failed",
            "year_offset_failed",
            "year_offset",
        ),
        (
            "SQL configuration export failed",
            "config_export_failed",
            "config_read",
        ),
        ("SQL schema export failed", "schema_export_failed", "schema"),
        (
            "SQL inventory export failed",
            "inventory_export_failed",
            "inventory",
        ),
        (
            "SQL system data check failed",
            "support_check_failed",
            "support",
        ),
        (
            "SQL extra table check failed",
            "extra_table_check_failed",
            "extra_table",
        ),
        (
            "SQL catalog export failed",
            "catalog_export_failed",
            "catalog",
        ),
        ("SQL folder export failed", "folder_export_failed", "folder"),
        (
            "SQL folder catalog failed",
            "folder_export_failed",
            "folder",
        ),
        ("SQL users export failed", "users_export_failed", "users"),
        (
            "SQL user email check failed",
            "users_export_failed",
            "users",
        ),
        ("SQL data export failed", "data_export_failed", "data_table"),
        (
            "SQL binary storage count failed",
            "blob_count_failed",
            "blob_count",
        ),
        ("output already exists", "invalid_request", "start"),
        (
            "invalid SQL Server connection argument",
            "invalid_request",
            "start",
        ),
        (
            "SQL authentication requires SQLCMDPASSWORD",
            "invalid_request",
            "start",
        ),
        ("checking output", "invalid_request", "start"),
        ("creating output staging file", "output_failed", "publish"),
        ("creating CONFIG staging file", "output_failed", "publish"),
        ("writing CF", "output_failed", "publish"),
        ("syncing CF", "output_failed", "publish"),
        ("publishing CF", "output_failed", "publish"),
        ("writing DT", "output_failed", "publish"),
        ("syncing DT", "output_failed", "publish"),
        ("publishing DT", "output_failed", "publish"),
        ("could not safely clean", "cleanup_failed", "cleanup"),
        ("1CD conversion failed", "convert_failed", "convert"),
        (
            "dynamic-update leftovers of the CONFIG cannot be applied",
            "dynamic_update_refused",
            "apply",
        ),
        (
            "reading an entry of the dynamic update",
            "dynamic_update_failed",
            "apply",
        ),
        (
            "two CONFIG entries have the same name",
            "dynamic_update_failed",
            "apply",
        ),
        ("writing staged CONFIG as CF", "output_failed", "publish"),
        (
            "DT profile of the configuration is refused",
            "dt_profile_refused",
            "profile",
        ),
    ];
    if message.contains("does not accept the options of the Microsoft ODBC sqlcmd") {
        return ("wrong_client", "connect");
    }
    KINDS
        .iter()
        .find(|(prefix, _, _)| message.starts_with(prefix))
        .map_or(("other", "other"), |(_, class, stage)| (class, stage))
}

pub fn failure_refusal(message: &str) -> Option<(&'static str, &'static str)> {
    if let Some(rest) = message.strip_prefix(crate::sql_metadata::PROFILE_REFUSAL_START) {
        let (code, _) = rest.split_once(']')?;
        let field = crate::sql_metadata::PROFILE_REFUSAL_CODES
            .iter()
            .find(|known| **known == code)
            .copied();
        return Some(("dt_profile", field.unwrap_or("other")));
    }
    const START: &str = "dynamic-update leftovers of the CONFIG cannot be applied [";
    const CODES: &[&str] = &[
        "marker",
        "empty_generation",
        "unlisted_generation",
        "deleted_list",
        "too_many_generations",
        "generation_shape",
        "interplay",
        "deleted_missing",
        "copy_name",
        "multipart_copy",
        "versions_missing",
    ];
    let (code, _) = message.strip_prefix(START)?.split_once(']')?;
    let field = CODES.iter().find(|known| **known == code).copied();
    Some(("dynamic_update", field.unwrap_or("other")))
}
