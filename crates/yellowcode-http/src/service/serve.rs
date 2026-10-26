use super::memory::{ErrorBody, Refusal};
use super::{query, query_output};
use crate::{DeadlineReader, close_gracefully, conn::DeadlineWriter, transport as http};
use query_console::Console;
use std::{
    io::Write,
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const REFUSAL_TIMEOUT: Duration = Duration::from_millis(20);

fn close_refused(stream: &TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let deadline = Instant::now() + REFUSAL_TIMEOUT;
    let mut sink = [0u8; 4096];
    let mut reader = stream;
    let mut left = 64 * 1024usize;
    while left > 0 {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() || stream.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        match std::io::Read::read(&mut reader, &mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

pub struct Config {
    pub platform_build: String,
    pub memory: super::memory::Settings,
    pub base: PathBuf,
    pub listen: SocketAddr,
}

pub fn run(args: Config, err: &mut dyn Write) -> yellowcode_core::Result<()> {
    let (process, listener) = yellowcode_server::QueryProcess::start(
        yellowcode_server::QueryConfig {
            platform_build: args.platform_build,
            memory: args.memory,
            base: args.base,
        },
        err,
        || TcpListener::bind(args.listen),
    )?;
    let console = process.console();
    let memory = process.memory();
    writeln!(
        err,
        "yellowcode: warning: the base is opened once read-only; pages are read on demand and cached. External changes are not tracked. Serve an immutable copy and restart after updating it; concurrent 1C writes are unsupported."
    )?;
    writeln!(
        err,
        "yellowcode: listening on http://{}",
        listener.local_addr()?
    )?;
    err.flush()?;
    for note in &console.info().notes {
        let _ = writeln!(err, "yellowcode: schema note: {note}");
    }
    let mut worker: Option<JoinHandle<()>> = None;
    loop {
        let (stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        let refused = memory.admission().or_else(|| {
            worker
                .as_ref()
                .is_some_and(|h| !h.is_finished())
                .then_some(Refusal::BUSY)
        });
        if let Some(refusal) = refused {
            let mut diagnostic = RequestDiagnostic::new(console, memory);
            diagnostic.status = refusal.status;
            diagnostic.reason = refusal.reason();
            let result = send_response_counted(
                &stream,
                &refusal_response(refusal),
                false,
                REFUSAL_TIMEOUT,
                "GET, POST",
                &mut diagnostic.wire_bytes,
            );
            diagnostic.transport = transport_status(&result);
            close_refused(&stream);
            continue;
        }
        if let Some(done) = worker.take() {
            done.join().map_err(|_| "HTTP query worker panicked")?;
        }
        let console = Arc::clone(console);
        let memory = Arc::clone(memory);
        worker = Some(
            std::thread::Builder::new()
                .name("yellowcode-http-query".into())
                .stack_size(memory_budget::OWNER_WORKER_STACK_BYTES)
                .spawn(move || connection(&console, &stream, &memory))?,
        );
    }
}

fn connection(console: &Console, stream: &TcpStream, memory: &super::memory::Context) {
    let mut diagnostic = RequestDiagnostic::new(console, memory);
    if let Some(refusal) = memory.admission() {
        diagnostic.status = refusal.status;
        diagnostic.reason = refusal.reason();
        let result = send_response_counted(
            stream,
            &refusal_response(refusal),
            false,
            IO_TIMEOUT,
            "GET, POST",
            &mut diagnostic.wire_bytes,
        );
        diagnostic.transport = transport_status(&result);
        close_gracefully(stream);
        return;
    }
    let mut reader = DeadlineReader {
        stream,
        deadline: Instant::now() + IO_TIMEOUT,
    };
    let request = super::input::read_http_request(&mut reader, memory.ledger());
    let (response, head_only, allow) = match request {
        Ok(req) => {
            let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle(console, &req, memory)
            }))
            .unwrap_or_else(|_| error(500, "internal query error"));
            let allow = if req.path.starts_with("/table/") {
                "GET"
            } else {
                "POST"
            };
            (response, req.method == "HEAD", allow)
        }
        Err(super::input::HttpInputError::Status(status)) => {
            (error(status, "invalid HTTP request"), false, "GET, POST")
        }
        Err(super::input::HttpInputError::Memory(reason)) => {
            (resource_error(reason), false, "GET, POST")
        }
        Err(super::input::HttpInputError::Io(reason)) => {
            diagnostic.reason = "read_error";
            diagnostic.transport = transport_status(&Err(reason));
            return;
        }
    };
    diagnostic.status = response.status;
    diagnostic.reason = response.body.reason();
    let result = send_response_counted(
        stream,
        &response,
        head_only,
        IO_TIMEOUT,
        allow,
        &mut diagnostic.wire_bytes,
    );
    diagnostic.transport = transport_status(&result);
    drop(response);
    close_gracefully(stream);
}

fn handle(
    console: &Console,
    req: &http::Request,
    memory: &super::memory::Context,
) -> ManagedResponse {
    if let Some(kind) = req.path.strip_prefix("/table/") {
        let kind = kind.strip_suffix('/').unwrap_or(kind);
        if kind.is_empty() || kind.contains('/') {
            return error(404, "no such endpoint");
        }
        if req.method != "GET" {
            return error(405, "GET required");
        }
        return table_page(console, kind, req, memory);
    }
    if req.path != "/query" {
        return error(404, "no such endpoint");
    }
    if req.method != "POST" {
        return error(405, "POST required");
    }
    let parsed = match super::input::parse_query(&req.body, memory.ledger()) {
        Ok(parsed) => parsed,
        Err(super::input::InputError::Malformed) => return error(400, "invalid query JSON"),
        Err(super::input::InputError::Limit) => return refusal_response(Refusal::INPUT),
        Err(super::input::InputError::Memory(reason)) => return resource_error(reason),
    };
    if let Err(e) = query::validate_parameters(&parsed.request.params) {
        return error(400, &e.to_string());
    }
    if !(1..=query_console::MAX_ROWS).contains(&parsed.max_rows) {
        return error(400, "invalid maxRows");
    }
    let timeout = match Duration::try_from_secs_f64(parsed.timeout) {
        Ok(value) if !value.is_zero() => value,
        _ => return error(400, "timeout must be positive finite seconds"),
    };
    let request = parsed.request;
    let (report, timed_out) = match query::execute(console, &request, timeout) {
        Ok(Ok(result)) => result,
        Ok(Err(refusal)) => return refusal_response(Refusal::run(&refusal)),
        Err(e) => return error(500, &e.to_string()),
    };
    let mut writer = match memory.response_writer() {
        Ok(writer) => writer,
        Err(refusal) => return refusal_response(refusal),
    };
    let encoded = query_output::write_report(
        &report,
        parsed.max_rows,
        timed_out,
        query::Format::Json,
        &mut writer.serialization_sink(),
        &mut std::io::sink(),
    );
    if let Some(reason) = writer.error().copied() {
        drop(writer);
        drop(report);
        return resource_error(reason);
    }
    match encoded {
        Ok(()) => http::Response {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: Body::Managed {
                bytes: writer.into_buffer(),
                reason: super::memory::report_reason(&report, timed_out),
            },
            ..Default::default()
        },
        Err(e) => error(422, &e.to_string()),
    }
}

fn table_page(
    console: &Console,
    kind: &str,
    req: &http::Request,
    memory: &super::memory::Context,
) -> ManagedResponse {
    let mut name = None;
    let mut limit = None;
    let mut cursor = None;
    let mut index = None;
    let mut eq = None;
    let mut range = None;
    let mut order = None;
    let mut posted = None;
    let mut deletion_mark = None;
    for (key, value) in &req.query {
        let target = match key.as_str() {
            "name" => &mut name,
            "limit" => &mut limit,
            "cursor" => &mut cursor,
            "index" => &mut index,
            "eq" => &mut eq,
            "range" => &mut range,
            "order" => &mut order,
            "posted" => &mut posted,
            "deletionMark" => &mut deletion_mark,
            _ => return error(400, "unknown query parameter"),
        };
        if target.replace(value.as_str()).is_some() {
            return error(400, "duplicate query parameter");
        }
    }
    let Some(name) = name else {
        return error(400, "name is required");
    };
    let limit = match limit {
        None => 1000,
        Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            match s.parse::<usize>() {
                Ok(limit) => limit,
                Err(_) => return error(400, "invalid limit"),
            }
        }
        Some(_) => return error(400, "invalid limit"),
    };
    let order_value = match order {
        None | Some("asc") => query_console::IndexOrder::Asc,
        Some("desc") => query_console::IndexOrder::Desc,
        Some(_) => return error(400, "invalid order"),
    };
    let mut filter = super::table::RowFilter::default();
    for (value, target, invalid) in [
        (posted, &mut filter.posted, "invalid posted"),
        (
            deletion_mark,
            &mut filter.deletion_mark,
            "invalid deletionMark",
        ),
    ] {
        if let Some(value) = value {
            match super::table::filter_value(value) {
                Ok(value) => *target = Some(value),
                Err(_) => return error(400, invalid),
            }
        }
    }
    let result = if let Some(index) = index {
        let request =
            match super::input::index_request(index, eq, range, order_value, memory.ledger()) {
                Ok(request) => request,
                Err(super::input::InputError::Malformed) => {
                    return error(400, "invalid eq or range JSON");
                }
                Err(super::input::InputError::Limit) => return refusal_response(Refusal::INPUT),
                Err(super::input::InputError::Memory(reason)) => return resource_error(reason),
            };
        console.read_index_page_filtered(kind, name, limit, cursor, &request, &filter)
    } else {
        if eq.is_some() || range.is_some() || order.is_some() {
            return error(400, "eq, range and order require index");
        }
        console.read_table_page_filtered(kind, name, limit, cursor, &filter)
    };
    match result {
        Ok(page) => serialize_response(200, &page, memory),
        Err(e) => {
            use query_console::TableReadError;
            if let TableReadError::IndexRejected {
                error,
                available_indexes,
            } = &e
            {
                return serialize_response(
                    422,
                    &super::table::Rejected {
                        error,
                        available: available_indexes,
                    },
                    memory,
                );
            }
            let status = match e {
                TableReadError::Invalid(_) => 400,
                TableReadError::NotFound(_) => 404,
                TableReadError::Unsupported(_) | TableReadError::IndexRejected { .. } => 422,
                TableReadError::Storage(_) => 500,
                TableReadError::Memory(m) => return refusal_response(Refusal::limit(m)),
            };
            error(status, &e.to_string())
        }
    }
}

type ManagedResponse = http::Response<Body>;
#[allow(clippy::large_enum_variant)]
enum Body {
    Small {
        body: ErrorBody,
        reason: &'static str,
    },
    Managed {
        bytes: memory_budget::BudgetedVec<u8>,
        reason: &'static str,
    },
}
impl Default for Body {
    fn default() -> Self {
        Self::Small {
            body: ErrorBody::empty(),
            reason: "ok",
        }
    }
}
impl Body {
    fn reason(&self) -> &'static str {
        match self {
            Self::Small { reason, .. } => reason,
            Self::Managed { reason, .. } => reason,
        }
    }
}
impl AsRef<[u8]> for Body {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Small { body, .. } => body.as_bytes(),
            Self::Managed { bytes, .. } => bytes.as_slice(),
        }
    }
}
fn error(status: u16, message: &str) -> ManagedResponse {
    http::Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: Body::Small {
            body: ErrorBody::new(message),
            reason: super::memory::reason(message),
        },
        ..Default::default()
    }
}
fn refusal_response(refusal: Refusal) -> ManagedResponse {
    error(refusal.status, refusal.text)
}
fn resource_error(reason: memory_budget::BudgetError) -> ManagedResponse {
    refusal_response(Refusal::budget(reason))
}
fn serialize_response(
    status: u16,
    value: &impl serde::Serialize,
    memory: &super::memory::Context,
) -> ManagedResponse {
    let mut writer = match memory.response_writer() {
        Ok(writer) => writer,
        Err(refusal) => return refusal_response(refusal),
    };
    let encoded = serde_json::to_writer(writer.serialization_sink(), value);
    if let Some(reason) = writer.error().copied() {
        drop(writer);
        return resource_error(reason);
    }
    match encoded {
        Ok(()) => http::Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: Body::Managed {
                bytes: writer.into_buffer(),
                reason: if status == 200 { "ok" } else { "http_error" },
            },
            ..Default::default()
        },
        Err(_) => error(500, "response serialization failed"),
    }
}

fn send_response_counted<B: AsRef<[u8]>>(
    stream: &TcpStream,
    response: &http::Response<B>,
    head_only: bool,
    timeout: Duration,
    allow: &str,
    wire_bytes: &mut u64,
) -> std::io::Result<()> {
    stream.set_nonblocking(true)?;
    let mut writer = DeadlineWriter {
        stream,
        deadline: Instant::now() + timeout,
        wire_bytes: 0,
    };
    let result = http::write_response_with_allow(
        &mut writer,
        response,
        head_only,
        (response.status == 405).then_some(allow),
    );
    *wire_bytes = writer.wire_bytes;
    let reset = stream.set_nonblocking(false);
    result.and(reset)
}

struct RequestDiagnostic<'a> {
    console: &'a Console,
    memory: &'a super::memory::Context,
    started: Instant,
    id: u64,
    status: u16,
    reason: &'static str,
    transport: &'static str,
    wire_bytes: u64,
}
impl<'a> RequestDiagnostic<'a> {
    fn new(console: &'a Console, memory: &'a super::memory::Context) -> Self {
        Self {
            console,
            memory,
            started: Instant::now(),
            id: super::memory_diagnostic::next_id(),
            status: 0,
            reason: "read_error",
            transport: "not_sent",
            wire_bytes: 0,
        }
    }
}
impl Drop for RequestDiagnostic<'_> {
    fn drop(&mut self) {
        let record = super::memory_diagnostic::Record {
            id: self.id,
            status: self.status,
            reason: self.reason,
            transport: self.transport,
            wire_bytes: self.wire_bytes,
            elapsed_ms: self.started.elapsed().as_millis(),
            memory: self.memory.snapshot(),
            reader: self.console.reader_stats(),
        };
        let _ = record.write(&mut std::io::stderr().lock());
    }
}
fn transport_status(result: &std::io::Result<()>) -> &'static str {
    match result {
        Ok(()) => "sent",
        Err(error) => match error.kind() {
            std::io::ErrorKind::TimedOut => "timeout",
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset => "disconnected",
            _ => "io_error",
        },
    }
}
