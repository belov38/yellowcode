#![allow(dead_code)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub const MEMORY_ENV: [&str; 6] = [
    "YELLOWCODE_MEMORY_LIMIT",
    "YELLOWCODE_CACHE_LIMIT",
    "YELLOWCODE_QUERY_MEMORY_LIMIT",
    "YELLOWCODE_RESULT_MEMORY_LIMIT",
    "YELLOWCODE_RESPONSE_LIMIT",
    "YELLOWCODE_CONTAINER_MEMORY_BYTES",
];

pub fn command(bin: &str) -> Command {
    let mut command = Command::new(bin);
    for name in MEMORY_ENV {
        command.env_remove(name);
    }
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null());
    command
}

pub struct Service {
    child: Child,
    pub addr: SocketAddr,
    pub startup: Vec<String>,
    diagnostics: mpsc::Receiver<String>,
    busy: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub struct Response {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("{e}: {}", String::from_utf8_lossy(&self.body));
        })
    }
}

impl Service {
    pub fn start(bin: &str, base: &Path, flags: &[&str]) -> Self {
        Self::start_env(bin, base, flags, &[])
    }
    pub fn start_env(bin: &str, base: &Path, flags: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = command(bin);
        command
            .arg("serve")
            .arg(base)
            .args(["--listen", "127.0.0.1:0"])
            .args(flags)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (diagnostic_tx, diagnostics) = mpsc::sync_channel(256);
        let (tx, rx) = mpsc::channel();
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let busy_seen = busy.clone();
        std::thread::spawn(move || {
            let mut startup = Vec::new();
            let mut announced = false;
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if line.starts_with("memory_request ") {
                    if line.contains(" reason=busy ") {
                        busy_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        let _ = diagnostic_tx.try_send(line);
                    }
                    continue;
                }
                if !announced {
                    startup.push(line.clone());
                    if let Some(addr) = line.strip_prefix("yellowcode: listening on http://") {
                        announced = true;
                        let _ = tx.send(Ok((addr.parse::<SocketAddr>().unwrap(), startup.clone())));
                    }
                }
            }
            if !announced {
                let _ = tx.send(Err(startup.join("\n")));
            }
        });
        let (addr, startup) = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("server did not announce a listener")
            .unwrap_or_else(|transcript| panic!("serve did not start:\n{transcript}"));
        Self {
            child,
            addr,
            startup,
            diagnostics,
            busy,
        }
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn diagnostic(&self) -> String {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let line = self
                .diagnostics
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .expect("no memory_request diagnostic");
            if !line.contains(" reason=busy ") {
                return line;
            }
        }
    }
    pub fn busy_refusals(&self) -> usize {
        self.busy.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn diagnostic_or_refusal(&self, busy_before: usize) -> Option<String> {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            match self.diagnostics.recv_timeout(Duration::from_millis(5)) {
                Ok(line) if !line.contains(" reason=busy ") => return Some(line),
                _ if self.busy_refusals() > busy_before => return None,
                _ => {}
            }
            assert!(Instant::now() < end, "no diagnostic and no refusal");
        }
    }
    pub fn drain(&self) {
        while self.diagnostics.try_recv().is_ok() {}
    }
    pub fn exchange_once(&self, raw: &[u8]) -> Response {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(30)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let _ = stream.write_all(raw);
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response head");
        let head = String::from_utf8(response[..split].to_vec()).unwrap();
        let body = response[split + 4..].to_vec();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(head.contains("Content-Type: application/json"), "{head}");
        assert!(
            head.contains(&format!("Content-Length: {}\r\n", body.len())),
            "{head}"
        );
        Response { status, head, body }
    }
    pub fn exchange(&self, raw: &[u8]) -> Response {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let response = self.exchange_once(raw);
            let busy = response.status == 503
                && serde_json::from_slice::<Value>(&response.body)
                    .is_ok_and(|v| v["error"] == "query service is busy");
            if !busy {
                return response;
            }
            assert!(Instant::now() < end, "worker slot was not released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn post(&self, value: &Value) -> Response {
        self.exchange(&post_raw("/query", value))
    }
    pub fn get(&self, path: &str) -> Response {
        self.exchange(format!("GET {path} HTTP/1.1\r\nHost: example\r\n\r\n").as_bytes())
    }
    pub fn table(&self, kind: &str, name: &str, limit: usize, cursor: Option<&str>) -> Response {
        let mut path = format!(
            "/table/{}?name={}&limit={limit}",
            encode(kind),
            encode(name)
        );
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={}", encode(cursor)));
        }
        self.get(&path)
    }
    pub fn threads(&self) -> Option<usize> {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.pid())).ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|n| n.trim().parse().ok())
    }
}

pub fn post_raw(path: &str, value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap();
    let mut raw = format!("POST {path} HTTP/1.1\r\nHost: infra.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    raw.extend(body);
    raw
}

pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn number(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key))
        .unwrap_or_else(|| panic!("{key} missing in {line}"))
        .parse()
        .unwrap()
}

pub const REQUEST_CATEGORIES: [&str; 5] = ["execution", "result", "row", "input", "response"];
pub fn assert_request_released(line: &str) {
    for category in REQUEST_CATEGORIES {
        assert_eq!(
            number(line, &format!("{category}_live=")),
            0,
            "{category} retained: {line}"
        );
    }
}
