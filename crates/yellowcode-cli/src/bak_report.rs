use crate::bak::{Command, Format};
use bak_trace::{Destination, LiteralKind, Outcome, RunShape, Scope, Trace};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
};

const TRACE_VARIABLE: &str = "YELLOWCODE_TRACE";

const COMMIT: &str = match option_env!("YC_BUILD_COMMIT") {
    Some(commit) if !commit.is_empty() => commit,
    _ => "unknown",
};

pub(crate) struct Session {
    trace: Trace,
    _scope: Scope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Flag,
    Variable,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Flag => "--trace",
            Source::Variable => TRACE_VARIABLE,
        }
    }
}

fn requested(flag: Option<&Path>, variable: Option<OsString>) -> Option<(PathBuf, Source)> {
    if let Some(path) = flag {
        return Some((path.to_path_buf(), Source::Flag));
    }
    variable
        .filter(|value| !value.is_empty())
        .map(|value| (PathBuf::from(value), Source::Variable))
}

fn format_tag(format: Format) -> &'static str {
    match format {
        Format::Cf => "cf",
        Format::Dt => "dt",
        Format::Onecd => "1cd",
    }
}

fn unusable(source: Source, path: &Path, error: &io::Error) -> String {
    let failure = bak_trace::WriteFailure::of(error);
    let number = failure
        .os_error
        .map_or_else(|| "без номера".to_owned(), |number| number.to_string());
    format!(
        "{}: файл трассы не создан (ошибка ОС: {number}, {}): {}; укажите новый файл",
        source.name(),
        failure.kind,
        path.display()
    )
}

pub(crate) fn begin(command: Option<&crate::Command>) -> Result<Option<Session>, String> {
    let Some(crate::Command::Bak(Command::Convert {
        source,
        server,
        backup_set,
        to,
        output,
        user,
        sqlcmd,
        sqlcmd_lifecycle_timeout,
        trust_server_certificate,
        trace: trace_flag,
        memory_limit,
        raw,
        strict: _,
    })) = command
    else {
        return Ok(None);
    };
    let Some(server) = server else {
        return Ok(None);
    };
    let asked = requested(trace_flag.as_deref(), std::env::var_os(TRACE_VARIABLE));
    let trace = Trace::new(crate::VERSION, COMMIT);
    let destination = Destination {
        explicit: asked.as_ref().map(|(path, _)| path.clone()),
        output: Some(output.clone()),
    };
    if let Err(error) = trace.set_destination(destination)
        && let Some((path, from)) = &asked
    {
        return Err(unusable(*from, path, &error));
    }
    let output_text = output.to_string_lossy();
    let sqlcmd = sqlcmd
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("sqlcmd"));
    let sqlcmd_text = sqlcmd.to_string_lossy();
    if let Some(password) = std::env::var_os("SQLCMDPASSWORD") {
        let password = password.to_string_lossy();
        if !password.is_empty() {
            trace.sensitive(LiteralKind::Secret, &password);
        }
    }
    trace.run(&RunShape {
        command: "bak_convert",
        to: format_tag(*to),
        backup_set: *backup_set,
        trusted_auth: user.is_none(),
        trust_server_certificate: *trust_server_certificate,
        lifecycle_timeout_secs: *sqlcmd_lifecycle_timeout,
        memory_limit_given: memory_limit.is_some(),
        raw: Some(*raw),
        trace_requested: asked.is_some(),
        source,
        output: &output_text,
        server,
        user: user.as_deref(),
        sqlcmd: (sqlcmd.as_os_str() != "sqlcmd").then_some(&*sqlcmd_text),
    });
    bak_trace::install_panic_hook();
    let scope = trace.enter();
    Ok(Some(Session {
        trace,
        _scope: scope,
    }))
}

pub(crate) fn end(session: Option<Session>, code: i32, failure: Option<&str>, err: &mut dyn Write) {
    let Some(session) = session else {
        return;
    };
    let outcome = if code == 0 {
        Outcome::Ok
    } else {
        Outcome::Failed
    };
    if let Some(text) = failure {
        let (class, stage) = yellowcode_core::plain::bak::failure_kind(text);
        if let Some((class, field)) = yellowcode_core::plain::bak::failure_refusal(text) {
            session.trace.refusal(&bak_trace::Refusal {
                class,
                field,
                part: None,
                expected: None,
                observed: None,
            });
        }
        session.trace.failure(class, stage, text);
    }
    let finished = session.trace.finish(outcome, code);
    let line = match (finished.path, finished.failure) {
        (Some(path), _) if outcome == Outcome::Ok => bak_trace::trace_written(&path),
        (Some(path), _) => bak_trace::failure_written(&path),
        (None, Some(failure)) => bak_trace::not_written(failure.os_error, failure.kind),
        (None, None) => return,
    };
    let _ = writeln!(err, "{line}");
}
