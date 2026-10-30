use query_console::{Console, Lang, Mode, ParamIn, RunRequest};
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    time::Duration,
};

pub const MAX_INPUT_BYTES: usize = 8 << 20;

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Format {
    Table,
    Json,
    Csv,
}

pub enum Source {
    Text(String),
    File(PathBuf),
    Stdin,
}

pub const DEFAULT_TIMEOUT: &str = "60";
pub const DEFAULT_MAX_ROWS: &str = "10000";

pub struct Request {
    pub memory: super::memory::Settings,
    pub base: PathBuf,
    pub source: Source,
    pub params: Vec<String>,
    pub params_file: Option<PathBuf>,
    pub format: Option<Format>,
    pub timeout: Duration,
    pub max_rows: usize,
}

impl Request {
    pub fn check(&self) -> Result<(), String> {
        if self.timeout.is_zero() {
            return Err("timeout is too small".into());
        }
        if self.max_rows == 0 || self.max_rows > query_console::MAX_ROWS {
            return Err(format!(
                "max-rows must be between 1 and {}",
                query_console::MAX_ROWS
            ));
        }
        Ok(())
    }
}

pub fn timeout(text: &str) -> Result<Duration, String> {
    let seconds: f64 = text
        .parse()
        .map_err(|_| "timeout must be positive finite seconds")?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("timeout must be positive finite seconds".into());
    }
    let value = Duration::try_from_secs_f64(seconds).map_err(|_| "timeout is too large")?;
    if value.is_zero() {
        return Err("timeout is too small".into());
    }
    Ok(value)
}
pub fn row_limit(text: &str) -> Result<usize, String> {
    let n: usize = text
        .parse()
        .map_err(|_| "max-rows must be a positive integer")?;
    if n == 0 || n > query_console::MAX_ROWS {
        return Err(format!(
            "max-rows must be between 1 and {}",
            query_console::MAX_ROWS
        ));
    }
    Ok(n)
}
fn parameters(
    args: &Request,
    budget: &memory_budget::MemoryBudget,
) -> crate::Result<memory_budget::Charged<Vec<ParamIn>>> {
    let mut params = match &args.params_file {
        Some(path) => {
            let source = super::input::read_text(&mut File::open(path)?, budget)?;
            super::input::parameters_json(source.as_bytes(), budget)?
        }
        None => super::input::parameters_json(b"[]", budget)?,
    };
    for flag in &args.params {
        let (name, repr) = flag.split_once('=').ok_or("--param expects NAME=REPR")?;
        params = super::input::append_parameter(params, name, repr, budget)?;
    }
    validate_parameters(&params)?;
    Ok(params)
}

pub fn validate_parameters(params: &[ParamIn]) -> crate::Result<()> {
    let mut bytes = 0usize;
    for (index, param) in params.iter().enumerate() {
        bytes = bytes
            .saturating_add(param.name.len())
            .saturating_add(param.repr.len());
        if bytes > MAX_INPUT_BYTES {
            return Err("combined parameters exceed 8 MiB".into());
        }
        if param.name.trim().is_empty() || param.name.trim() != param.name {
            return Err("parameter name must be nonempty with no surrounding whitespace".into());
        }
        if params.iter().take(index).any(|prior| {
            super::lowercase::lowercase(&prior.name).eq(super::lowercase::lowercase(&param.name))
        }) {
            return Err("duplicate parameter".into());
        }
    }
    Ok(())
}

pub fn run(
    args: Request,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> crate::Result<i32> {
    args.check()?;
    let (memory, reserve) = args.memory.resolve()?;
    recorded(
        super::control::CliMemory::new(memory, reserve),
        out,
        err,
        |out, err, memory, diagnostic| {
            run_with_memory(args, input, out, err, tty, memory, diagnostic)
        },
    )
}

pub fn recorded(
    mut memory: super::control::CliMemory,
    out: &mut dyn Write,
    err: &mut dyn Write,
    body: impl FnOnce(
        &mut dyn Write,
        &mut dyn Write,
        &mut super::control::CliMemory,
        &mut super::memory_diagnostic::Record,
    ) -> crate::Result<i32>,
) -> crate::Result<i32> {
    if memory.summary_wanted() {
        memory.summary(err)?;
    }
    let started = std::time::Instant::now();
    let mut diagnostic = super::memory_diagnostic::Record {
        id: super::memory_diagnostic::next_id(),
        status: 1,
        reason: "request_error",
        transport: "not_sent",
        wire_bytes: 0,
        elapsed_ms: 0,
        memory: memory.snapshot(),
        reader: onecd::ReaderStats::default(),
    };
    let mut counted = CountedOutput {
        output: out,
        bytes: 0,
    };
    let result = body(&mut counted, err, &mut memory, &mut diagnostic);
    diagnostic.wire_bytes = counted.bytes;
    if let Err(error) = &result
        && let Some(reason) =
            super::memory_diagnostic::resource_reason(error.as_ref()).or(matches!(
                error.downcast_ref::<super::input::InputError>(),
                Some(super::input::InputError::Limit)
            )
            .then_some("memory_input"))
    {
        diagnostic.reason = reason;
    }
    diagnostic.elapsed_ms = started.elapsed().as_millis();
    diagnostic.memory = memory.snapshot();
    diagnostic.status = match &result {
        Ok(code) => u16::try_from(*code).unwrap_or(1),
        Err(_) => 1,
    };
    let _ = diagnostic.write(err);
    result
}
struct CountedOutput<'a> {
    output: &'a mut dyn Write,
    bytes: u64,
}
impl Write for CountedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
fn run_with_memory(
    args: Request,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
    memory: &mut super::control::CliMemory,
    diagnostic: &mut super::memory_diagnostic::Record,
) -> crate::Result<i32> {
    let params = parameters(&args, memory.ledger())?;
    let text = match &args.source {
        Source::Text(text) => {
            if text.len() > MAX_INPUT_BYTES {
                return Err("query input exceeds 8 MiB".into());
            }
            super::input::copy_text(
                text.strip_prefix('\u{feff}').unwrap_or(text),
                memory.ledger(),
            )?
        }
        Source::File(path) => super::input::read_text(&mut File::open(path)?, memory.ledger())?,
        Source::Stdin => super::input::read_text(input, memory.ledger())?,
    };
    memory.refresh()?;
    if let Some(refusal) = memory.admission() {
        diagnostic.reason = refusal.reason();
        return Err(refusal.text.into());
    }
    let console = memory.open(&args.base)?;
    super::control::trim_heap();
    for note in &console.info().notes {
        let _ = writeln!(err, "yellowcode: schema note: {note}");
    }
    let req = text.try_zip(params)?.map(|(text, params)| RunRequest {
        text,
        params,
        mode: Mode::Batch,
        lang: Lang::En,
    });
    let (report, timed_out) = match execute_on_worker(&console, &req, args.timeout)? {
        Ok(result) => result,
        Err(refusal) => {
            let refusal = super::memory::Refusal::run(&refusal);
            diagnostic.reason = refusal.reason();
            diagnostic.reader = console.reader_stats();
            let _ = writeln!(err, "yellowcode: {}", refusal.text);
            return Ok(1);
        }
    };
    if let Some(error) = report.error() {
        if timed_out {
            let _ = writeln!(
                err,
                "yellowcode: query execution timed out (cooperative cancellation)"
            );
        }
        for cause in error.chain() {
            let _ = writeln!(err, "yellowcode: {cause}");
        }
    }
    let code = i32::from(report.error().is_some());
    diagnostic.reason = super::memory::report_reason(&report, timed_out);
    let format = args
        .format
        .unwrap_or(if tty { Format::Table } else { Format::Json });
    let result = super::query_output::write_report_limited(
        &report,
        args.max_rows,
        timed_out,
        format,
        memory.response_limit(),
        out,
        err,
    );
    diagnostic.reader = console.reader_stats();
    finish_output(result, code, diagnostic)
}

pub fn finish_output(
    result: crate::Result<()>,
    code: i32,
    diagnostic: &mut super::memory_diagnostic::Record,
) -> crate::Result<i32> {
    diagnostic.transport = match &result {
        Ok(()) => "sent",
        Err(error)
            if error.is::<memory_budget::OutputClosed>()
                || error.is::<memory_budget::OutputDisconnected>() =>
        {
            "disconnected"
        }
        Err(error) if error.is::<memory_budget::OutputTimedOut>() => "timeout",
        Err(error) => match error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
        {
            Some(
                std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted,
            ) => "disconnected",
            Some(std::io::ErrorKind::TimedOut) => "timeout",
            _ => "not_sent",
        },
    };
    if result.is_err() && diagnostic.transport == "not_sent" {
        diagnostic.reason = "output_error";
    }
    match result {
        Err(error)
            if error.is::<memory_budget::OutputClosed>()
                || error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(code)
        }
        Err(error) => Err(error),
        Ok(()) => Ok(code),
    }
}

fn execute_on_worker(
    console: &Console,
    req: &RunRequest,
    timeout: Duration,
) -> crate::Result<Result<(query_console::Report, bool), query_console::RunRefusal>> {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("yellowcode-query".into())
            .stack_size(memory_budget::OWNER_WORKER_STACK_BYTES)
            .spawn_scoped(scope, || execute(console, req, timeout))?;
        worker
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

pub fn execute(
    console: &Console,
    req: &RunRequest,
    timeout: Duration,
) -> crate::Result<Result<(query_console::Report, bool), query_console::RunRefusal>> {
    let cancel = AtomicBool::new(false);
    let timed_out = AtomicBool::new(false);
    let report = std::thread::scope(|scope| -> crate::Result<_> {
        let (finished, wait) = mpsc::channel::<()>();
        let (cancel, timed_out) = (&cancel, &timed_out);
        let dog = std::thread::Builder::new()
            .name("yellowcode-query-timeout".into())
            .spawn_scoped(scope, move || {
                if wait.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout) {
                    timed_out.store(true, Ordering::Release);
                    cancel.store(true, Ordering::Release);
                }
            })?;
        let report = console.run_budgeted(req, cancel);
        drop(finished);
        dog.join().map_err(|_| "query timeout watchdog failed")?;
        Ok(report)
    })?;
    Ok(report.map(|report| {
        let timed_out = report.cancelled && timed_out.load(Ordering::Acquire);
        (report, timed_out)
    }))
}
