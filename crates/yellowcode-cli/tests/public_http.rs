#[path = "support/public_query_base.rs"]
pub(crate) mod base;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Service(Child, SocketAddr, mpsc::Receiver<String>);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Service {
    fn start(base: &Path) -> Self {
        Self::start_with(base, &[])
    }
    fn start_with(base: &Path, flags: &[&str]) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
            .arg("serve")
            .arg(base)
            .args(["--listen", "127.0.0.1:0"])
            .args(flags)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (diagnostic_tx, diagnostic_rx) = mpsc::sync_channel(32);
        let mut service = Self(child, "127.0.0.1:0".parse().unwrap(), diagnostic_rx);
        let stderr = service.0.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut transcript = String::new();
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if let Some(addr) = line.strip_prefix("yellowcode: listening on http://") {
                    let _ = tx.send(Ok(addr.parse::<SocketAddr>().unwrap()));
                }
                if line.starts_with("memory_request ") {
                    if !line.contains(" reason=busy ") {
                        let _ = diagnostic_tx.try_send(line);
                    }
                    continue;
                }
                transcript.push_str(&line);
                transcript.push('\n');
            }
            let _ = tx.send(Err(transcript));
        });
        service.1 = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("server did not announce a listener")
            .expect("serve must start without GUI");
        service
    }
    fn diagnostic(&self) -> String {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = self
                .2
                .recv_timeout(left)
                .expect("no memory_request diagnostic of an admitted request");
            if !line.contains(" reason=busy ") {
                return line;
            }
        }
    }
    fn request(&self, raw: &[u8]) -> (u16, Value) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let response = self.request_once(raw);
            if response.0 != 503 {
                return response;
            }
            assert!(Instant::now() < end, "slot was not released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn request_once(&self, raw: &[u8]) -> (u16, Value) {
        let mut stream = TcpStream::connect_timeout(&self.1, Duration::from_secs(30)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let _ = stream.write_all(raw);
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(head.contains("Content-Type: application/json"), "{head}");
        if status == 405 {
            assert!(head.contains("Allow: "), "{head}");
        }
        assert!(
            head.contains(&format!("Content-Length: {}\r\n", body.len())),
            "{head}"
        );
        (status, serde_json::from_str(body).unwrap())
    }
    fn post_raw(value: Value) -> Vec<u8> {
        Self::post_to("/query", value)
    }
    fn post_to(path: &str, value: Value) -> Vec<u8> {
        let body = serde_json::to_vec(&value).unwrap();
        let mut raw = format!("POST {path} HTTP/1.1\r\nHost: infra.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend(body);
        raw
    }
    fn post(&self, value: Value) -> (u16, Value) {
        self.request(&Self::post_raw(value))
    }
    fn get(&self, path: &str) -> (u16, Value) {
        self.request(format!("GET {path} HTTP/1.1\r\nHost: example\r\n\r\n").as_bytes())
    }
    fn table(&self, kind: &str, name: &str, limit: usize, cursor: Option<&str>) -> (u16, Value) {
        let encode = |s: &str| {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        };
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
    fn hold_incomplete(&self) -> TcpStream {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let mut held = TcpStream::connect(self.1).unwrap();
            held.write_all(b"POST /query HTTP/1.1\r\n").unwrap();
            let (status, _) = self.request_once(&Self::post_raw(json!({"text":"SELECT 1"})));
            if status == 503 && !answered(&held) {
                return held;
            }
            assert_eq!(status, 200);
            drop(held);
            assert!(Instant::now() < end, "could not acquire connection slot");
        }
    }
    fn available(&self) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, _) = self.post(json!({"text":"SELECT 1"}));
            if status != 503 {
                assert_eq!(status, 200);
                return;
            }
            assert!(Instant::now() < end, "slot was not released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn answered(stream: &TcpStream) -> bool {
    stream.set_nonblocking(true).unwrap();
    let pending =
        !matches!(stream.peek(&mut [0; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
    stream.set_nonblocking(false).unwrap();
    pending
}

#[test]
fn http_query_preserves_cli_json_and_base_and_reuses_service() {
    let tmp = tempfile::tempdir().unwrap();
    let base = base::create(tmp.path());
    let before = std::fs::read(base.join("1Cv8.1CD")).unwrap();
    let service = Service::start(&base);
    for _ in 0..2 {
        service.available();
        let (status, j) = service.post(json!({
            "text":"SELECT &P AS N, NULL AS Nil, &U AS U, &R AS R, &S AS S",
            "params":[{"name":"P","repr":"12345678901234567890.123456789"},
                      {"name":"U","repr":"Неопределено"},
                      {"name":"R","repr":"CatalogRef.Goods(00112233-4455-6677-8899-aabbccddeeff)"},
                      {"name":"S","repr":"\"ёжий\""}]
        }));
        assert_eq!(status, 200, "{j}");
        assert_eq!(j["schemaVersion"], 1);
        assert_eq!(j["error"], Value::Null);
        assert_eq!(j["cancelled"], false);
        assert_eq!(j["timedOut"], false);
        let row = &j["statements"][0]["rows"][0];
        assert_eq!(
            row[0],
            json!({"k":"number","t":"12345678901234567890.123456789"})
        );
        assert_eq!(row[1]["k"], "null");
        assert_eq!(row[2]["k"], "undefined");
        assert_eq!(row[3]["r"]["kind"], "CatalogRef.Goods");
        assert_eq!(row[4]["t"], "\"ёжий\"");
    }
    service.available();
    let (status, j) = service.post(json!({"text":format!("SELECT 42{}", " ".repeat(300 * 1024))}));
    assert_eq!(status, 200, "{j}");
    assert_eq!(j["statements"][0]["rows"][0][0]["t"], "42");
    assert_eq!(std::fs::read(base.join("1Cv8.1CD")).unwrap(), before);
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1);
}

#[test]
fn http_limits_errors_and_timeout_are_explicit() {
    let tmp = tempfile::tempdir().unwrap();
    let service = Service::start(&base::create(tmp.path()).join("1Cv8.1CD"));
    let (status, j) =
        service.post(json!({"text":"SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3", "maxRows":2}));
    assert_eq!(status, 200);
    assert_eq!(j["statements"][0]["total"], 3);
    assert_eq!(j["statements"][0]["kept"], 2);
    assert_eq!(j["statements"][0]["truncated"], true);
    service.available();
    let (status, j) = service.post(json!({"text":"SELECT 1 FROM"}));
    assert_eq!(status, 200);
    assert_eq!(j["error"]["phase"], "compile");
    assert!(j["error"]["at"].is_array());
    service.available();
    let (status, j) =
        service.post(json!({"text":"SELECT &P", "params":[{"name":"P","repr":"not a value"}]}));
    assert_eq!(status, 200);
    assert_eq!(j["error"]["phase"], "params");
    service.available();
    let (status, j) = service.post(json!({"text":"SELECT G.Barcodes FROM Catalog.Goods AS G"}));
    assert_eq!(status, 422);
    assert!(j["error"].as_str().unwrap().contains("nested"));
    service.available();
    let seed = (2..=8)
        .map(|n| format!(" UNION ALL SELECT {n}"))
        .collect::<String>();
    let from = (0..10)
        .map(|n| format!("T AS T{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let (status, j) = service.post(json!({"text":format!("SELECT 1 AS N INTO T{seed}; SELECT COUNT(*) FROM {from}"),"timeout":0.01}));
    assert_eq!(status, 200, "{j}");
    assert_eq!(j["cancelled"], true);
    assert_eq!(j["timedOut"], true);
    service.available();
}

#[test]
fn http_rejects_bad_requests_and_recovers_after_busy_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let service = Service::start(&base::create(tmp.path()));
    for request in [
        json!({}),
        json!({"text":12}),
        json!({"text":"SELECT 1","timeout":0}),
        json!({"text":"SELECT 1","timeout":-1}),
        json!({"text":"SELECT 1","maxRows":0}),
        json!({"text":"SELECT 1","maxRows":100001}),
        json!({"text":"SELECT 1","params":[{"name":"P","repr":"1"},{"name":"p","repr":"2"}]}),
        json!({"text":"SELECT 1","params":[{"name":" P","repr":"1"}]}),
        json!({"text":"SELECT 1","unknown":true}),
    ] {
        service.available();
        assert_eq!(service.post(request.clone()).0, 400, "{request}");
    }
    for (raw, status) in [
        ("GET /query HTTP/1.1\r\n\r\n", 405),
        ("GET / HTTP/1.1\r\n\r\n", 404),
        (
            "POST /unknown HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            404,
        ),
        (
            "POST /query HTTP/1.1\r\nContent-Type: application/json\r\n\r\n",
            411,
        ),
        (
            "POST /query HTTP/1.1\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n",
            415,
        ),
        (
            "POST /query HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n!!",
            400,
        ),
        (
            "POST /query HTTP/1.1\r\nContent-Length: 8388609\r\n\r\n",
            413,
        ),
        (
            "POST /query HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            501,
        ),
    ] {
        service.available();
        assert_eq!(service.request(raw.as_bytes()).0, status, "{raw}");
    }
    service.available();
    let held = service.hold_incomplete();
    assert_eq!(
        service
            .request_once(&Service::post_raw(json!({"text":"SELECT 1"})))
            .0,
        503
    );
    drop(held);
    service.available();
}

#[test]
fn serve_help_and_startup_failures_do_not_start_gui() {
    let tmp = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (vec!["serve", "--help"], 0),
        (vec!["serve"], 2),
        (vec!["serve", "missing", "--listen", "bad-address"], 2),
        (vec!["serve", "missing", "--listen", "127.0.0.1:0"], 1),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
            .args(&args)
            .current_dir(tmp.path())
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn conventional_http_client_and_incomplete_request_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let service = Service::start(&base::create(tmp.path()));
    let mut stream = TcpStream::connect(service.1).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let body = r#"{"text":"SELECT 42"}"#;
    write!(stream, "POST /query HTTP/1.1\r\nHost: example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 200 "), "{status}");
    let mut length = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["statements"][0]["rows"][0][0]["t"],
        "42"
    );
    drop(reader);
    service.available();
    let mut held = service.hold_incomplete();
    held.set_read_timeout(Some(Duration::from_secs(100)))
        .unwrap();
    assert_eq!(
        service
            .request_once(&Service::post_raw(json!({"text":"SELECT 1"})))
            .0,
        503
    );
    let started = Instant::now();
    assert_eq!(held.read(&mut [0u8; 1]).unwrap(), 0);
    assert!(started.elapsed() < Duration::from_secs(100));
    drop(held);
    service.available();
}

#[test]
fn direct_table_pages_preserve_typed_values_and_resume_across_holes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = base::create_named(tmp.path(), "ТоварыЁжий");
    base::populate(&dir, "ТоварыЁжий", 8, true);
    let before = std::fs::read(dir.join("1Cv8.1CD")).unwrap();
    let service = Service::start(&dir);

    let mut cursor = Value::Null;
    let mut all = Vec::new();
    let mut first_cursor = Value::Null;
    let mut columns = Value::Null;
    loop {
        let (status, page) = service.table("catalog", "тОвАрЫёЖиЙ", 2, cursor.as_str());
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["schemaVersion"], 1);
        let rows = page["rows"].as_array().unwrap();
        assert!(!rows.is_empty());
        assert!(rows.len() <= 2);
        if all.is_empty() {
            columns = page["columns"].clone();
            first_cursor = page["nextCursor"].clone();
        }
        assert_eq!(columns, page["columns"]);
        all.extend(rows.iter().cloned());
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(all.len() <= 6, "cursor did not advance");
    }
    assert_eq!(all.len(), 6);
    for computed in ["Предопределенный", "ИмяПредопределенныхДанных"]
    {
        assert!(
            !columns
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == computed)
        );
    }
    let (status, expected) =
        service.post(json!({"text":"SELECT T.ЭтоГруппа FROM Catalog.ТоварыЁжий AS T"}));
    assert_eq!(status, 200, "{expected}");
    assert_eq!(expected["error"], Value::Null, "{expected}");
    let position = |name: &str| {
        columns
            .as_array()
            .unwrap()
            .iter()
            .position(|c| c["name"] == name)
            .unwrap()
    };
    let folders: Vec<_> = all
        .iter()
        .map(|r| json!([r[position("ЭтоГруппа")].clone()]))
        .collect();
    assert_eq!(json!(folders), expected["statements"][0]["rows"]);
    let refs: std::collections::HashSet<_> = all
        .iter()
        .map(|r| r[position("Ссылка")]["r"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(refs.len(), 6);
    assert_eq!(all[0][position("Наименование")]["t"], "\"Товар ёжий 1\"");
    assert_eq!(
        all[0][position("N38_0")]["t"],
        "12345678901234567890123456789012345678"
    );
    assert_eq!(
        all[0][position("N32_8")]["t"],
        "12345678901234567890.12345678"
    );
    let a = service.table("catalog", "ТоварыЁжий", 2, first_cursor.as_str());
    let b = service.table("Справочник", "ТоварыЁжий", 2, first_cursor.as_str());
    assert_eq!(
        a, b,
        "stateless cursors must be repeatable across kind aliases"
    );
    assert_eq!(
        service
            .table("catalog", "Units", 2, first_cursor.as_str())
            .0,
        400
    );
    let other_service = Service::start(&dir);
    assert_eq!(
        other_service
            .table("catalog", "ТоварыЁжий", 2, first_cursor.as_str())
            .0,
        400
    );
    assert_eq!(std::fs::read(dir.join("1Cv8.1CD")).unwrap(), before);
}

#[test]
fn direct_table_request_validation_and_empty_page() {
    let tmp = tempfile::tempdir().unwrap();
    let service = Service::start(&base::create(tmp.path()));
    for (path, expected) in [
        ("/table/catalog", 400),
        ("/table/catalog?name=Goods&limit=0", 400),
        ("/table/catalog?name=Goods&limit=10001", 400),
        ("/table/catalog?name=Goods&limit=-1", 400),
        ("/table/catalog?name=Goods&cursor=bad", 400),
        ("/table/catalog?name=Goods.Barcodes", 400),
        ("/table/catalog?name=Missing", 404),
        ("/table/catalog?name=Goods&offset=100", 400),
        ("/table/catalog?name=Goods&limit=1&limit=2", 400),
        ("/table/catalog?name=Goods&name=Units", 400),
        ("/table/catalog?name=%FF", 400),
    ] {
        assert_eq!(service.get(path).0, expected, "{path}");
    }
    let (status, page) = service.get("/table/catalog/?name=Goods");
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["rows"], json!([]));
    assert_eq!(page["nextCursor"], Value::Null);
    assert!(!page["columns"].as_array().unwrap().is_empty());
    let end = Instant::now() + Duration::from_secs(30);
    let response = loop {
        let mut stream = TcpStream::connect(service.1).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let _ = stream.write_all(&Service::post_to("/table/catalog?name=Goods", json!({})));
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        if !response.starts_with("HTTP/1.1 503 ") {
            break response;
        }
        assert!(Instant::now() < end, "slot was not released");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(response.starts_with("HTTP/1.1 405 "), "{response}");
    assert!(response.contains("Allow: GET\r\n"), "{response}");
}

#[test]
#[ignore = "data:- mem:1G time:13s — release HTTP benchmark; HTTP2_BASE and HTTP2_TABLE select an immutable real base"]
fn benchmark_direct_table_http() {
    let tmp = tempfile::tempdir().unwrap();
    let external = std::env::var_os("HTTP2_BASE");
    let (dir, name) = if let Some(path) = external {
        (
            std::path::PathBuf::from(path),
            std::env::var("HTTP2_TABLE").unwrap(),
        )
    } else {
        let dir = base::create_named(tmp.path(), "ТоварыЁжий");
        base::populate(&dir, "ТоварыЁжий", 10_000, false);
        (dir, "ТоварыЁжий".to_owned())
    };
    let service = Service::start(&dir);
    let (status, first) = service.table("catalog", &name, 1000, None);
    assert_eq!(status, 200, "{first}");
    let fields = first["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| format!("T.{}", c["name"].as_str().unwrap()))
        .collect::<Vec<_>>()
        .join(", ");
    let select = |top: &str| format!("SELECT {top}{fields} FROM Catalog.{name} AS T");
    let query = |top: &str| {
        let (status, result) = service.post(json!({"text":select(top), "maxRows":100000}));
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["error"], Value::Null, "{result}");
        result["statements"][0]["rows"].as_array().unwrap().len()
    };
    let export = || {
        let mut cursor = None::<String>;
        let mut count = 0;
        loop {
            let (status, result) = service.table("catalog", &name, 1000, cursor.as_deref());
            assert_eq!(status, 200, "{result}");
            count += result["rows"].as_array().unwrap().len();
            cursor = result["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        count
    };
    let (status, equivalent) = service.post(json!({"text":select("TOP 1000 "), "maxRows":1000}));
    assert_eq!(status, 200, "{equivalent}");
    assert_eq!(equivalent["error"], Value::Null, "{equivalent}");
    assert_eq!(
        first["rows"], equivalent["statements"][0]["rows"],
        "benchmark must compare equivalent values"
    );
    let total = export();
    assert_eq!(query(""), total);
    let expected_first = total.min(1000);
    let mut cases = Vec::new();
    for case in [
        "table_first_1000",
        "query_top_1000",
        "table_export_1000",
        "query_export",
    ] {
        let mut ms = Vec::new();
        for _ in 0..9 {
            std::thread::sleep(Duration::from_millis(2));
            let start = Instant::now();
            let count = match case {
                "table_first_1000" => {
                    let (status, page) = service.table("catalog", &name, 1000, None);
                    assert_eq!(status, 200);
                    page["rows"].as_array().unwrap().len()
                }
                "query_top_1000" => query("TOP 1000 "),
                "table_export_1000" => export(),
                _ => query(""),
            };
            ms.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(
                count,
                if case.contains("export") {
                    total
                } else {
                    expected_first
                }
            );
        }
        ms.sort_by(f64::total_cmp);
        cases.push(json!({"case":case,"medianMs":ms[4],"p95Ms":ms[8],"samplesMs":ms}));
    }
    println!(
        "HTTP2_BENCH {}",
        json!({"table":name,"rows":total,"columns":first["columns"].as_array().unwrap().len(),"pageSize":1000,"repetitions":9,"cases":cases})
    );
}

#[test]
fn indexed_table_range_is_bounded_and_reports_real_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let base = base::create_named(tmp.path(), "ТоварыЁжий");
    base::populate(&base, "ТоварыЁжий", 20, false);
    let service = Service::start(&base);
    let encode = |s: &str| s.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
    let path = format!("/table/catalog?name={}", encode("ТоварыЁжий"));
    let (status, error) = service.get(&format!("{path}&index=missing"));
    assert_eq!(status, 422, "{error}");
    let indexes = error["availableIndexes"]
        .as_array()
        .expect("real indexes in diagnostic");
    let index = indexes
        .iter()
        .find(|i| i["fields"][0]["name"] == "Код")
        .expect("numeric code index");
    let index_name = index["name"].as_str().unwrap();
    assert_eq!(index["fields"][0]["conditions"], true);
    let url = format!(
        "{path}&index={}&range={}&limit=2",
        encode(index_name),
        encode(r#"{"gte":"5","lt":"10"}"#)
    );
    let (status, first) = service.get(&url);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["access"]["index"], index_name);
    let code = first["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["name"] == "Код")
        .unwrap();
    let mut rows = first["rows"].as_array().unwrap().clone();
    let mut next = first["nextCursor"].as_str().map(str::to_owned);
    while let Some(cursor) = next {
        let (status, page) = service.get(&format!("{url}&cursor={}", encode(&cursor)));
        assert_eq!(status, 200, "{page}");
        rows.extend(page["rows"].as_array().unwrap().iter().cloned());
        next = page["nextCursor"].as_str().map(str::to_owned);
        let (status, _) = service.get(&format!(
            "{path}&index={}&cursor={}",
            encode(index_name),
            encode(&cursor)
        ));
        assert_eq!(status, 400, "cursor must bind conditions");
    }
    let codes: Vec<_> = rows
        .iter()
        .map(|r| r[code]["t"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["5", "6", "7", "8", "9"]);
    let (status, eq) = service.get(&format!(
        "{path}&index={}&eq={}",
        encode(index_name),
        encode(r#"["7"]"#)
    ));
    assert_eq!(status, 200, "{eq}");
    assert_eq!(eq["rows"].as_array().unwrap().len(), 1);
    let (status, bad) = service.get(&format!(
        "{path}&index={}&eq={}",
        encode(index_name),
        encode(r#"["7.5"]"#)
    ));
    assert_eq!(status, 422, "{bad}");
    assert!(bad["availableIndexes"].is_array());
}

fn url_encode(s: &str) -> String {
    s.bytes().map(|b| format!("%{b:02X}")).collect()
}
fn code_index_url(service: &Service, name: &str) -> String {
    let path = format!("/table/catalog?name={}", url_encode(name));
    let (status, error) = service.get(&format!("{path}&index=missing"));
    assert_eq!(status, 422, "{error}");
    let indexes = error["availableIndexes"].as_array().unwrap();
    let index = indexes
        .iter()
        .find(|i| i["fields"][0]["name"] == "Код")
        .unwrap();
    format!(
        "{path}&index={}",
        url_encode(index["name"].as_str().unwrap())
    )
}
#[test]
fn indexed_duplicates_continue_without_gaps_and_reject_unsupported_conditions() {
    let tmp = tempfile::tempdir().unwrap();
    let base = base::create_named(tmp.path(), "ТоварыЁжий");
    base::populate_grouped(&base, "ТоварыЁжий", 2500, true, 2500);
    let service = Service::start(&base);
    let path = code_index_url(&service, "ТоварыЁжий");
    let url = format!("{path}&eq={}&limit=127", url_encode(r#"["1"]"#));
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    loop {
        let path = cursor
            .as_ref()
            .map(|c: &String| format!("{url}&cursor={}", url_encode(c)))
            .unwrap_or_else(|| url.clone());
        let (status, page) = service.get(&path);
        assert_eq!(status, 200, "{page}");
        for row in page["rows"].as_array().unwrap() {
            assert!(seen.insert(row.to_string()), "duplicate row across pages");
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 1667);
    for suffix in [
        format!("&eq={}", url_encode(r#"["\"ёжий\""]"#)),
        format!("&eq={}", url_encode(r#"["100000000"]"#)),
        format!("&eq={}", url_encode(r#"["NULL"]"#)),
        format!("&eq={}", url_encode(r#"["1","1","1","1"]"#)),
    ] {
        let (status, error) = service.get(&format!("{path}{suffix}"));
        assert_eq!(status, 422, "{error}");
        assert!(!error["availableIndexes"].as_array().unwrap().is_empty());
    }
    for suffix in [
        "&sort=Код".to_owned(),
        "&eq=broken".into(),
        format!("&range={}", url_encode(r#"{"gt":"1","gte":"1"}"#)),
        "&cursor=i1:00".into(),
    ] {
        let (status, error) = service.get(&format!("{path}{suffix}"));
        assert_eq!(status, 400, "{error}");
    }
    let (status, error) = service.get(&format!(
        "/table/catalog?name={}&range={}",
        url_encode("ТоварыЁжий"),
        url_encode(r#"{"gte":"1"}"#)
    ));
    assert_eq!(status, 400, "{error}");
}

#[test]
#[ignore = "data:- mem:1G time:4s — release benchmark of first and deep pages in duplicate index keys"]
fn benchmark_indexed_duplicate_pages_http() {
    let tmp = tempfile::tempdir().unwrap();
    let base = base::create_named(tmp.path(), "ТоварыЁжий");
    base::populate_grouped(&base, "ТоварыЁжий", 20_000, false, 20_000);
    let service = Service::start(&base);
    let path = code_index_url(&service, "ТоварыЁжий");
    let url = format!("{path}&eq={}&limit=100", url_encode(r#"["1"]"#));
    let mut cursor = None;
    for _ in 0..195 {
        let path = cursor
            .as_ref()
            .map(|c: &String| format!("{url}&cursor={}", url_encode(c)))
            .unwrap_or_else(|| url.clone());
        let (status, page) = service.get(&path);
        assert_eq!(status, 200, "{page}");
        cursor = Some(page["nextCursor"].as_str().unwrap().to_owned());
    }
    let deep = format!("{url}&cursor={}", url_encode(&cursor.unwrap()));
    let mut cases = Vec::new();
    for (case, url) in [("first_100", &url), ("after_19500_duplicates_100", &deep)] {
        let mut ms = Vec::new();
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(2));
            let start = Instant::now();
            let (status, page) = service.get(url);
            ms.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(status, 200, "{page}");
            assert_eq!(page["rows"].as_array().unwrap().len(), 100);
        }
        ms.sort_by(f64::total_cmp);
        cases.push(json!({"case":case,"medianMs":ms[7],"minMs":ms[0],"maxMs":ms[14]}));
    }
    println!(
        "HTTP3_BENCH {}",
        json!({"rows":20000,"fields":29,"cases":cases})
    );
}

#[test]
fn http_resource_refusal_and_next_success_emit_bounded_private_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let service = Service::start_with(
        &base::create(tmp.path()),
        &["--response-limit", "1KiB", "--cache-limit", "0"],
    );
    let secret = "private-value-that-must-not-appear";
    let oversized = format!("SELECT \"{}\" AS S", secret.repeat(100));
    let (status, error) = service.post(json!({"text":oversized}));
    assert_eq!(status, 422, "{error}");
    assert_eq!(error["error"], "MEMORY_LIMIT: response");
    let first = service.diagnostic();
    assert!(
        first.contains("status=422 reason=memory_response transport=sent"),
        "{first}"
    );
    let (status, result) = service.post(json!({"text":"SELECT 1 AS N"}));
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["error"], Value::Null);
    let second = service.diagnostic();
    assert!(
        second.contains("status=200 reason=ok transport=sent"),
        "{second}"
    );
    let number = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    assert!(number(&first, "id=") < number(&second, "id="));
    for line in [&first, &second] {
        assert!(line.len() < 4096);
        assert!(!line.contains(secret));
        assert!(!line.contains("SELECT"));
        assert!(number(line, "wire_bytes=") > 0);
        assert_eq!(number(line, "input_live="), 0);
        assert_eq!(number(line, "response_live="), 0);
        for category in [
            "metadata",
            "reader_working",
            "cache",
            "execution",
            "result",
            "row",
            "input",
            "response",
        ] {
            assert!(line.contains(&format!("{category}_live=")), "{line}");
            assert!(line.contains(&format!("{category}_peak=")), "{line}");
        }
    }
}
