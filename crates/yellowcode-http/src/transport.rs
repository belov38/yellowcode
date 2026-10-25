use std::io::{BufRead, Write};

const MAX_REQUEST_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
pub const MAX_BODY: usize = 256 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub host: Option<String>,
    pub sec_fetch_site: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct Response<B = Vec<u8>> {
    pub status: u16,
    pub content_type: &'static str,
    pub body: B,
    pub csp: Option<&'static str>,
    pub cache_control: Option<&'static str>,
}

#[derive(Debug)]
pub enum HttpError {
    BadRequest,
    UriTooLong,
    HeadersTooLarge,
    Method(String),
    LengthRequired,
    BodyTooLarge,
    UnsupportedMediaType,
    NotImplemented,
    Io(std::io::Error),
}

impl HttpError {
    pub fn status(&self) -> Option<u16> {
        match self {
            HttpError::BadRequest => Some(400),
            HttpError::UriTooLong => Some(414),
            HttpError::HeadersTooLarge => Some(431),
            HttpError::Method(_) => Some(405),
            HttpError::LengthRequired => Some(411),
            HttpError::BodyTooLarge => Some(413),
            HttpError::UnsupportedMediaType => Some(415),
            HttpError::NotImplemented => Some(501),
            HttpError::Io(_) => None,
        }
    }
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

fn read_line(
    r: &mut dyn BufRead,
    limit: usize,
    too_long: fn() -> HttpError,
) -> Result<Option<Vec<u8>>, HttpError> {
    let mut line = Vec::new();
    loop {
        let (used, done) = {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(HttpError::BadRequest)
                };
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(buf.get(..i).unwrap_or_default());
                    (i + 1, true)
                }
                None => {
                    line.extend_from_slice(buf);
                    (buf.len(), false)
                }
            }
        };
        r.consume(used);
        if line.len() > limit {
            return Err(too_long());
        }
        if done {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

pub fn read_json_request(r: &mut dyn BufRead, max_body: usize) -> Result<Request, HttpError> {
    read_request_with(r, &|_| true, max_body)
}

pub fn read_request_with(
    r: &mut dyn BufRead,
    accepts_post: &dyn Fn(&str) -> bool,
    max_body: usize,
) -> Result<Request, HttpError> {
    let line =
        read_line(r, MAX_REQUEST_LINE, || HttpError::UriTooLong)?.ok_or(HttpError::BadRequest)?;
    let line = String::from_utf8(line).map_err(|_| HttpError::BadRequest)?;
    let mut parts = line.split(' ');
    let (method, target, version) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v), None) => (m, t, v),
        _ => return Err(HttpError::BadRequest),
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() {
        return Err(HttpError::BadRequest);
    }
    let mut total = 0usize;
    let mut host = None;
    let mut sec_fetch_site = None;
    let mut lengths: Vec<String> = Vec::new();
    let mut content_type = None;
    let mut chunked = false;
    loop {
        let h = read_line(r, MAX_HEADERS, || HttpError::HeadersTooLarge)?
            .ok_or(HttpError::BadRequest)?;
        if h.is_empty() {
            break;
        }
        total += h.len() + 2;
        if total > MAX_HEADERS {
            return Err(HttpError::HeadersTooLarge);
        }
        if let Some((name, value)) = String::from_utf8_lossy(&h).split_once(':') {
            let value = value.trim().to_string();
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => host = Some(value),
                "sec-fetch-site" => sec_fetch_site = Some(value),
                "content-length" => lengths.push(value),
                "content-type" => content_type = Some(value),
                "transfer-encoding" => chunked = true,
                _ => {}
            }
        }
    }
    if !matches!(method, "GET" | "HEAD" | "POST") {
        return Err(HttpError::Method(method.to_string()));
    }
    if !target.starts_with('/') {
        return Err(HttpError::BadRequest);
    }
    let (raw_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let path = percent_decode(raw_path).ok_or(HttpError::BadRequest)?;
    if path.contains('\0') || path.split('/').any(|s| s == "..") {
        return Err(HttpError::BadRequest);
    }
    if method == "POST" && !accepts_post(&path) {
        return Err(HttpError::Method(method.to_string()));
    }
    let mut query = Vec::new();
    for pair in raw_query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.push((query_decode(k)?, query_decode(v)?));
    }
    let body = if method == "POST" {
        read_body(r, &lengths, content_type.as_deref(), chunked, max_body)?
    } else {
        Vec::new()
    };
    Ok(Request {
        method: method.to_string(),
        path,
        query,
        host,
        sec_fetch_site,
        body,
    })
}

fn read_body(
    r: &mut dyn BufRead,
    lengths: &[String],
    content_type: Option<&str>,
    chunked: bool,
    max_body: usize,
) -> Result<Vec<u8>, HttpError> {
    if chunked {
        return Err(HttpError::NotImplemented);
    }
    let len = match lengths {
        [] => return Err(HttpError::LengthRequired),
        [one] => one,
        _ => return Err(HttpError::BadRequest),
    };
    if len.is_empty() || !len.bytes().all(|b| b.is_ascii_digit()) {
        return Err(HttpError::BadRequest);
    }
    let len: usize = match len.parse() {
        Ok(n) if n <= max_body => n,
        _ => return Err(HttpError::BodyTooLarge),
    };
    let is_json = content_type.is_some_and(|c| {
        c.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    });
    if !is_json {
        return Err(HttpError::UnsupportedMediaType);
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

fn query_decode(s: &str) -> Result<String, HttpError> {
    percent_decode(&s.replace('+', " ")).ok_or(HttpError::BadRequest)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let h = hex(*b.get(i + 1)?)?;
            let l = hex(*b.get(i + 2)?)?;
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        501 => "Not Implemented",
        422 => "Unprocessable Content",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        414 => "URI Too Long",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

pub const PAGE_CSP: &str =
    "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'";

pub fn write_response<B: AsRef<[u8]>>(
    w: &mut dyn Write,
    resp: &Response<B>,
    head_only: bool,
) -> std::io::Result<()> {
    write_response_with_allow(w, resp, head_only, None)
}

pub fn write_response_with_allow<B: AsRef<[u8]>>(
    w: &mut dyn Write,
    resp: &Response<B>,
    head_only: bool,
    allow: Option<&str>,
) -> std::io::Result<()> {
    let cache = resp.cache_control.unwrap_or("no-store");
    write!(
        w,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.as_ref().len()
    )?;
    if let Some(csp) = resp.csp.or_else(|| {
        resp.content_type
            .starts_with("text/html")
            .then_some(PAGE_CSP)
    }) {
        write!(w, "Content-Security-Policy: {csp}\r\n")?;
    }
    if let Some(allow) = allow {
        write!(w, "Allow: {allow}\r\n")?;
    }
    w.write_all(b"Connection: close\r\n\r\n")?;
    if !head_only {
        w.write_all(resp.body.as_ref())?;
    }
    w.flush()
}
