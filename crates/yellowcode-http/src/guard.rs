use crate::transport::{Request, Response};

pub fn guard(req: &Request, port: u16, protected: &dyn Fn(&str) -> bool) -> Option<Response> {
    if let Some(h) = &req.host {
        let h = h.to_ascii_lowercase();
        if h != format!("127.0.0.1:{port}") && h != format!("localhost:{port}") {
            return Some(text(421, "misdirected request\n"));
        }
    }
    if protected(&req.path) && req.sec_fetch_site.as_deref() == Some("cross-site") {
        return Some(text(403, "cross-site request\n"));
    }
    None
}

pub fn text(status: u16, body: &str) -> Response {
    Response {
        status,
        content_type: "text/plain; charset=utf-8",
        body: body.as_bytes().to_vec().into(),
        ..Default::default()
    }
}
