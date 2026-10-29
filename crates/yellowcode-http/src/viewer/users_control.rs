use super::http::{Request, Response};
use serde::Serialize;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::json;
use std::fmt;
use yellowcode_core::{
    users::{self, UserAction, UserWriteError, UserWriteRequest},
    viewer::state::{State, Viewer},
};

pub(super) fn is_write_route(req: &Request) -> bool {
    matches!(
        req.path.as_str(),
        "/api/users/write-snapshot" | "/api/users/apply"
    )
}
fn reply(status: u16, value: &impl Serialize) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => Response {
            status,
            content_type: "application/json; charset=utf-8",
            body,
            ..Default::default()
        },
        Err(_) => error(500, "response_unavailable", "response unavailable"),
    }
}
fn error(status: u16, code: &str, message: &str) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: json!({"error":message, "code":code, "phase":"before_commit", "committed":false})
            .to_string()
            .into_bytes(),
        ..Default::default()
    }
}
fn forbidden() -> Response {
    error(
        403,
        "write_permission_required",
        "current local viewer permission required",
    )
}
pub(super) fn busy() -> Response {
    error(409, "source_busy", "source readers or loader are busy")
}

pub(super) fn source_closed() -> Response {
    error(409, "source_session_closed", "source session closed")
}

fn local(req: &Request) -> bool {
    let Some(host) = req.host.as_deref() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let desktop = host == "onec.localhost";
    let local_http = host.split_once(':').is_some_and(|(name, port)| {
        matches!(name, "localhost" | "127.0.0.1") && port.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if !desktop && !local_http {
        return false;
    }
    if req
        .sec_fetch_site
        .as_deref()
        .is_some_and(|s| !matches!(s, "same-origin" | "none"))
    {
        return false;
    }
    let origin_ok = req.origin.as_deref().is_some_and(|origin| {
        if desktop {
            matches!(
                origin,
                "onec://localhost" | "http://onec.localhost" | "https://onec.localhost"
            )
        } else {
            origin == format!("http://{host}")
        }
    });
    if req.origin.is_some() && !origin_ok {
        return false;
    }
    req.method != "POST"
        || origin_ok
        || req.sec_fetch_site.as_deref() == Some("same-origin")
        || (desktop && req.origin.is_none() && req.sec_fetch_site.is_none())
}
fn admission(req: &Request) -> Result<(), Response> {
    if !req.query.is_empty() {
        return Err(error(
            400,
            "invalid_input",
            "users endpoints take no query parameters",
        ));
    }
    let method_ok = match req.path.as_str() {
        "/api/users/write-snapshot" => matches!(req.method.as_str(), "GET" | "HEAD"),
        "/api/users/apply" => req.method == "POST",
        _ => false,
    };
    if !method_ok {
        return Err(error(405, "method_not_allowed", "method not allowed"));
    }
    if !local(req) {
        return Err(forbidden());
    }
    Ok(())
}

pub(super) fn without_viewer(_state: &State, req: &Request) -> Response {
    if let Err(r) = admission(req) {
        return r;
    }
    error(
        409,
        "viewer_required",
        "a current viewer session is required",
    )
}

#[derive(Clone, Copy)]
enum Phase {
    BeforeCommit,
    Applying,
    Committed,
}

fn panic_response(phase: Phase) -> Response {
    match phase {
        Phase::BeforeCommit => error(500, "internal_error", "user request failed before commit"),
        Phase::Applying => map_error(UserWriteError::CommitUnknown),
        Phase::Committed => map_error(UserWriteError::CommittedUnverified),
    }
}

pub(super) fn handle(viewer: &Viewer, req: &Request) -> Response {
    catch_request(viewer, |phase| handle_inner(viewer, req, phase))
}

fn catch_request(
    viewer: &Viewer,
    request: impl FnOnce(&std::cell::Cell<Phase>) -> Response,
) -> Response {
    let phase = std::cell::Cell::new(Phase::BeforeCommit);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| request(&phase))) {
        Ok(response) => response,
        Err(_) => {
            viewer.shutdown();
            panic_response(phase.get())
        }
    }
}

fn handle_inner(viewer: &Viewer, req: &Request, phase: &std::cell::Cell<Phase>) -> Response {
    if let Err(r) = admission(req) {
        return r;
    }
    let state = viewer.snapshot();
    let control = state.users_control.clone();
    if req.path == "/api/users/write-snapshot" {
        if !matches!(
            yellowcode_core::viewer::source::detect(&state.source),
            Ok(yellowcode_core::viewer::source::SourceKind::Infobase)
        ) {
            return map_error(UserWriteError::UnsupportedSource);
        }
        let Ok(_read) = control.admission.read() else {
            return busy();
        };
        let Some(permission) = control.permission(&state.source) else {
            return forbidden();
        };
        return match users::write_snapshot(&state.source) {
            Ok(snapshot) => {
                control.remember_revision(&snapshot.revision);
                #[derive(Serialize)]
                struct Snapshot<'a> {
                    #[serde(flatten)]
                    snapshot: users::UserWriteSnapshot,
                    write_permission: &'a str,
                    permission_lifetime_seconds: u64,
                }
                reply(
                    200,
                    &Snapshot {
                        snapshot,
                        write_permission: &permission,
                        permission_lifetime_seconds:
                            yellowcode_core::viewer::users_control::PERMISSION_LIFETIME.as_secs(),
                    },
                )
            }
            Err(e) => map_error(e),
        };
    }
    let Ok(_exclusive) = control.admission.try_write() else {
        return busy();
    };
    if !control.authorize(&state.source, req.user_write_permission.as_deref()) {
        return forbidden();
    }
    let request = match decode(&req.body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if !viewer.idle() {
        return busy();
    }
    if !yellowcode_core::viewer::consoles::slot(&state.source).release_idle()
        || !yellowcode_core::viewer::code_consoles::code_slot(&state.source).release_idle()
    {
        return busy();
    }
    if let Err(e) = control.consume_revision(&request.revision) {
        return map_error(e);
    }
    phase.set(Phase::Applying);
    match users::apply(&state.source, &request) {
        Ok(outcome) => {
            phase.set(Phase::Committed);
            if viewer.refresh_after_users_write().is_err() {
                return map_error(UserWriteError::CommittedUnverified);
            }
            control.remember_revision(&outcome.revision);
            match serde_json::to_vec(&outcome) {
                Ok(body) => Response {
                    status: 200,
                    content_type: "application/json; charset=utf-8",
                    body,
                    ..Default::default()
                },
                Err(_) => map_error(UserWriteError::CommittedUnverified),
            }
        }
        Err(e) => {
            if matches!(
                e,
                UserWriteError::CommitUnknown | UserWriteError::CommittedUnverified
            ) {
                control.revoke();
            } else if matches!(e, UserWriteError::StaleRevision) {
                let _ = viewer.refresh_after_users_write();
            }
            map_error(e)
        }
    }
}

fn map_error(e: UserWriteError) -> Response {
    use UserWriteError::*;
    let (status, code) = match &e {
        InvalidInput(_) => (400, "invalid_input"),
        UnsupportedSource => (409, "unsupported_source"),
        UnsupportedLayout(_) => (422, "unsupported_layout"),
        UnsupportedPolicy => (422, "unsupported_policy"),
        UnsupportedPlatform => (422, "unsupported_platform"),
        StaleRevision => (409, "stale_revision"),
        NotFound => (404, "not_found"),
        Ambiguous => (409, "ambiguous_identity"),
        ConfirmationRequired => (409, "confirmation_required"),
        LastUserConfirmationRequired => (409, "last_user_confirmation_required"),
        LastAdministrator => (409, "last_administrator"),
        DataAreaReadOnly => (409, "data_area_read_only"),
        StorageBeforeCommit => (503, "storage_before_commit"),
        CommitUnknown => (500, "commit_unknown"),
        CommittedUnverified => (500, "committed_unverified"),
    };
    let (phase, committed) = match e {
        CommitUnknown => ("commit_unknown", None),
        CommittedUnverified => ("committed_unverified", Some(true)),
        _ => ("before_commit", Some(false)),
    };
    reply(
        status,
        &json!({"error": e.to_string(), "code": code, "phase":phase, "committed":committed}),
    )
}
fn decode(body: &[u8]) -> Result<UserWriteRequest, Response> {
    let invalid = || error(400, "invalid_input", "invalid user write request");
    if body.len() > 64 * 1024 {
        return Err(error(413, "body_limit", "user write body exceeds limit"));
    }
    let mut parser = serde_json::Deserializer::from_slice(body);
    let mut remaining = 512usize;
    Shape {
        remaining: &mut remaining,
        depth: 0,
    }
    .deserialize(&mut parser)
    .map_err(|_| invalid())?;
    parser.end().map_err(|_| invalid())?;
    let request: UserWriteRequest = serde_json::from_slice(body).map_err(|_| invalid())?;
    if request.revision.is_empty() || request.revision.len() > 128 {
        return Err(map_error(UserWriteError::StaleRevision));
    }
    match &request.action {
        UserAction::Update { identity, .. }
        | UserAction::ClearPassword { identity }
        | UserAction::Delete { identity }
            if !users::valid_id(identity) =>
        {
            return Err(invalid());
        }
        _ => {}
    }
    Ok(request)
}

struct Shape<'a> {
    remaining: &'a mut usize,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Shape<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        *self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| de::Error::custom("item limit"))?;
        if self.depth > 8 {
            return Err(de::Error::custom("depth limit"));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Shape<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded user request")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, s: &str) -> Result<(), E> {
        if s.len() > 4096 {
            Err(E::custom("string limit"))
        } else {
            Ok(())
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        for _ in 0..128 {
            if seq
                .next_element_seed(Shape {
                    remaining: self.remaining,
                    depth: self.depth + 1,
                })?
                .is_none()
            {
                return Ok(());
            }
        }
        if seq
            .next_element_seed(Shape {
                remaining: self.remaining,
                depth: self.depth + 1,
            })?
            .is_some()
        {
            return Err(de::Error::custom("list limit"));
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        for _ in 0..16 {
            if map
                .next_key_seed(Shape {
                    remaining: self.remaining,
                    depth: self.depth + 1,
                })?
                .is_none()
            {
                return Ok(());
            }
            map.next_value_seed(Shape {
                remaining: self.remaining,
                depth: self.depth + 1,
            })?;
        }
        Err(de::Error::custom("field limit"))
    }
}
