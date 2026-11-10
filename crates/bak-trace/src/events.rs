use crate::scan::ScanSummary;
use std::fmt;

#[derive(Clone, Copy, Default)]
pub struct RunShape<'a> {
    pub command: &'static str,
    pub to: &'static str,
    pub backup_set: u32,
    pub trusted_auth: bool,
    pub trust_server_certificate: bool,
    pub lifecycle_timeout_secs: Option<u64>,
    pub memory_limit_given: bool,
    pub raw: Option<bool>,
    pub trace_requested: bool,
    pub source: &'a str,
    pub output: &'a str,
    pub server: &'a str,
    pub user: Option<&'a str>,
    pub sqlcmd: Option<&'a str>,
}

impl fmt::Debug for RunShape<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunShape")
            .field("command", &self.command)
            .field("to", &self.to)
            .field("backup_set", &self.backup_set)
            .field("trusted_auth", &self.trusted_auth)
            .field("trust_server_certificate", &self.trust_server_certificate)
            .field("lifecycle_timeout_secs", &self.lifecycle_timeout_secs)
            .field("memory_limit_given", &self.memory_limit_given)
            .field("raw", &self.raw)
            .field("trace_requested", &self.trace_requested)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandShape {
    pub trusted: bool,
    pub trust_certificate: bool,
    pub query_timeout_secs: u64,
}

#[derive(Debug)]
pub struct StepRecord {
    pub step: &'static str,
    pub nested: bool,
    pub ok: bool,
    pub started: bool,
    pub started_ms: u64,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout_bytes: u64,
    pub stdout_lines: u64,
    pub command: CommandShape,
    pub stderr: ScanSummary,
    pub os_error: Option<i32>,
    pub io_kind: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigStats {
    pub rows: u64,
    pub bytes: u64,
    pub files: u64,
    pub multipart_files: u64,
    pub max_parts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub class: &'static str,
    pub field: &'static str,
    pub part: Option<i64>,
    pub expected: Option<i64>,
    pub observed: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BakHeader {
    pub backup_type: Option<i64>,
    pub compressed: Option<bool>,
    pub encrypted: Option<bool>,
    pub snapshot: Option<bool>,
    pub damaged: Option<bool>,
    pub sets: Option<u64>,
    pub position: Option<u64>,
    pub family_count: Option<u64>,
    pub software_major: Option<i64>,
    pub software_minor: Option<i64>,
    pub software_build: Option<i64>,
    pub database_version: Option<i64>,
    pub compatibility_level: Option<i64>,
    pub backup_bytes: Option<u64>,
    pub compressed_bytes: Option<u64>,
    pub recovery_model: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptKind {
    Sigint,
    Sigterm,
    Sighup,
    ConsoleCtrlC,
    ConsoleBreak,
    ConsoleClose,
    ConsoleLogoff,
    ConsoleShutdown,
    Other,
}

impl InterruptKind {
    pub fn tag(self) -> &'static str {
        match self {
            InterruptKind::Sigint => "sigint",
            InterruptKind::Sigterm => "sigterm",
            InterruptKind::Sighup => "sighup",
            InterruptKind::ConsoleCtrlC => "console_ctrl_c",
            InterruptKind::ConsoleBreak => "console_break",
            InterruptKind::ConsoleClose => "console_close",
            InterruptKind::ConsoleLogoff => "console_logoff",
            InterruptKind::ConsoleShutdown => "console_shutdown",
            InterruptKind::Other => "other",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            InterruptKind::Sigint | InterruptKind::ConsoleCtrlC | InterruptKind::ConsoleBreak => {
                130
            }
            InterruptKind::Sigterm
            | InterruptKind::ConsoleLogoff
            | InterruptKind::ConsoleShutdown => 143,
            InterruptKind::Sighup | InterruptKind::ConsoleClose => 129,
            InterruptKind::Other => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    Interrupted,
    Panic,
}

impl Outcome {
    pub fn tag(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Interrupted => "interrupted",
            Outcome::Panic => "panic",
        }
    }
}
