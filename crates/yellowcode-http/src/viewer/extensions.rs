use super::{
    http::{Request, Response},
    state::Viewer,
};
use serde::Deserialize;
use yellowcode_core::viewer::extensions::{self, Code, CopyKind};

pub(super) fn is_post_route(path: &str) -> bool {
    matches!(
        path,
        "/api/extensions/start" | "/api/extensions/cancel" | "/api/extensions/content/start"
    )
}
pub(super) fn is_route(req: &Request) -> bool {
    req.path == "/api/extensions" || is_post_route(&req.path)
}
fn local(req: &Request) -> bool {
    let Some(host) = req.host.as_deref() else {
        return false;
    };
    let desktop = host.eq_ignore_ascii_case("onec.localhost");
    let http = host.split_once(':').is_some_and(|(name, port)| {
        (name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1")
            && port.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if !desktop && !http {
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
            origin
                .strip_prefix("http://")
                .is_some_and(|o| o.eq_ignore_ascii_case(host))
        }
    });
    if req.origin.is_some() && !origin_ok {
        return false;
    }
    req.method != "POST"
        || origin_ok
        || req.sec_fetch_site.as_deref() == Some("same-origin")
        || desktop && req.origin.is_none() && req.sec_fetch_site.is_none()
}
fn failure(status: u16, code: Code) -> Response {
    let bytes:&[u8]=match code {
        Code::Unsupported=>br#"{"code":"unsupported","error":"Viewing extensions is unavailable for this source"}"#,
        Code::MemoryLimit=>br#"{"code":"memory_limit","error":"There are not enough resources to read extensions"}"#,
        Code::SourceChanged=>br#"{"code":"source_changed","error":"The source changed; reopen it"}"#,
        Code::SourceClosed=>br#"{"code":"source_closed","error":"The source is closed"}"#,
        Code::Stale=>br#"{"code":"stale","error":"This extension request has expired"}"#,
        Code::Busy=>br#"{"code":"busy","error":"The previous extension request is finishing"}"#,
        Code::Cancelled=>br#"{"code":"cancelled","error":"Reading extensions was cancelled"}"#,
        Code::Deadline=>br#"{"code":"deadline","error":"Reading extensions took too long"}"#,
        Code::WorkLimit=>br#"{"code":"work_limit","error":"This source exceeds the supported extension viewing limits"}"#,
        Code::Invalid=>br#"{"code":"invalid","error":"Invalid extension request"}"#,
        Code::NoContent=>br#"{"code":"no_content","error":"There is no stored content for this extension"}"#,
        Code::ContentLayout=>br#"{"code":"content_layout","error":"This stored extension layout is unsupported"}"#,
        Code::CorruptContent=>br#"{"code":"corrupt_content","error":"The stored extension content could not be verified"}"#,
        Code::Unavailable=>br#"{"code":"unavailable","error":"Extensions could not be read"}"#,
    };
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: bytes.to_vec().into(),
        ..Default::default()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel<'a> {
    #[serde(borrow)]
    scan: &'a str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Content<'a> {
    #[serde(borrow)]
    scan: &'a str,
    #[serde(borrow)]
    id: &'a str,
    copy: Option<CopyKind>,
}
fn token(raw: &str) -> bool {
    let b = raw.as_bytes();
    b.len() == 34
        && b.iter().enumerate().all(|(i, byte)| match i {
            0 => *byte == b'e',
            17 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}
pub(super) fn handle(viewer: &Viewer, req: &Request) -> Response {
    if !local(req) {
        return failure(403, Code::Invalid);
    }
    let method = if req.path == "/api/extensions" {
        "GET"
    } else {
        "POST"
    };
    if req.method != method {
        return failure(405, Code::Invalid);
    }
    if req.body.len() > 128 {
        return failure(413, Code::Invalid);
    }
    let state = viewer.snapshot();
    if state.users_control.closed() {
        return failure(409, Code::SourceClosed);
    }
    let result = match req.path.as_str() {
        "/api/extensions/start" => {
            if !req.query.is_empty()
                || req.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
                || serde_json::from_slice::<Empty>(&req.body).is_err()
            {
                return failure(400, Code::Invalid);
            }
            extensions::start(&state)
        }
        "/api/extensions/content/start" => {
            if !req.query.is_empty()
                || req.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
            {
                return failure(400, Code::Invalid);
            }
            let Ok(body) = serde_json::from_slice::<Content<'_>>(&req.body) else {
                return failure(400, Code::Invalid);
            };
            if !token(body.scan) {
                return failure(400, Code::Invalid);
            }
            extensions::start_content(&state, body.scan, body.id, body.copy)
        }
        "/api/extensions/cancel" => {
            if !req.query.is_empty()
                || req.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
            {
                return failure(400, Code::Invalid);
            }
            let Ok(body) = serde_json::from_slice::<Cancel<'_>>(&req.body) else {
                return failure(400, Code::Invalid);
            };
            if !token(body.scan) {
                return failure(400, Code::Invalid);
            }
            extensions::cancel(&state, body.scan)
        }
        "/api/extensions" => {
            let [(key, value)] = req.query.as_slice() else {
                return failure(400, Code::Invalid);
            };
            if key != "scan" || !token(value) || !req.body.is_empty() {
                return failure(400, Code::Invalid);
            }
            let Ok(_read) = state.users_control.admission.try_read() else {
                return failure(503, Code::Busy);
            };
            if state.users_control.closed() {
                return failure(409, Code::SourceClosed);
            }
            extensions::status(&state, value)
        }
        _ => return failure(404, Code::Invalid),
    };
    match result {
        Ok(body) => Response {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: body.into(),
            ..Default::default()
        },
        Err(code) => failure(code.status(), code),
    }
}
