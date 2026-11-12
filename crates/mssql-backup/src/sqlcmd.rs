use std::{
    fmt::Write as _,
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    config_cf::CfOptions,
    dynamic_update::Summary,
    sql_config,
    sql_config_spool::{self, Limits},
    sql_server::SqlPlan,
};

pub(crate) const CONFIG_LIMITS: Limits = Limits {
    max_files: 1_000_000,
    max_parts: 1024,
    max_file_bytes: 16 * 1024 * 1024 * 1024,
    max_total_bytes: 16 * 1024 * 1024 * 1024,
};

pub(crate) const CONFIG_ROW_LIMIT: usize = 128 * 1024 * 1024;

mod cleaned;
mod diag;
mod lifecycle;
mod tracing;
mod unfinished;

#[derive(Clone)]
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
    pub dynamic_update: Option<Summary>,
}

#[derive(Debug)]
pub struct DtReport {
    pub tables: usize,
    pub rows: u64,
    pub files: u64,
    pub users: u64,
}

pub use process_containment::interrupt::{Kind as Interrupt, Watch as InterruptWatch};

pub fn end_process(kind: Interrupt) -> ! {
    process_containment::interrupt::end_process(kind)
}

pub fn watch_interrupts() -> io::Result<InterruptWatch> {
    process_containment::interrupt::watch()
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    interrupted: Option<Interrupt>,
}

impl Error {
    pub fn interrupted(&self) -> Option<Interrupt> {
        self.interrupted
    }
}

fn err(message: impl Into<String>) -> Error {
    Error {
        message: message.into(),
        interrupted: None,
    }
}

fn interrupted_error(kind: Interrupt, message: impl Into<String>) -> Error {
    Error {
        message: message.into(),
        interrupted: Some(kind),
    }
}

fn requested() -> Option<Interrupt> {
    process_containment::interrupt::armed()
        .then(process_containment::interrupt::requested)
        .flatten()
}

fn announce(progress: &mut dyn Write, kind: Interrupt, plan: &SqlPlan) {
    let _ = writeln!(
        progress,
        "yellowcode: interrupted by {}; stopping the SQL client and removing the temporary database {}; further signals are ignored until this ends",
        kind.name(),
        plan.database_name()
    );
}

fn valid_arg(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_control)
}

const CLEANUP_BUDGET: Duration = Duration::from_secs(120);

fn cleanup_request(request: &Request) -> Request {
    Request {
        lifecycle_timeout: Some(
            request
                .lifecycle_timeout
                .map_or(CLEANUP_BUDGET, |own| own.min(CLEANUP_BUDGET)),
        ),
        ..request.clone()
    }
}

fn say(progress: &mut dyn Write, text: &str) {
    let _ = writeln!(progress, "yellowcode: {text}");
}

fn cleanup_seconds(request: &Request) -> u64 {
    cleanup_request(request)
        .lifecycle_timeout
        .unwrap_or(CLEANUP_BUDGET)
        .as_secs()
}

fn say_removing(progress: &mut dyn Write, request: &Request, plan: &SqlPlan) {
    say(
        progress,
        &format!(
            "removing the unfinished temporary database {} from the server (waiting at most {} s)",
            plan.database_name(),
            cleanup_seconds(request)
        ),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Failure,
    Success,
}

fn say_checking(progress: &mut dyn Write, request: &Request, plan: &SqlPlan, after: After) {
    let limit = match after {
        After::Failure => format!(" (waiting at most {} s)", cleanup_seconds(request)),
        After::Success => String::new(),
    };
    say(
        progress,
        &format!(
            "checking that the temporary database {} is gone, and removing it if it is not{limit}",
            plan.database_name()
        ),
    );
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

pub use tracing::{failure_kind, failure_refusal};

struct RunError {
    started: bool,
    message: String,
}

const REDACTED: &[u8] = b"[redacted]";

fn covered_by_secret(bytes: &[u8], secret: &[u8], cut_start: bool, cut_end: bool) -> Vec<bool> {
    let mut covered = vec![false; bytes.len()];
    let width = secret.len();
    if width == 0 {
        return covered;
    }
    for start in 0..bytes.len() {
        if bytes
            .get(start..)
            .is_some_and(|rest| rest.starts_with(secret))
        {
            covered
                .iter_mut()
                .skip(start)
                .take(width)
                .for_each(|c| *c = true);
        }
    }
    if cut_start
        && let Some(length) = (1..=width.min(bytes.len()))
            .rev()
            .find(|&length| bytes.get(..length) == secret.get(width - length..))
    {
        covered.iter_mut().take(length).for_each(|c| *c = true);
    }
    if cut_end
        && let Some(length) = (1..=(width - 1).min(bytes.len()))
            .rev()
            .find(|&length| bytes.get(bytes.len() - length..) == secret.get(..length))
    {
        covered
            .iter_mut()
            .skip(bytes.len() - length)
            .for_each(|c| *c = true);
    }
    covered
}

fn redact_cut(bytes: &[u8], secret: &[u8], cut_start: bool, cut_end: bool) -> String {
    let covered = covered_by_secret(bytes, secret, cut_start, cut_end);
    let mut redacted = Vec::with_capacity(bytes.len());
    let mut in_run = false;
    for (&byte, &hidden) in bytes.iter().zip(covered.iter()) {
        if hidden {
            if !in_run {
                redacted.extend_from_slice(REDACTED);
            }
        } else {
            redacted.push(byte);
        }
        in_run = hidden;
    }
    String::from_utf8_lossy(&redacted).trim().to_owned()
}

fn redact_diagnostic(bytes: &[u8], secret: &[u8], truncated: bool) -> String {
    redact_cut(bytes, secret, false, truncated)
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

fn password_bytes() -> Vec<u8> {
    #[cfg(test)]
    if let Some(secret) = TEST_PASSWORD.with(|p| p.borrow().clone()) {
        return secret;
    }
    let Some(value) = std::env::var_os("SQLCMDPASSWORD") else {
        return Vec::new();
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().into_owned().into_bytes()
    }
}

fn diagnostic(bytes: &[u8], truncated: bool) -> String {
    redact_diagnostic(bytes, &password_bytes(), truncated)
}

fn query_timeout_seconds(lifecycle: Option<Duration>) -> u64 {
    let budget = lifecycle.unwrap_or(Duration::from_secs(1920));
    let reserve = Duration::from_secs(120).min(budget / 4);
    (budget - reserve).as_secs().clamp(1, 65_534)
}

fn run<T>(
    request: &Request,
    script: &str,
    consume: impl FnOnce(lifecycle::Reader) -> Result<T, String>,
) -> Result<T, RunError> {
    if !crate::sql_server::lines_fit(script) {
        return Err(RunError {
            started: false,
            message: format!(
                "internal error: a SQL script line is longer than {} characters; nothing was sent to sqlcmd",
                crate::sql_server::SCRIPT_LINE_LIMIT
            ),
        });
    }
    let mut command = Command::new(&request.sqlcmd);
    command
        .args([
            "-b", "-r1", "-I", "-x", "-w", "65535", "-y", "0", "-f", "65001", "-l", "15", "-t",
        ])
        .arg(query_timeout_seconds(request.lifecycle_timeout).to_string())
        .args(["-d", "master", "-S"])
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

enum Restored {
    Ready,
    Refused(String),
}

fn valid_refusal(code: &str) -> bool {
    let (name, detail) = code
        .split_once(':')
        .map_or((code, None), |(n, d)| (n, Some(d)));
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && detail
            .is_none_or(|d| !d.is_empty() && d.len() <= 9 && d.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Unreached,
    Inspecting,
    Restoring,
}

struct Parsed {
    phase: Phase,
    restored: Option<Restored>,
}

fn parse_restore_output(text: &str) -> Parsed {
    let mut phase = Phase::Unreached;
    let mut terminal = None;
    let mut unexpected = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line {
            "YC_BAK_INSPECTING" if terminal.is_none() => {
                phase = phase.max_stage(Phase::Inspecting);
            }
            "YC_BAK_RESTORING" if terminal.is_none() => phase = phase.max_stage(Phase::Restoring),
            other if terminal.is_none() => terminal = Some(other),
            _ => unexpected = true,
        }
    }
    let restored = match terminal {
        _ if unexpected => None,
        Some("YC_BAK_READY") => Some(Restored::Ready),
        Some(other) if phase != Phase::Restoring => other
            .strip_prefix("YC_BAK_REFUSED:")
            .filter(|code| valid_refusal(code))
            .map(|code| Restored::Refused(code.to_owned())),
        _ => None,
    };
    Parsed { phase, restored }
}

impl Phase {
    fn max_stage(self, other: Phase) -> Phase {
        if (other as u8) > (self as u8) {
            other
        } else {
            self
        }
    }
}

fn restore(request: &Request, plan: &SqlPlan) -> Result<Restored, (RunError, Phase)> {
    let phase = std::cell::Cell::new(Phase::Unreached);
    run(request, &plan.restore_script(), |stdout| {
        let mut bytes = Vec::new();
        stdout
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("reading SQL response: {e}"))?;
        let text = if bytes.len() > 8192 {
            None
        } else {
            std::str::from_utf8(&bytes).ok()
        };
        let parsed = text.map_or(
            Parsed {
                phase: Phase::Unreached,
                restored: None,
            },
            parse_restore_output,
        );
        phase.set(parsed.phase);
        parsed
            .restored
            .ok_or_else(|| "unexpected SQL response".to_owned())
    })
    .map_err(|failure| (failure, phase.get()))
}

fn unreadable_hint(diagnostics: &str) -> &'static str {
    let has = |number: &str| diagnostics.contains(&format!("Msg {number},"));
    if ["3287", "3241", "3242", "3243"].iter().any(|n| has(n)) {
        "the file is not a complete, readable SQL Server backup (truncated, damaged or not a backup); "
    } else if has("3201") {
        "SQL Server cannot open the file (check that the path exists on the SQL Server machine and that its service account can read it); "
    } else if has("3203") {
        "SQL Server hit a read error on the file; "
    } else {
        ""
    }
}

fn is_own_check_failure(detail: &str) -> bool {
    detail.contains("Msg 51000,")
        && [
            "temporary database already exists",
            "unsupported server data path",
        ]
        .iter()
        .any(|text| detail.contains(text))
}

fn has_server_error(text: &str) -> bool {
    text.match_indices("Msg ").any(|(at, _)| {
        let rest = text.get(at + 4..).unwrap_or_default();
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0
            && rest
                .get(digits..)
                .is_some_and(|after| after.starts_with(','))
    })
}

fn login_refused(text: &str) -> bool {
    text.contains("Login failed") || text.contains("Login timeout expired")
}

fn inspection_failure(detail: &str) -> String {
    if is_own_check_failure(detail) {
        format!("SQL restore script stopped before RESTORE: {detail}")
    } else if detail.contains("Msg 213,") {
        format!(
            "SQL restore script stopped before RESTORE: this SQL Server returns a result layout that Yellowcode does not expect (verified: SQL Server 2022); {detail}"
        )
    } else if has_server_error(detail) {
        format!(
            "SQL Server could not read the backup: {}{detail}",
            unreadable_hint(detail)
        )
    } else {
        format!("SQL command stopped before RESTORE, which says nothing about the backup: {detail}")
    }
}

fn fail_unreadable(
    request: &Request,
    plan: &SqlPlan,
    raw: &str,
    progress: &mut dyn Write,
) -> Error {
    let server_answered = has_server_error(raw);
    let detail = if server_answered {
        raw.replace(
            "; SQL client exchange uncertain; server-side completion/cancellation unproven",
            "",
        )
    } else {
        raw.to_owned()
    };
    let message = inspection_failure(&detail);
    say_checking(progress, request, plan, After::Failure);
    match cleanup(request, plan) {
        Ok(()) if server_answered => err(format!("{message}; no temporary database remains")),
        Ok(()) => err(format!(
            "{message}; when the cleanup ran, no temporary database remained"
        )),
        Err(cleanup) => err(format!(
            "{message}; could not safely clean temporary database {}: {}",
            plan.database_name(),
            cleaned::complete(plan.database_name(), &cleanup.message)
        )),
    }
}

const NOT_SYSADMIN_MESSAGE: &str = "the login must have the sysadmin role on the test server: the product restores a temporary database and must be able to remove it; no temporary database was created";

fn refusal_message(code: &str, backup_set: u32) -> String {
    if code == "not_sysadmin" {
        return NOT_SYSADMIN_MESSAGE.to_owned();
    }
    let (name, detail) = code.split_once(':').unwrap_or((code, ""));
    let reason = match name {
        "not_full" => format!(
            "backup set {backup_set} is not a full database backup (differential, log, file and partial backups cannot be restored on their own)"
        ),
        "encrypted" => "the backup is encrypted".to_owned(),
        "tde" => "the database uses transparent data encryption (TDE)".to_owned(),
        "striped" => "the backup is striped across several files or devices".to_owned(),
        "snapshot" | "snapshot_url" => "the backup is a snapshot backup".to_owned(),
        "damaged" => "the backup is marked as damaged".to_owned(),
        "incomplete" => {
            "the backup has incomplete metadata (for example a tail-log backup)".to_owned()
        }
        "layout" => "the backup set must hold exactly one data file and one log file, all present in the set".to_owned(),
        "header_shape" => "this SQL Server returns a backup header layout that Yellowcode has not been verified with (verified: SQL Server 2022)".to_owned(),
        "set_not_found" => format!("backup set {backup_set} is not on the media (the media holds {detail} sets)"),
        _ => "the backup cannot be restored by this command".to_owned(),
    };
    format!("unsupported backup [{code}]: {reason}; no temporary database was created")
}

fn cleanup(request: &Request, plan: &SqlPlan) -> Result<(), RunError> {
    cleanup_with(&cleanup_request(request), plan)
}

fn cleanup_after_success(request: &Request, plan: &SqlPlan) -> Result<(), RunError> {
    cleanup_with(request, plan)
}

fn final_cleanup(
    request: &Request,
    plan: &SqlPlan,
    worked: bool,
    progress: &mut dyn Write,
) -> Result<(), RunError> {
    if worked {
        say_checking(progress, request, plan, After::Success);
        cleanup_after_success(request, plan)
    } else {
        say_checking(progress, request, plan, After::Failure);
        cleanup(request, plan)
    }
}

fn cleanup_with(request: &Request, plan: &SqlPlan) -> Result<(), RunError> {
    let _cleaning = lifecycle::OrdinaryCleanup::enter(
        plan.database_name(),
        request
            .lifecycle_timeout
            .unwrap_or(Duration::from_secs(1920))
            .as_secs(),
    );
    let wait = unfinished::wait_seconds(query_timeout_seconds(request.lifecycle_timeout));
    let answer = run(request, &plan.cleanup_script_with_wait(wait), |stdout| {
        let mut bytes = Vec::new();
        stdout
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("reading SQL response: {e}"))?;
        if bytes.len() > 8192 {
            return Err("unexpected SQL response".to_owned());
        }
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(cleaned::parse_answer)
            .ok_or_else(|| "unexpected SQL response".to_owned())
    })?;
    match answer {
        cleaned::Cleaned::Gone => Ok(()),
        cleaned::Cleaned::NotRemoved(failed) => Err(RunError {
            started: true,
            message: cleaned::message(plan.database_name(), &failed),
        }),
    }
}

fn fail_with_cleanup(
    request: &Request,
    plan: &SqlPlan,
    message: &str,
    skip_removal: bool,
    progress: &mut dyn Write,
) -> String {
    let started = Instant::now();
    let window = cleanup_request(request)
        .lifecycle_timeout
        .unwrap_or(CLEANUP_BUDGET);
    let mut again = false;
    loop {
        let step = if skip_removal {
            Ok(unfinished::Step::NotTried)
        } else {
            if !again {
                say_removing(progress, request, plan);
            }
            unfinished::attempt(request, plan)
        };
        if !again {
            say_checking(progress, request, plan, After::Failure);
        }
        let confirmed = cleanup(request, plan).map_err(|failure| failure.message);
        if still_recovering(&step) && started.elapsed() < window && !lifecycle::stop_waiting() {
            if !again {
                say(
                    progress,
                    &format!(
                        "the temporary database {} is still being recovered by the server; trying again (for at most {} s in all)",
                        plan.database_name(),
                        window.as_secs()
                    ),
                );
                again = true;
            }
            std::thread::sleep(RECOVERY_PAUSE);
            continue;
        }
        return unfinished::verdict(message, plan.database_name(), &step, &confirmed);
    }
}

const RECOVERY_PAUSE: Duration = Duration::from_secs(1);

fn still_recovering(step: &Result<unfinished::Step, String>) -> bool {
    matches!(step, Ok(unfinished::Step::Kept(state)) if state == "RECOVERING")
}

fn settle(error: Error) -> Error {
    if error.interrupted.is_some() {
        return error;
    }
    match requested() {
        Some(kind) => interrupted_error(
            kind,
            format!("interrupted by {}; {}", kind.name(), error.message),
        ),
        None => error,
    }
}

fn interrupted_restore(
    request: &Request,
    plan: &SqlPlan,
    kind: Interrupt,
    started: bool,
    progress: &mut dyn Write,
) -> Error {
    let base = format!("interrupted by {}", kind.name());
    if !started {
        return interrupted_error(kind, format!("{base}; no temporary database was created"));
    }
    announce(progress, kind, plan);
    let _after_interrupt = lifecycle::InterruptCleanup::enter();
    interrupted_error(
        kind,
        fail_with_cleanup(request, plan, &base, false, progress),
    )
}

fn interrupted_export(
    request: &Request,
    plan: &SqlPlan,
    kind: Interrupt,
    progress: &mut dyn Write,
    caller_keeps: bool,
) -> Error {
    let base = format!("interrupted by {}", kind.name());
    if caller_keeps {
        return interrupted_error(
            kind,
            format!(
                "{base}; temporary database {} was left to the caller",
                plan.database_name()
            ),
        );
    }
    announce(progress, kind, plan);
    let _after_interrupt = lifecycle::InterruptCleanup::enter();
    let removed = final_cleanup(request, plan, false, progress).map_err(|failure| failure.message);
    interrupted_error(
        kind,
        unfinished::verdict(
            &base,
            plan.database_name(),
            &Ok(unfinished::Step::NotTried),
            &removed,
        ),
    )
}

fn wrong_client_message(request: &Request, failure: &RunError) -> Option<String> {
    let said = failure
        .message
        .split("sqlcmd diagnostics: ")
        .nth(1)
        .unwrap_or(&failure.message)
        .split("; sqlcmd exited")
        .next()
        .unwrap_or_default();
    said.contains("Unknown Option").then(|| {
        format!(
            "{} does not accept the options of the Microsoft ODBC sqlcmd that Yellowcode uses (go-sqlcmd is one such program); install the ODBC sqlcmd (mssql-tools18) and pass its path with --sqlcmd; the client said: {said}",
            request.sqlcmd.display()
        )
    })
}

fn restore_failure_message(failure: &RunError) -> String {
    format!(
        "SQL restore failed: {}{}",
        unreadable_hint(&failure.message),
        failure.message
    )
}

fn restore_or_explain(
    request: &Request,
    plan: &SqlPlan,
    backup_set: u32,
    progress: &mut dyn Write,
) -> Result<(), Error> {
    let outcome = restore(request, plan);
    if let Err((failure, _)) = &outcome
        && let Some(kind) = requested()
    {
        return Err(interrupted_restore(
            request,
            plan,
            kind,
            failure.started,
            progress,
        ));
    }
    match outcome {
        Ok(Restored::Ready) => Ok(()),
        Ok(Restored::Refused(code)) => Err(err(refusal_message(&code, backup_set))),
        Err((failure, Phase::Inspecting)) if failure.started => Err(settle(fail_unreadable(
            request,
            plan,
            &failure.message,
            progress,
        ))),
        Err((failure, Phase::Unreached)) if wrong_client_message(request, &failure).is_some() => {
            Err(err(
                wrong_client_message(request, &failure).unwrap_or_default()
            ))
        }
        Err((failure, phase)) => {
            let message = restore_failure_message(&failure);
            let never_logged_in = phase == Phase::Unreached && login_refused(&failure.message);
            Err(settle(err(if failure.started {
                fail_with_cleanup(request, plan, &message, never_logged_in, progress)
            } else {
                message
            })))
        }
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
    convert_cf_with_options(request, backup_set, &CfOptions::default(), progress)
}

pub fn convert_cf_with_options(
    request: &Request,
    backup_set: u32,
    options: &CfOptions,
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
    restore_or_explain(request, &plan, backup_set, progress)?;
    let result = (|| {
        let year_offset = run(request, &plan.year_offset_script(), |stdout| {
            sql_config::read_year_offset(stdout).map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL year offset failed: {}", e.message)))?;
        let builder =
            sql_config_spool::Builder::new_in(year_offset, CONFIG_LIMITS, parent(&request.output))
                .map_err(|e| err(format!("creating CONFIG staging file: {e}")))?;
        let mut config = run(request, &plan.framed_config_script(), |stdout| {
            sql_config_spool::read_framed(stdout, builder, CONFIG_ROW_LIMIT)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL configuration export failed: {}", e.message)))?;
        let rows = config.part_count();
        let mut writer = BufWriter::new(output.as_file_mut());
        let dynamic_update = config
            .write_cf_with(options, &mut writer)
            .map_err(|e| err(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| err(format!("writing CF: {e}")))?;
        Ok(Report {
            rows,
            dynamic_update,
        })
    })();
    if let Some(kind) = requested() {
        return Err(interrupted_export(request, &plan, kind, progress, false));
    }
    let removed = final_cleanup(request, &plan, result.is_ok(), progress);
    if let Err(failure) = removed {
        let cleanup = format!(
            "could not safely clean temporary database {}: {}",
            plan.database_name(),
            cleaned::complete(plan.database_name(), &failure.message)
        );
        return Err(settle(match result {
            Err(export) => err(format!("{export}; {cleanup}")),
            Ok(_) => err(cleanup),
        }));
    }
    if let Some(kind) = requested() {
        return Err(interrupted_error(
            kind,
            match result {
                Ok(_) => format!(
                    "interrupted by {}; temporary database {} (cleanup confirmed that it is gone); nothing was published",
                    kind.name(),
                    plan.database_name()
                ),
                Err(export) => format!(
                    "interrupted by {}; {export}; temporary database {} (cleanup confirmed that it is gone)",
                    kind.name(),
                    plan.database_name()
                ),
            },
        ));
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

const USERS_WITHOUT_SEPARATION: &str = "SQL system data check failed: в базе есть пользователи информационной базы (таблица V8USERS или файл users.usr не пусты), а конфигурация не разделяет пользователей; DT для такой базы пока не поддерживается";

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
    if task_token.is_none() {
        restore_or_explain(request, &plan, backup_set, progress)?;
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
            sql_support::read_support_findings(stdout).map_err(|e| e.to_string())
        })
        .map_err(|e| err(format!("SQL system data check failed: {}", e.message)))?
        .map_err(|found| err(format!("SQL system data check failed: {found}")))?;
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
                let profile =
                    sql_metadata::dt_profile_staged(spool, sql_metadata::PROFILE_BUDGET_BYTES)
                        .map_err(|e| err(e.to_string()))?;
                strict_compatibility = Some(profile.compatibility);
                profile.layout
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
                        return Err(err(USERS_WITHOUT_SEPARATION));
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
                    (
                        if strict {
                            strict_compatibility
                                .ok_or_else(|| err("missing verified compatibility"))?
                        } else {
                            80501
                        },
                        true,
                    )
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
        let heads = TableHead::database(&schema.schema).map_err(|e| err(e.to_string()))?;
        let tables = if legacy {
            None
        } else {
            let names: Vec<&str> = std::iter::once("BINARYDATA")
                .chain(heads.iter().map(|head| head.phys_name.as_str()))
                .collect();
            let script = plan
                .catalog_batch_script(&names)
                .map_err(|e| err(e.to_string()))?;
            let mut tables = run(request, &script, |stdout| {
                sql_catalog::read_catalog_batch(stdout, names.len(), sql_catalog::Limits::default())
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| err(format!("SQL catalog export failed: {}", e.message)))?
            .into_iter();
            let storage = tables
                .next()
                .ok_or_else(|| err("missing storage catalog"))?;
            Some((storage.columns, tables.collect::<Vec<_>>()))
        };
        let mut accounting = sql_blob::StorageAccounting::new(100_000);
        let mut storage = None;
        let storage_rows = match &tables {
            Some((storage_catalog, _)) => {
                let count = run(request, &plan.blob_count_script(), |out| {
                    sql_blob::read_storage_count(out).map_err(|e| e.to_string())
                })
                .map_err(|e| err(format!("SQL binary storage count failed: {}", e.message)))?;
                if count != 0 {
                    let script = plan
                        .blob_spool_script(storage_catalog)
                        .map_err(|e| err(e.to_string()))?;
                    let spool = run(request, &script, |out| {
                        sql_blob::read_spool(out, parent(&request.output), 100_000)
                            .map_err(|e| e.to_string())
                    })
                    .map_err(|e| {
                        err(format!(
                            "SQL data export failed: binary storage: {}",
                            e.message
                        ))
                    })?;
                    if spool.rows() != count {
                        return Err(err(
                            "SQL data export failed: binary storage: the rows differ from the storage count",
                        ));
                    }
                    storage = Some(spool);
                }
                Some(count)
            }
            None => None,
        };
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
                        sql_support::read_email_findings(stdout).map_err(|e| e.to_string())
                    })
                    .map_err(|e| err(format!("SQL user email check failed: {}", e.message)))?
                    .map_err(|found| err(format!("SQL user email check failed: {found}")))?;
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
        let mut rows = 0_u64;
        let mut resolve = |reference: &sql_blob::StorageRef| {
            let parts = match storage.as_mut() {
                Some(spool) => spool.parts(reference).map_err(|e| e.to_string())?,
                None => Vec::new(),
            };
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
        if let (Some((_, catalogs)), false) = (&tables, framed) {
            let estimates: Vec<u64> = catalogs
                .iter()
                .map(|table| table.pages.saturating_mul(8192))
                .collect();
            for batch in crate::sql_batch::plan(&estimates, crate::sql_batch::LIMITS) {
                let members: Vec<(u64, &TableHead, &sql_catalog::TableCatalog)> = heads
                    .iter()
                    .zip(catalogs)
                    .enumerate()
                    .skip(batch.start)
                    .take(batch.len())
                    .map(|(index, (head, table))| (index as u64 + 1, head, table))
                    .collect();
                let script = plan
                    .data_batch_script(
                        &members
                            .iter()
                            .map(|(index, head, table)| crate::sql_batch::Table {
                                index: *index,
                                name: &head.phys_name,
                                columns: &head.columns,
                                actual: &table.columns,
                            })
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|e| err(e.to_string()))?;
                let count = run(request, &script, |stdout| {
                    let mut frames = crate::sql_rows::Frames::new(stdout);
                    let mut count = 0_u64;
                    for (index, head, _) in &members {
                        let at = |e: String| {
                            format!("table {index} of {} ({}): {e}", heads.len(), head.phys_name)
                        };
                        crate::sql_batch::begin(&mut frames, *index)
                            .map_err(|e| at(e.to_string()))?;
                        let rows = sql_dt::write_sql_section_with_resolver(
                            &mut dt,
                            head,
                            &mut frames,
                            schema.year_offset,
                            &mut resolve,
                        )
                        .map_err(|e| at(e.to_string()))?;
                        crate::sql_batch::end(&mut frames, *index, rows)
                            .map_err(|e| at(e.to_string()))?;
                        count += rows;
                    }
                    frames.end().map_err(|e| e.to_string())?;
                    Ok(count)
                })
                .map_err(|e| err(format!("SQL data export failed: {}", e.message)))?;
                rows = rows
                    .checked_add(count)
                    .ok_or_else(|| err("data row count overflow"))?;
            }
        }
        let per_table = if tables.is_none() || framed {
            heads.len()
        } else {
            0
        };
        for (index, head) in heads.iter().enumerate().take(per_table) {
            let actual = match &tables {
                None => catalog(&head.phys_name)?,
                Some((_, catalogs)) => catalogs
                    .get(index)
                    .ok_or_else(|| err("missing SQL catalog"))?
                    .columns
                    .clone(),
            };
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
                if framed {
                    sql_dt::write_framed_sql_table_with_resolver(
                        &mut dt,
                        head,
                        stdout,
                        schema.year_offset,
                        &mut resolve,
                    )
                } else {
                    sql_dt::write_sql_table_with_resolver(
                        &mut dt,
                        head,
                        stdout,
                        schema.year_offset,
                        &mut resolve,
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
        drop(resolve);
        if let Some(count) = storage_rows {
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
    let result = match (cleanup_policy, requested()) {
        (policy, Some(kind)) => Err(interrupted_export(
            request,
            &plan,
            kind,
            progress,
            matches!(policy, SqlCleanup::CallerOwned),
        )),
        (SqlCleanup::CallerOwned, None) => result,
        (SqlCleanup::Automatic, None) => {
            match final_cleanup(request, &plan, result.is_ok(), progress) {
                Ok(()) => match (requested(), result) {
                    (Some(kind), Ok(_)) => Err(interrupted_error(
                        kind,
                        format!(
                            "interrupted by {}; temporary database {} (cleanup confirmed that it is gone); nothing was published",
                            kind.name(),
                            plan.database_name()
                        ),
                    )),
                    (Some(kind), Err(export)) => Err(interrupted_error(
                        kind,
                        format!(
                            "interrupted by {}; {export}; temporary database {} (cleanup confirmed that it is gone)",
                            kind.name(),
                            plan.database_name()
                        ),
                    )),
                    (None, result) => result,
                },
                Err(failure) => {
                    let cleanup = format!(
                        "could not safely clean temporary database {}: {}",
                        plan.database_name(),
                        cleaned::complete(plan.database_name(), &failure.message)
                    );
                    match result {
                        Err(export) => Err(settle(err(format!("{export}; {cleanup}")))),
                        _ => Err(settle(err(cleanup))),
                    }
                }
            }
        }
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
    let noted = |failure: &Error, note: String| Error {
        message: format!("{failure}; {note}"),
        interrupted: failure.interrupted,
    };
    if let Err(e) = file.as_file().sync_all() {
        return noted(&failure, format!("could not sync incomplete DT: {e}"));
    }
    if let Err(e) = file.persist_noclobber(path) {
        return noted(
            &failure,
            format!("could not retain incomplete DT: {}", e.error),
        );
    }
    failure
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn retaining_an_incomplete_dt_keeps_an_interrupt_an_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("taken");
        std::fs::write(&taken, b"x").unwrap();
        let file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let cut = interrupted_error(Interrupt::Interrupt, "interrupted by SIGINT; cut");
        let error = retain_incomplete(file, Some(&taken), cut);
        assert_eq!(error.interrupted(), Some(Interrupt::Interrupt));
        assert!(
            error
                .to_string()
                .starts_with("interrupted by SIGINT; cut; could not retain incomplete DT: "),
            "{error}"
        );
        assert_eq!(std::fs::read(&taken).unwrap(), b"x");
    }

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
        let script = megabyte();
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
        let error = run(&request, &megabyte(), |mut stdout| {
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
        let error = run(&request, &megabyte(), |mut stdout| {
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
            let _ = run(&request, &megabyte(), |mut stdout| {
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
        let output = run(&request, &megabyte(), |mut stdout| {
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
