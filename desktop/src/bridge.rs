use yellowcode_core::viewer::state::Viewer;
use yellowcode_http::transport::{Body, PAGE_CSP, Response};
use yellowcode_http::viewer::dispatch as handle_viewer;
use yellowcode_http::viewer::read_request;

const START_PAGE: &str = include_str!("start.html");
const START_SCRIPT: &str = include_str!("start.js");

pub struct Reply {
    pub status: u16,
    pub content_type: String,
    pub csp: Option<&'static str>,
    pub cache_control: &'static str,
    pub body: Body,
}

impl Reply {
    fn from_response(r: Response) -> Reply {
        let csp = r
            .csp
            .or_else(|| r.content_type.starts_with("text/html").then_some(PAGE_CSP));
        Reply {
            status: r.status,
            content_type: r.content_type.to_string(),
            csp,
            cache_control: r.cache_control.unwrap_or("no-store"),
            body: r.body,
        }
    }

    fn text(status: u16, body: &str) -> Reply {
        Reply {
            status,
            content_type: "text/plain; charset=utf-8".into(),
            csp: None,
            cache_control: "no-store",
            body: body.as_bytes().to_vec().into(),
        }
    }

    fn json(status: u16, body: &serde_json::Value) -> Reply {
        Reply {
            status,
            content_type: "application/json; charset=utf-8".into(),
            csp: None,
            cache_control: "no-store",
            body: serde_json::to_vec(body).unwrap_or_default().into(),
        }
    }
}

pub fn internal_error() -> Reply {
    Reply::json(500, &serde_json::json!({ "error": "internal error" }))
}

pub fn busy() -> Reply {
    Reply::json(503, &serde_json::json!({ "error": "busy" }))
}

pub fn respond(state: Option<&Viewer>, method: &str, path_and_query: &str) -> Reply {
    respond_with_body(state, method, path_and_query, None, &[])
}

pub fn respond_with_body(
    state: Option<&Viewer>,
    method: &str,
    path_and_query: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Reply {
    respond_internal(
        None,
        state,
        method,
        path_and_query,
        content_type,
        body,
        None,
    )
}

pub fn respond_with_skins(
    skins: &yellowcode_core::viewer::skin::Store,
    state: Option<&Viewer>,
    method: &str,
    path_and_query: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Reply {
    respond_internal(
        Some(skins),
        state,
        method,
        path_and_query,
        content_type,
        body,
        None,
    )
}

pub struct LocalRequestHeaders<'a> {
    pub authority: &'a str,
    pub origin: Option<&'a str>,
    pub fetch_site: Option<&'a str>,
    pub permission: Option<&'a str>,
}

pub fn respond_local(
    skins: &yellowcode_core::viewer::skin::Store,
    state: Option<&Viewer>,
    method: &str,
    target: &str,
    content_type: Option<&str>,
    body: &[u8],
    headers: LocalRequestHeaders<'_>,
) -> Reply {
    respond_internal(
        Some(skins),
        state,
        method,
        target,
        content_type,
        body,
        Some(headers),
    )
}

fn respond_internal(
    skins: Option<&yellowcode_core::viewer::skin::Store>,
    state: Option<&Viewer>,
    method: &str,
    path_and_query: &str,
    content_type: Option<&str>,
    body: &[u8],
    headers: Option<LocalRequestHeaders<'_>>,
) -> Reply {
    let odd = |s: &str| s.chars().any(|c| c.is_control() || c == ' ');
    if odd(method)
        || odd(path_and_query)
        || content_type.is_some_and(|c| c.chars().any(char::is_control))
    {
        return Reply::text(400, "error\n");
    }
    let mut raw = format!("{method} {path_and_query} HTTP/1.1\r\n");
    if let Some(headers) = headers {
        if !matches!(headers.authority, "localhost" | "onec.localhost") {
            return Reply::text(403, "local viewer required\n");
        }
        raw.push_str("Host: onec.localhost\r\n");
        for (name, value) in [
            ("Origin", headers.origin),
            ("Sec-Fetch-Site", headers.fetch_site),
            ("X-User-Write-Permission", headers.permission),
        ] {
            if let Some(value) = value {
                if value.chars().any(char::is_control) || value.len() > 4096 {
                    return Reply::text(400, "error\n");
                }
                raw.push_str(&format!("{name}: {value}\r\n"));
            }
        }
    }
    let mut tail: &[u8] = &[];
    if method == "POST" {
        if let Some(c) = content_type {
            raw.push_str(&format!("Content-Type: {c}\r\n"));
        }
        raw.push_str(&format!("Content-Length: {}\r\n", body.len()));
        let limit = if path_and_query.split('?').next() == Some("/api/users/apply") {
            64 * 1024
        } else {
            yellowcode_http::transport::MAX_BODY
        };
        if body.len() <= limit {
            tail = body;
        }
    }
    raw.push_str("\r\n");
    let mut raw = raw.into_bytes();
    raw.extend_from_slice(tail);
    let req = match read_request(&mut std::io::Cursor::new(raw)) {
        Ok(r) => r,
        Err(e) => return Reply::text(e.status().unwrap_or(400), "error\n"),
    };
    if method == "GET" {
        let picture: Option<(&str, &[u8])> = match req.path.as_str() {
            "/startup/splash.svg" => Some((
                "image/svg+xml",
                include_bytes!("startup_pictures/splash.svg"),
            )),
            "/startup/progress-bar.png" => Some((
                "image/png",
                include_bytes!("startup_pictures/progress-bar.png"),
            )),
            "/startup/progress-border.png" => Some((
                "image/png",
                include_bytes!("startup_pictures/progress-border.png"),
            )),
            _ => None,
        };
        if let Some((content_type, bytes)) = picture {
            return Reply {
                status: 200,
                content_type: content_type.into(),
                csp: Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
                cache_control: "no-store",
                body: bytes.to_vec().into(),
            };
        }
        if req.path == "/skin/icon/pic/splash" {
            let custom = skins
                .and_then(|store| store.lock().ok())
                .is_some_and(|store| store.icon("pic/splash", false).is_some());
            if !custom {
                return Reply {
                    status: 200,
                    content_type: "image/svg+xml".into(),
                    csp: Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
                    cache_control: "no-store",
                    body: include_bytes!("startup_pictures/splash.svg")
                        .to_vec()
                        .into(),
                };
            }
        }
    }
    if let Some(skins) = skins
        && let Some(response) = yellowcode_http::viewer::skin::handle(skins, &req)
    {
        return Reply::from_response(response);
    }
    if matches!(
        req.path.as_str(),
        "/start.html" | "/splash.html" | "/start.js"
    ) {
        return Reply {
            status: 200,
            content_type: if req.path == "/start.js" {
                "text/javascript; charset=utf-8"
            } else {
                "text/html; charset=utf-8"
            }
            .into(),
            csp: (req.path != "/start.js").then_some(PAGE_CSP),
            cache_control: "no-store",
            body: if req.path == "/start.js" {
                START_SCRIPT
            } else {
                START_PAGE
            }
            .as_bytes()
            .to_vec()
            .into(),
        };
    }
    match state {
        Some(v) => Reply::from_response(handle_viewer(v, &req)),
        None if req.path == "/" => Reply {
            status: 200,
            content_type: "text/html; charset=utf-8".into(),
            csp: Some(PAGE_CSP),
            cache_control: "no-store",
            body: START_PAGE.as_bytes().to_vec().into(),
        },
        None if req.path == "/start.js" => Reply {
            status: 200,
            content_type: "text/javascript; charset=utf-8".into(),
            csp: None,
            cache_control: "no-store",
            body: START_SCRIPT.as_bytes().to_vec().into(),
        },
        None if req.path.starts_with("/api/") => Reply::json(
            503,
            &serde_json::json!({ "error": "no configuration is open" }),
        ),
        None => Reply::text(404, "not found\n"),
    }
}
