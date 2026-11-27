use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::tls::{self, Tls, TlsStream};

pub const MAX_RESPONSE_BODY: usize = 64 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
pub const ONCE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("bad base URL {0:?}: expected http://host[:port][/path] or https://host[:port][/path]")]
    BadUrl(String),
    #[error("--ca-cert applies to https:// only, the URL {0:?} is not https://")]
    CaCertWithoutTls(String),
    #[error("CA certificate file {}: {reason}", path.display())]
    CaCert { path: PathBuf, reason: String },
    #[error("TLS: {0}")]
    Tls(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("malformed HTTP response: {0}")]
    Malformed(String),
    #[error("response {0} exceeds {1} bytes")]
    TooLarge(&'static str, usize),
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub prefix: String,
}

impl BaseUrl {
    pub fn parse(url: &str) -> Result<Self, TransportError> {
        let bad = || TransportError::BadUrl(url.to_string());
        let (tls, rest) = match (url.strip_prefix("http://"), url.strip_prefix("https://")) {
            (Some(rest), _) => (false, rest),
            (None, Some(rest)) => (true, rest),
            (None, None) => return Err(bad()),
        };
        let default_port = if tls { 443 } else { 80 };
        let (authority, path) = match rest.find('/') {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(bad());
        }
        let (host, port) = if let Some(h) = authority.strip_prefix('[') {
            let (h, after) = h.split_once(']').ok_or_else(bad)?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad())?,
                None if after.is_empty() => default_port,
                None => return Err(bad()),
            };
            (format!("[{h}]"), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad())?),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            tls,
            host,
            port,
            prefix: path.trim_end_matches('/').to_string(),
        })
    }

    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    fn socket_addr(&self) -> String {
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        format!("{host}:{}", self.port)
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => s.get_ref(),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

pub struct HttpTransport {
    base: BaseUrl,
    base_location: String,
    tls: Option<Tls>,
    conn: Option<BufReader<Stream>>,
}

impl HttpTransport {
    pub fn new(base_url: &str) -> Result<Self, TransportError> {
        Self::with_ca_certs(base_url, &[])
    }

    pub fn with_ca_certs(base_url: &str, ca_files: &[PathBuf]) -> Result<Self, TransportError> {
        let base = BaseUrl::parse(base_url)?;
        if !base.tls && !ca_files.is_empty() {
            return Err(TransportError::CaCertWithoutTls(base_url.to_string()));
        }
        let tls = if base.tls {
            Some(Tls::new(&base.host, ca_files)?)
        } else {
            None
        };
        Ok(Self {
            base,
            base_location: base_url.trim_end_matches('/').to_string(),
            tls,
            conn: None,
        })
    }

    pub fn base_location(&self) -> &str {
        &self.base_location
    }

    fn connect(&self) -> Result<BufReader<Stream>, TransportError> {
        let s = TcpStream::connect(self.base.socket_addr())?;
        s.set_read_timeout(Some(TIMEOUT))?;
        s.set_write_timeout(Some(TIMEOUT))?;
        s.set_nodelay(true)?;
        let stream = match &self.tls {
            None => Stream::Plain(s),
            Some(tls) => Stream::Tls(Box::new(tls.connect(s)?)),
        };
        Ok(BufReader::new(stream))
    }

    pub fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, TransportError> {
        let req = self.request_bytes(method, path, headers, body);
        let had_conn = self.conn.is_some();
        match self.send_and_read(&req) {
            Ok(r) => Ok(r),
            Err(TransportError::Io(_)) | Err(TransportError::Malformed(_)) if had_conn => {
                self.conn = None;
                self.send_and_read(&req)
            }
            Err(e) => Err(e),
        }
    }

    pub fn exchange_once(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, TransportError> {
        let req = self.request_bytes(method, path, headers, body);
        self.conn = None;
        let mut conn = self.connect()?;
        conn.get_ref().tcp().set_read_timeout(Some(ONCE_TIMEOUT))?;
        conn.get_mut().write_all(&req)?;
        conn.get_mut().flush()?;
        let resp = read_response(&mut conn)?;
        conn.get_ref().tcp().set_read_timeout(Some(TIMEOUT))?;
        self.conn = Some(conn);
        Ok(resp)
    }

    fn request_bytes(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Vec<u8> {
        let mut req = Vec::new();
        let full = format!("{}{path}", self.base.prefix);
        req.extend_from_slice(format!("{method} {full} HTTP/1.1\r\n").as_bytes());
        req.extend_from_slice(format!("Host: {}\r\n", self.base.authority()).as_bytes());
        req.extend_from_slice(format!("1C-BaseLocation: {}\r\n", self.base_location).as_bytes());
        for (k, v) in headers {
            req.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        req.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        req.extend_from_slice(b"\r\n");
        req.extend_from_slice(body);
        req
    }

    fn send_and_read(&mut self, req: &[u8]) -> Result<Response, TransportError> {
        if self.conn.is_none() {
            self.conn = Some(self.connect()?);
        }
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| TransportError::Malformed("no connection after connect".to_string()))?;
        let result = conn
            .get_mut()
            .write_all(req)
            .and_then(|()| conn.get_mut().flush())
            .map_err(TransportError::Io)
            .and_then(|()| read_response(conn));
        let result = match result {
            Err(TransportError::Io(e)) => Err(tls::classify(e, &self.base.host)),
            other => other,
        };
        if let Err(TransportError::Tls(_)) = result {
            self.conn = None;
        }
        result
    }
}

fn read_response<R: BufRead>(r: &mut R) -> Result<Response, TransportError> {
    let mut head = Vec::new();
    loop {
        let before = head.len();
        if r.read_until(b'\n', &mut head)? == 0 {
            return Err(TransportError::Malformed(
                "connection closed before headers".to_string(),
            ));
        }
        if head.len() > MAX_HEAD {
            return Err(TransportError::TooLarge("head", MAX_HEAD));
        }
        if head[before..].iter().all(|&b| b == b'\r' || b == b'\n')
            && head[before..].contains(&b'\n')
            && (head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n"))
        {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text
        .split("\r\n")
        .flat_map(|l| l.split('\n'))
        .filter(|l| !l.is_empty());
    let status_line = lines
        .next()
        .ok_or_else(|| TransportError::Malformed("empty response".to_string()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| TransportError::Malformed(format!("bad status line: {status_line:?}")))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let chunked = get("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let body = if chunked {
        read_chunked(r)?
    } else if let Some(len) = get("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
        if len > MAX_RESPONSE_BODY {
            return Err(TransportError::TooLarge("body", MAX_RESPONSE_BODY));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)?;
        body
    } else {
        let mut body = Vec::new();
        r.take(MAX_RESPONSE_BODY as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > MAX_RESPONSE_BODY {
            return Err(TransportError::TooLarge("body", MAX_RESPONSE_BODY));
        }
        body
    };
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn read_chunked<R: BufRead>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line)?;
        let size_hex = line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| TransportError::Malformed(format!("bad chunk size: {size_hex:?}")))?;
        if size == 0 {
            loop {
                let mut t = String::new();
                if r.read_line(&mut t)? == 0 || t == "\r\n" || t == "\n" {
                    break;
                }
            }
            break;
        }
        if body.len() + size > MAX_RESPONSE_BODY {
            return Err(TransportError::TooLarge("body", MAX_RESPONSE_BODY));
        }
        let mut chunk = vec![0u8; size];
        r.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf)?;
    }
    Ok(body)
}
