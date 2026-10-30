mod check_cache;
mod forms;
mod native;
mod private;
pub use native::AnalysisPaths;
mod selected;
mod static_pictures;
use crate::transport::{Request, Response, write_response};
use mdconf::ManagedEntrySource;
use memory_budget::BudgetError;
use std::{
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use yellowcode_core::viewer::{body::ViewerBody, snapshot::ManagedSnapshot};

pub struct Endpoint<S: ManagedEntrySource> {
    snapshot: ManagedSnapshot<S>,
    source: memory_budget::BudgetedText,
    wire_limit: u64,
    native: Option<native::OwnedPaths>,
    checks: check_cache::CheckCache,
}
impl<S: ManagedEntrySource> Endpoint<S> {
    pub(super) fn new(
        snapshot: ManagedSnapshot<S>,
        source: &str,
        wire_limit: u64,
    ) -> Result<memory_budget::Shared<Self>, BudgetError> {
        Self::with_native(snapshot, source, wire_limit, None)
    }
    pub(super) fn with_native(
        snapshot: ManagedSnapshot<S>,
        source: &str,
        wire_limit: u64,
        paths: Option<AnalysisPaths<'_>>,
    ) -> Result<memory_budget::Shared<Self>, BudgetError> {
        let budget = snapshot.catalog().memory_budget().clone();
        let mut display = memory_budget::BudgetedText::try_with_capacity(
            &budget,
            memory_budget::Category::Metadata,
            u64::try_from(source.len()).map_err(|_| BudgetError::Overflow)?,
            source.len(),
        )?;
        display.try_push_str(source)?;
        let native = paths
            .map(|paths| native::OwnedPaths::new(&budget, paths))
            .transpose()?;
        let reservation = budget.try_reserve(memory_budget::Category::Metadata, 0)?;
        memory_budget::Shared::try_new(
            Self {
                snapshot,
                source: display,
                wire_limit,
                native,
                checks: std::sync::Mutex::new(check_cache::AnswerCache::new(
                    check_cache::CHECK_BYTES,
                )),
            },
            reservation,
        )
    }
}
impl<S: ManagedEntrySource + Send + Sync> super::sealed::Backend
    for memory_budget::Shared<Endpoint<S>>
{
}
impl<S: ManagedEntrySource + Send + Sync> super::HttpDispatch for memory_budget::Shared<Endpoint<S>>
where
    S::Error: std::fmt::Display,
{
    fn response(
        &self,
        request: &Request,
        control: &RequestControl<'_>,
    ) -> Result<Response<ViewerBody>, ManagedHttpError> {
        control.check()?;
        if let Some(response) = super::front::handle(request) {
            return Ok(eager(response));
        }
        handle_inner(
            &self.snapshot,
            self.source.as_str(),
            request,
            self.wire_limit,
            control,
            Resources {
                paths: self.native.as_ref().map(native::OwnedPaths::borrow),
                checks: Some(&self.checks),
            },
        )
    }
}

pub(super) fn eager(response: Response) -> Response<ViewerBody> {
    let Response {
        status,
        content_type,
        body,
        csp,
        cache_control,
    } = response;
    let body = match body {
        crate::transport::Body::Native(bytes) => ViewerBody::Eager(bytes),
        crate::transport::Body::Charged(bytes) => match bytes.try_into_shared() {
            Ok(shared) => ViewerBody::Shared(shared),
            Err(_) => return memory_refusal(),
        },
    };
    Response {
        status,
        content_type,
        body,
        csp,
        cache_control,
    }
}

pub(super) fn memory_refusal() -> Response<ViewerBody> {
    error(
        503,
        b"{\"error\":\"loading response failed: memory budget refused\"}",
    )
}

#[derive(Debug)]
pub enum ManagedHttpError {
    Memory(BudgetError),
    Canceled,
    Deadline,
}
impl From<BudgetError> for ManagedHttpError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}

pub struct RequestControl<'a> {
    canceled: &'a AtomicBool,
    shutdown: Option<&'a AtomicBool>,
    deadline: Option<Instant>,
    peer: Option<&'a std::net::TcpStream>,
}
impl<'a> RequestControl<'a> {
    pub fn new(canceled: &'a AtomicBool, deadline: Option<Instant>) -> Self {
        Self {
            canceled,
            shutdown: None,
            deadline,
            peer: None,
        }
    }
    pub(super) fn with_shutdown(
        canceled: &'a AtomicBool,
        shutdown: &'a AtomicBool,
        deadline: Option<Instant>,
        peer: &'a std::net::TcpStream,
    ) -> Self {
        Self {
            canceled,
            shutdown: Some(shutdown),
            deadline,
            peer: Some(peer),
        }
    }
    fn is_canceled(&self) -> bool {
        let stopped = self.canceled.load(Ordering::Acquire)
            || self
                .shutdown
                .is_some_and(|flag| flag.load(Ordering::Acquire));
        if stopped {
            return true;
        }
        let failed = self.peer.is_some_and(|peer| {
            if !matches!(peer.take_error(), Ok(None)) {
                return true;
            }
            let mut byte = [0];
            matches!(peer.peek(&mut byte), Err(error) if !matches!(
                error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ))
        });
        if failed {
            self.canceled.store(true, Ordering::Release);
            return true;
        }
        false
    }
    fn check(&self) -> Result<(), ManagedHttpError> {
        if self.is_canceled() {
            return Err(ManagedHttpError::Canceled);
        }
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            return Err(ManagedHttpError::Deadline);
        }
        Ok(())
    }
    fn check_io(&self) -> io::Result<()> {
        match self.check() {
            Ok(()) => Ok(()),
            Err(ManagedHttpError::Deadline) => Err(io::ErrorKind::TimedOut.into()),
            Err(_) => Err(io::ErrorKind::ConnectionAborted.into()),
        }
    }
}

fn response(status: u16, body: ViewerBody) -> Response<ViewerBody> {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body,
        ..Default::default()
    }
}
fn error(status: u16, body: &'static [u8]) -> Response<ViewerBody> {
    response(status, ViewerBody::Static(body))
}

pub fn handle<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    source: &str,
    request: &Request,
    wire_limit: u64,
    control: &RequestControl<'_>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: std::fmt::Display,
{
    handle_inner(
        snapshot,
        source,
        request,
        wire_limit,
        control,
        Resources::default(),
    )
}
pub fn handle_with_analysis<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    source: &str,
    request: &Request,
    wire_limit: u64,
    control: &RequestControl<'_>,
    paths: AnalysisPaths<'_>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: std::fmt::Display,
{
    handle_inner(
        snapshot,
        source,
        request,
        wire_limit,
        control,
        Resources {
            paths: Some(paths),
            checks: None,
        },
    )
}
#[derive(Default)]
struct Resources<'a> {
    paths: Option<AnalysisPaths<'a>>,
    checks: Option<&'a check_cache::CheckCache>,
}
fn handle_inner<S: ManagedEntrySource>(
    snapshot: &ManagedSnapshot<S>,
    source: &str,
    request: &Request,
    wire_limit: u64,
    control: &RequestControl<'_>,
    resources: Resources<'_>,
) -> Result<Response<ViewerBody>, ManagedHttpError>
where
    S::Error: std::fmt::Display,
{
    control.check()?;
    let result = if let Some(result) = private::handle(
        snapshot.catalog().memory_budget(),
        source,
        request,
        wire_limit,
    ) {
        result?
    } else if !matches!(request.method.as_str(), "GET" | "HEAD") {
        error(405, b"{\"error\":\"method not allowed\"}")
    } else {
        match request.path.as_str() {
            "/api/info" => response(200, snapshot.info_body(source, wire_limit)?),
            "/api/stats" => response(200, snapshot.stats_body(wire_limit)?),
            "/api/tree" => {
                let id = request.query.iter().find(|(key, _)| key == "id");
                match id.map_or(Ok(0), |(_, value)| value.parse::<usize>()) {
                    Err(_) => error(400, b"{\"error\":\"id: not a number\"}"),
                    Ok(id) => match snapshot.children_body(id, wire_limit)? {
                        Some(body) => response(200, body),
                        None => error(404, b"{\"error\":\"no such node\"}"),
                    },
                }
            }
            _ => match forms::handle(snapshot, request, wire_limit, control, resources.paths)
                .or_else(|| {
                    resources.paths.and_then(|paths| {
                        native::handle(
                            snapshot,
                            request,
                            wire_limit,
                            control,
                            paths,
                            resources.checks,
                        )
                    })
                }) {
                Some(result) => result?,
                None => selected::handle(snapshot, request, wire_limit, control)?,
            },
        }
    };
    control.check()?;
    Ok(result)
}

pub fn send(
    writer: &mut dyn Write,
    response: Response<ViewerBody>,
    head_only: bool,
    control: &RequestControl<'_>,
) -> io::Result<()> {
    let mut writer = ControlledWriter { writer, control };
    let result = write_response(&mut writer, &response, head_only);
    if result.is_err() {
        control.canceled.store(true, Ordering::Release);
    }
    result
}
struct ControlledWriter<'a, 'b> {
    writer: &'a mut dyn Write,
    control: &'a RequestControl<'b>,
}
impl Write for ControlledWriter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.control.check_io()?;
        let piece = bytes.get(..bytes.len().min(64 << 10)).unwrap_or_default();
        let count = self.writer.write(piece)?;
        self.control.check_io()?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.control.check_io()?;
        self.writer.flush()?;
        self.control.check_io()
    }
}
