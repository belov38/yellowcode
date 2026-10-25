use super::http::{Request, Response};
use yellowcode_core::viewer::skin::Store;

fn json(status: u16, value: serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(&value).unwrap_or_default(),
        ..Default::default()
    }
}

pub fn handle(store: &Store, req: &Request) -> Option<Response> {
    if !matches!(
        req.path.as_str(),
        "/skin/vars.css" | "/api/skins" | "/api/skins/select"
    ) && !req.path.starts_with("/skin/icon/")
    {
        return None;
    }
    let mut skins = store.lock().unwrap_or_else(|e| e.into_inner());
    if req.path == "/api/skins/select" {
        if req.method != "POST" {
            return Some(json(405, serde_json::json!({"error":"use POST"})));
        }
        let id = serde_json::from_slice::<serde_json::Value>(&req.body)
            .ok()
            .and_then(|v| v.get("id")?.as_str().map(str::to_string));
        return Some(
            match id
                .ok_or_else(|| "id is required".to_string())
                .and_then(|id| skins.select(&id))
            {
                Ok(()) => json(200, skins.list()),
                Err(e) => json(400, serde_json::json!({"error":e})),
            },
        );
    }
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        return Some(json(405, serde_json::json!({"error":"use GET"})));
    }
    if req.path == "/api/skins" {
        return Some(json(200, skins.list()));
    }
    if req.path == "/skin/vars.css" {
        return Some(Response {
            status: 200,
            content_type: "text/css; charset=utf-8",
            body: skins.css().into_bytes(),
            ..Default::default()
        });
    }
    let key = req.path.strip_prefix("/skin/icon/")?;
    let double = req.query.iter().any(|(k, v)| k == "2x" && v == "1");
    Some(match skins.icon(key, double) {
        Some((ext, bytes)) => Response {
            status: 200,
            content_type: if ext == "svg" {
                "image/svg+xml"
            } else {
                "image/png"
            },
            body: bytes.to_vec(),
            csp: Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
            ..Default::default()
        },
        None => json(404, serde_json::json!({"error":"unknown skin icon"})),
    })
}
