use super::{
    State,
    http::{Request, Response},
};
use memory_budget::{Category, Reservation};
use std::cell::RefCell;
use std::sync::Arc;
use yellowcode_core::viewer::memory::Session;

thread_local! {
    static WORK: RefCell<Vec<Reservation>> = const { RefCell::new(Vec::new()) };
    static CURRENT: RefCell<Option<Arc<Session>>> = const { RefCell::new(None) };
}

pub(super) struct Scope {
    previous: Option<Arc<Session>>,
    _scratch: Option<Reservation>,
    previous_work: Vec<Reservation>,
}
impl Scope {
    pub fn enter(state: &State, request: &Request) -> Result<Self, Response> {
        let memory = state.session.clone();
        let scratch = match &memory {
            Some(memory) => {
                if memory.is_cancelled() {
                    return Err(refusal());
                }
                Some(
                    memory
                        .budget()
                        .try_reserve(Category::Execution, working_bytes(state, request))
                        .map_err(|_| refusal())?,
                )
            }
            None => None,
        };
        let previous = CURRENT.with(|current| current.replace(memory));
        Ok(Self {
            previous,
            _scratch: scratch,
            previous_work: WORK.with(|v| v.take()),
        })
    }
    pub fn finish(self, mut response: Response) -> Response {
        if let Some(memory) = current() {
            response.body = match response
                .body
                .account(memory.budget(), memory.response_bytes())
            {
                Ok(body) => body,
                Err(_) => return refusal(),
            };
        }
        response
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        WORK.with(|v| {
            v.replace(std::mem::take(&mut self.previous_work));
        });
        CURRENT.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

pub fn working_bytes(state: &State, request: &Request) -> u64 {
    match request.path.as_str() {
        path if path.starts_with("/api/x/qb/") => {
            (256_u64 << 10).saturating_add((request.body.len() as u64).saturating_mul(128))
        }
        "/api/info" | "/api/stats" | "/api/tree" | "/favicon.ico" => 64 << 10,
        path if path.starts_with("/api/x/query/") => {
            (256_u64 << 10).saturating_add((request.body.len() as u64).saturating_mul(16))
        }
        path if path.starts_with("/api/x/data/") => {
            (256_u64 << 10).saturating_add((request.body.len() as u64).saturating_mul(16))
        }
        "/" => 4 << 20,
        path if path.starts_with("/front/")
            || path.starts_with("/skin/")
            || path.starts_with("/res/font/")
            || path.starts_with("/res/pic/") =>
        {
            4 << 20
        }
        _ => state.response_working_bytes(),
    }
}

pub(super) fn current() -> Option<Arc<Session>> {
    CURRENT.with(|current| current.borrow().clone())
}

pub(super) fn refusal() -> Response {
    Response { status: 503, content_type: "application/json; charset=utf-8", body: b"{\"error\":\"MEMORY_LIMIT: response; close unused results or reopen with a larger budget\"}".to_vec().into(), ..Default::default() }
}

pub(super) fn json<T: serde::Serialize + ?Sized>(status: u16, value: &T) -> Response {
    if let Some(memory) = current() {
        return match crate::transport::json_with_memory(
            status,
            value,
            memory.budget(),
            memory.response_bytes(),
        ) {
            Ok(response) => response,
            Err(crate::transport::JsonError::Memory(_)) => refusal(),
            Err(crate::transport::JsonError::Serialization(_)) => {
                crate::text(500, "response serialization failed\n")
            }
        };
    }
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: {
            let mut bytes = Vec::new();
            match crate::body::to_writer_wide(&mut bytes, value) {
                Ok(()) => bytes,
                Err(_) => b"null".to_vec(),
            }
        }
        .into(),
        ..Default::default()
    }
}

pub(super) fn is_control(req: &Request) -> bool {
    req.method == "POST"
        && req.body.len() <= 128
        && req.query.len() <= 2
        && matches!(req.path.as_str(), "/api/x/qb/close" | "/api/x/query/cancel")
}

pub(super) fn hold_workspace(bytes: u64) -> Result<(), Response> {
    if let Some(memory) = current() {
        let lease = memory
            .budget()
            .try_reserve(Category::Execution, bytes)
            .map_err(|_| refusal())?;
        WORK.with(|v| v.borrow_mut().push(lease));
    }
    Ok(())
}

pub(super) fn with_cancel<T>(f: impl FnOnce(&std::sync::atomic::AtomicBool) -> T) -> T {
    match current() {
        Some(memory) => f(memory.cancellation()),
        None => f(&std::sync::atomic::AtomicBool::new(false)),
    }
}
