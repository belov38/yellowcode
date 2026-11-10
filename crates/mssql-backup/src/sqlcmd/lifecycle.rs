use super::{
    Request, RunError,
    diag::Retained,
    diagnostic,
    tracing::{StdoutTap, Watch},
};
use process_containment::{Pipe, Process, interrupt};
use std::{
    cell::{Cell, RefCell},
    fmt,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Work,
    OrdinaryCleanup,
    InterruptCleanup,
}

thread_local! {
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Work) };
    static CLEANING: RefCell<Option<(String, u64)>> = const { RefCell::new(None) };
}

pub(super) struct InterruptCleanup(Phase);
impl InterruptCleanup {
    pub(super) fn enter() -> Self {
        Self(PHASE.with(|phase| phase.replace(Phase::InterruptCleanup)))
    }
}
impl Drop for InterruptCleanup {
    fn drop(&mut self) {
        PHASE.with(|phase| phase.set(self.0));
    }
}

pub(super) struct OrdinaryCleanup(Option<Option<(String, u64)>>);
impl OrdinaryCleanup {
    pub(super) fn enter(database: &str, seconds: u64) -> Self {
        if PHASE.with(Cell::get) != Phase::Work {
            return Self(None);
        }
        let before =
            CLEANING.with(|cleaning| cleaning.replace(Some((database.to_owned(), seconds))));
        PHASE.with(|phase| phase.set(Phase::OrdinaryCleanup));
        Self(Some(before))
    }
}
impl Drop for OrdinaryCleanup {
    fn drop(&mut self) {
        if let Some(before) = self.0.take() {
            PHASE.with(|phase| phase.set(Phase::Work));
            CLEANING.with(|cleaning| *cleaning.borrow_mut() = before);
        }
    }
}

pub(super) fn own_console_group() -> bool {
    interrupt::armed() && PHASE.with(Cell::get) != Phase::Work
}

fn interruptible() -> bool {
    interrupt::armed() && PHASE.with(Cell::get) == Phase::Work
}

fn ordinary_cleanup() -> Option<(String, u64)> {
    if interrupt::armed() && PHASE.with(Cell::get) == Phase::OrdinaryCleanup {
        CLEANING.with(|cleaning| cleaning.borrow().clone())
    } else {
        None
    }
}

pub(super) fn stop_waiting() -> bool {
    interrupt::armed()
        && PHASE.with(Cell::get) != Phase::InterruptCleanup
        && interrupt::count() >= 2
}

fn say_seen(database: &str, seconds: u64) {
    if !interrupt::claim_notice() {
        return;
    }
    let by = interrupt::requested().map_or("a request", |kind| kind.name());
    let _ = writeln!(
        io::stderr(),
        "yellowcode: interrupted by {by}; the cleanup of the temporary database {database} continues (each command waits at most {seconds} s); repeat the signal to stop waiting"
    );
}

const DEATH_GRACE: Duration = Duration::from_millis(500);

fn died_of_interrupt(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        matches!(status.signal(), Some(1 | 2 | 15))
    }
    #[cfg(windows)]
    {
        status.code() == Some(0xC000_013A_u32 as i32)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = status;
        false
    }
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
    interruptible: bool,
    interrupted: AtomicBool,
    cleaning: Option<(String, u64)>,
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
        if self.interruptible && interrupt::requested().is_some() {
            self.interrupted.store(true, Ordering::Release);
            self.stop.store(true, Ordering::Release);
        }
        if let Some((database, seconds)) = &self.cleaning {
            let seen = interrupt::count();
            if seen >= 1 {
                say_seen(database, *seconds);
            }
            if seen >= 2 {
                self.interrupted.store(true, Ordering::Release);
                self.stop.store(true, Ordering::Release);
            }
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

const POST_STOP_DRAIN: usize = 256 * 1024;

#[derive(Debug)]
enum Unread {
    Failed(io::Error),
    StillOpen,
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(e) => write!(f, "{e}"),
            Self::StillOpen => f.write_str(
                "sqlcmd diagnostics pipe was still open when the exchange stopped, so later output was not read",
            ),
        }
    }
}

fn drain_diagnostics(mut pipe: impl Read, control: &Control) -> (Retained, Option<Unread>) {
    let mut kept = Retained::default();
    let mut after_stop = 0_usize;
    let mut saw_stop = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let stopped = control.check().is_err();
        saw_stop |= stopped;
        if stopped && after_stop >= POST_STOP_DRAIN {
            kept.mark_cut();
            return (
                kept,
                Some(Unread::Failed(io::Error::other("diagnostic drain stopped"))),
            );
        }
        match pipe.read(&mut buffer) {
            Ok(0) => {
                if saw_stop {
                    kept.mark_end_uncertain();
                }
                return (kept, None);
            }
            Ok(n) => {
                if stopped {
                    after_stop = after_stop.saturating_add(n);
                }
                kept.push(buffer.get(..n).unwrap_or_default());
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && !stopped => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                kept.mark_cut();
                return (kept, Some(Unread::StillOpen));
            }
            Err(e) => {
                control.abort();
                kept.mark_cut();
                return (kept, Some(Unread::Failed(e)));
            }
        }
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
    tap: Arc<StdoutTap>,
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
                result => return self.tap.seen(result, bytes),
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
    let interruptible = interruptible();
    if interruptible && let Some(kind) = interrupt::requested() {
        return Err(RunError {
            started: false,
            message: format!("interrupted by {}; no SQL client was started", kind.name()),
        });
    }
    let own_group = own_console_group();
    let cleaning = ordinary_cleanup();
    if let Some((database, seconds)) = &cleaning {
        if interrupt::count() >= 1 {
            say_seen(database, *seconds);
        }
        if interrupt::count() >= 2
            && let Some(kind) = interrupt::requested()
        {
            return Err(RunError {
                started: false,
                message: format!("interrupted by {}; no SQL client was started", kind.name()),
            });
        }
    }
    let mut watch = Watch::begin(request, script);
    let tap = watch.tap();
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
        interruptible,
        interrupted: AtomicBool::new(false),
        cleaning,
        done: AtomicU8::new(0),
        work_deadline: deadline - reserve,
        deadline,
    });
    let exchange: Result<_, RunError> = thread::scope(|scope| {
        let (ready, pipes) = std::sync::mpsc::channel();
        let monitor_control = Arc::clone(&control);
        let monitor = thread::Builder::new()
            .spawn_scoped(scope, move || {
                let spawned = if own_group {
                    process_containment::spawn_apart(command, monitor_control.deadline)
                } else {
                    process_containment::spawn(command, monitor_control.deadline)
                };
                match spawned {
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
                            message: format!("cannot start sqlcmd: {}", e.error),
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
                drain_diagnostics(stderr, &diagnostic_control)
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
                tap: Arc::clone(&tap),
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
    let (parsed, sent, mut diagnostics, status) =
        exchange.inspect_err(|error| watch.set_started(error.started))?;
    watch.joined(
        &status,
        control.expired.load(Ordering::Acquire),
        &mut diagnostics,
    );
    let parsed = match parsed {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if control.interruptible
        && !control.interrupted.load(Ordering::Acquire)
        && let Ok(Ok(status)) = &status
        && died_of_interrupt(status)
        && interrupt::wait_for_request(DEATH_GRACE).is_some()
    {
        control.interrupted.store(true, Ordering::Release);
    }
    if control.interrupted.load(Ordering::Acquire)
        && let Some(kind) = interrupt::requested()
    {
        return Err(failure(format!(
            "interrupted by {}; SQL client exchange uncertain; server-side completion/cancellation unproven",
            kind.name()
        )));
    }
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
        Ok((kept, read_error)) => {
            let detail = kept.render(&super::password_bytes());
            if !detail.is_empty() {
                errors.push(format!("sqlcmd diagnostics: {detail}"));
            }
            if let Some(unread) = read_error {
                errors.push(match &unread {
                    Unread::Failed(e) => format!("reading sqlcmd diagnostics: {e}"),
                    Unread::StillOpen => unread.to_string(),
                });
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
        watch.succeeded();
        return Ok(value);
    }
    errors
        .push("SQL client exchange uncertain; server-side completion/cancellation unproven".into());
    Err(failure(diagnostic(errors.join("; ").as_bytes(), true)))
}

#[cfg(all(test, unix))]
mod death_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    #[test]
    fn a_client_that_died_of_sigint_sigterm_or_sighup_died_of_the_interrupt() {
        for signal in [SIGHUP, SIGINT, SIGTERM] {
            assert!(
                died_of_interrupt(&ExitStatus::from_raw(signal)),
                "signal {signal}"
            );
        }
    }

    #[test]
    fn a_client_that_died_of_another_signal_or_exited_did_not() {
        for signal in [3, 6, 9, 11, 13] {
            assert!(
                !died_of_interrupt(&ExitStatus::from_raw(signal)),
                "signal {signal}"
            );
        }
        for code in [0, 1, 2, 15, 129, 130, 143] {
            assert!(
                !died_of_interrupt(&ExitStatus::from_raw(code << 8)),
                "exit code {code}"
            );
        }
    }
}
