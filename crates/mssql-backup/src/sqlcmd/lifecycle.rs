use super::{Request, RunError, diagnostic};
use process_containment::{Pipe, Process};
use std::{
    cell::Cell,
    io::{self, Read, Write},
    process::{Command, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const ALL_DONE: u8 = 7;
const POLL: Duration = Duration::from_millis(5);

thread_local! {
    static PARENT_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

struct ConsumerDeadline(Option<Instant>);
impl ConsumerDeadline {
    fn enter(deadline: Instant) -> Self {
        Self(PARENT_DEADLINE.with(|parent| parent.replace(Some(deadline))))
    }
}
impl Drop for ConsumerDeadline {
    fn drop(&mut self) {
        PARENT_DEADLINE.with(|parent| parent.set(self.0));
    }
}

struct Control {
    stop: AtomicBool,
    expired: AtomicBool,
    done: AtomicU8,
    work_deadline: Instant,
    deadline: Instant,
}

impl Control {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.work_deadline {
            self.expired.store(true, Ordering::Release);
            self.stop.store(true, Ordering::Release);
        }
        if self.stop.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SQL client exchange stopped",
            ));
        }
        Ok(())
    }
    fn abort(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

struct Done(Arc<Control>, u8);
impl Drop for Done {
    fn drop(&mut self) {
        self.0.done.fetch_or(self.1, Ordering::Release);
    }
}

pub(super) struct Reader {
    pipe: Pipe,
    control: Arc<Control>,
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.control.check()?;
            match self.pipe.read(bytes) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

fn send(mut pipe: Pipe, script: &[u8], control: &Control) -> io::Result<()> {
    let mut remaining = script;
    while !remaining.is_empty() {
        control.check()?;
        match pipe.write(remaining) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => remaining = remaining.get(n..).unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn supervise(mut process: Process, control: &Control) -> Result<ExitStatus, String> {
    let mut signalled = false;
    let mut failure = None;
    loop {
        let exited = match process.exited() {
            Ok(exited) => exited,
            Err(e) => {
                control.abort();
                return Err(format!(
                    "observing sqlcmd root exit: {e}; root reap unconfirmed"
                ));
            }
        };
        let _ = control.check();
        if !signalled
            && (control.stop.load(Ordering::Acquire)
                || (exited && control.done.load(Ordering::Acquire) == ALL_DONE))
        {
            if let Err(e) = process.terminate() {
                failure = Some(format!("terminating owned sqlcmd tree: {e}"));
                control.abort();
            }
            signalled = true;
        }
        if signalled && exited {
            let status = process.reap().map_err(|e| {
                control.abort();
                format!("reaping sqlcmd root: {e}")
            })?;
            return match failure {
                Some(e) => Err(e),
                None => Ok(status),
            };
        }
        if Instant::now() >= control.deadline {
            control.abort();
            return Err(format!(
                "{}root reap unconfirmed at lifecycle deadline",
                failure.map(|s| s + "; ").unwrap_or_default()
            ));
        }
        thread::sleep(POLL);
    }
}

fn failure(message: impl Into<String>) -> RunError {
    RunError {
        started: true,
        message: message.into(),
    }
}

pub(super) fn exchange<T>(
    request: &Request,
    command: &mut Command,
    script: &str,
    consume: impl FnOnce(Reader) -> Result<T, String>,
) -> Result<T, RunError> {
    let budget = request
        .lifecycle_timeout
        .unwrap_or(Duration::from_secs(1920));
    let start = Instant::now();
    let requested_deadline = start
        .checked_add(budget)
        .filter(|_| !budget.is_zero())
        .ok_or_else(|| RunError {
            started: false,
            message: "invalid sqlcmd lifecycle timeout".into(),
        })?;
    let deadline = PARENT_DEADLINE.with(|parent| {
        parent
            .get()
            .map_or(requested_deadline, |outer| outer.min(requested_deadline))
    });
    let remaining = deadline.saturating_duration_since(start);
    if remaining.is_zero() {
        return Err(RunError { started: false, message: "sqlcmd lifecycle timeout: enclosing SQL consumer deadline expired; no nested client started".into() });
    }
    let reserve = Duration::from_secs(120).min(remaining / 4);
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        expired: AtomicBool::new(false),
        done: AtomicU8::new(0),
        work_deadline: deadline - reserve,
        deadline,
    });
    let exchange = thread::scope(|scope| {
        let (ready, pipes) = std::sync::mpsc::channel();
        let monitor_control = Arc::clone(&control);
        let monitor = thread::Builder::new()
            .spawn_scoped(scope, move || {
                match process_containment::spawn(command, monitor_control.deadline) {
                    Ok(process_containment::Spawned {
                        process,
                        stdin,
                        stdout,
                        stderr,
                    }) => {
                        if ready.send(Ok((stdin, stdout, stderr))).is_err() {
                            monitor_control.abort();
                        }
                        supervise(process, &monitor_control)
                    }
                    Err(e) => {
                        let _ = ready.send(Err(RunError {
                            started: e.started,
                            message: format!("cannot start sqlcmd (containment): {}", e.error),
                        }));
                        Err("SQL client creation failed".into())
                    }
                }
            })
            .map_err(|e| RunError {
                started: false,
                message: format!("cannot start SQL supervisor: {e}; no SQL client created"),
            })?;
        let (stdin, stdout, stderr) = pipes.recv().map_err(|_| {
            failure("SQL supervisor failed during startup; root reap unconfirmed")
        })??;
        let diagnostic_control = Arc::clone(&control);
        let diagnostics = thread::Builder::new()
            .spawn_scoped(scope, move || {
                let _done = Done(Arc::clone(&diagnostic_control), 4);
                let mut pipe = stderr;
                let mut kept = Vec::new();
                let mut truncated = false;
                let mut buffer = [0_u8; 4096];
                loop {
                    let stopped = diagnostic_control.check().is_err();
                    if stopped && kept.len() == 8192 {
                        return (
                            kept,
                            true,
                            Some(io::Error::other("diagnostic drain stopped")),
                        );
                    }
                    match pipe.read(&mut buffer) {
                        Ok(0) => return (kept, truncated, None),
                        Ok(n) => {
                            let available = 8192_usize.saturating_sub(kept.len());
                            truncated |= n > available;
                            kept.extend_from_slice(
                                buffer.get(..n.min(available)).unwrap_or_default(),
                            );
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock && !stopped => {
                            thread::sleep(POLL)
                        }
                        Err(e) => {
                            diagnostic_control.abort();
                            return (kept, true, Some(e));
                        }
                    }
                }
            })
            .map_err(|e| {
                control.abort();
                failure(format!("cannot start SQL diagnostic reader: {e}"))
            })?;
        let writer_control = Arc::clone(&control);
        let writer = thread::Builder::new()
            .spawn_scoped(scope, move || {
                let _done = Done(Arc::clone(&writer_control), 1);
                let result = send(stdin, script.as_bytes(), &writer_control);
                if result.is_err() {
                    writer_control.abort();
                }
                result
            })
            .map_err(|e| {
                control.abort();
                failure(format!("cannot start SQL command writer: {e}"))
            })?;
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _deadline = ConsumerDeadline::enter(control.work_deadline);
            consume(Reader {
                pipe: stdout,
                control: Arc::clone(&control),
            })
        }));
        if !matches!(parsed, Ok(Ok(_))) {
            control.abort();
        }
        control.done.fetch_or(2, Ordering::Release);
        let sent = writer.join();
        let diagnostics = diagnostics.join();
        let status = monitor.join();
        Ok((parsed, sent, diagnostics, status))
    });
    let (parsed, sent, diagnostics, status) = exchange?;
    let parsed = match parsed {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    let mut errors = Vec::new();
    let value = match parsed {
        Ok(value) => Some(value),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    match sent {
        Ok(Ok(())) => {}
        Ok(Err(e)) => errors.push(format!("sending SQL command: {e}")),
        Err(_) => errors.push("SQL command writer failed".into()),
    }
    match diagnostics {
        Ok((bytes, truncated, read_error)) => {
            let detail = diagnostic(&bytes, truncated);
            if !detail.is_empty() {
                errors.push(format!("sqlcmd diagnostics: {detail}"));
            }
            if truncated {
                errors.push("sqlcmd diagnostics truncated or incomplete".into());
            }
            if let Some(e) = read_error {
                errors.push(format!("reading sqlcmd diagnostics: {e}"));
            }
        }
        Err(_) => errors.push("sqlcmd diagnostic reader failed".into()),
    }
    let status_ok = match status {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            errors.push(format!("sqlcmd exited with {status}"));
            false
        }
        Ok(Err(e)) => {
            errors.push(e);
            false
        }
        Err(_) => {
            errors.push("SQL supervisor failed; root reap unconfirmed".into());
            false
        }
    };
    if control.expired.load(Ordering::Acquire) {
        errors.push("sqlcmd lifecycle timeout".into());
    }
    let notices_only = errors.iter().all(|e| e.starts_with("sqlcmd diagnostics: "));
    if status_ok
        && !control.stop.load(Ordering::Acquire)
        && notices_only
        && let Some(value) = value
    {
        return Ok(value);
    }
    errors
        .push("SQL client exchange uncertain; server-side completion/cancellation unproven".into());
    Err(failure(diagnostic(errors.join("; ").as_bytes(), true)))
}
