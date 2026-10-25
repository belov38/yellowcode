use onec_desktop::bridge::respond;
use onec_desktop::recent::Recent;
use yellowcode_core::viewer::state::{State, Viewer};
use yellowcode_http::viewer::dispatch as handle_viewer;
use yellowcode_http::viewer::read_request;

fn base3() -> Viewer {
    Viewer::new(
        State::load(
            &cf_fixtures::fixture_dir("base3").join("p.cf"),
            None,
            yellowcode_core::viewer::Lang::Ru,
        )
        .unwrap(),
    )
}

#[test]
fn without_source() {
    let r = respond(None, "GET", "/");
    assert_eq!(r.status, 200);
    assert!(String::from_utf8(r.body).unwrap().contains("id=\"open\""));
    assert_eq!(respond(None, "GET", "/api/tree").status, 503);
    assert_eq!(respond(None, "GET", "/nope").status, 404);
}

#[test]
fn pages_allow_no_inline_script() {
    let csp_of = |r: &onec_desktop::bridge::Reply| r.csp.unwrap_or_default().to_string();
    let start = respond(None, "GET", "/");
    let page = respond(Some(&base3()), "GET", "/");
    for r in [&start, &page] {
        let csp = csp_of(r);
        let script_src = csp
            .split(';')
            .find(|d| d.trim().starts_with("script-src"))
            .unwrap_or("");
        assert_eq!(script_src.trim(), "script-src 'self'", "{csp}");
    }
    let html = String::from_utf8(start.body).unwrap();
    assert!(html.contains("<script src=\"/start.js\"></script>"));
    assert!(
        !html
            .replace("<script src=\"/start.js\">", "")
            .contains("<script>")
    );
    let js = respond(None, "GET", "/start.js");
    assert_eq!(js.status, 200);
    assert!(js.content_type.starts_with("text/javascript"));
    assert!(String::from_utf8(js.body).unwrap().contains("__TAURI__"));
}

#[test]
fn same_answers_as_the_server() {
    let st = base3();
    for target in [
        "/",
        "/api/info",
        "/api/tree?id=0",
        "/api/stats",
        "/api/show?path=Catalog.PCatalog",
        "/api/nope",
        "/api/show?path=%D0%A1%D0%BF",
        "/api/tree?id=x",
    ] {
        let raw = format!("GET {target} HTTP/1.1\r\n\r\n");
        let req = read_request(&mut std::io::Cursor::new(raw.into_bytes())).unwrap();
        let want = handle_viewer(&st, &req);
        let got = respond(Some(&st), "GET", target);
        let want_csp = want.csp.or_else(|| {
            want.content_type
                .starts_with("text/html")
                .then_some(yellowcode_http::transport::PAGE_CSP)
        });
        assert_eq!(got.csp, want_csp, "{target} csp");
        assert_eq!(
            (
                got.status,
                got.content_type.as_str(),
                got.cache_control,
                got.body
            ),
            (
                want.status,
                want.content_type,
                want.cache_control.unwrap_or("no-store"),
                want.body
            ),
            "{target}"
        );
    }
    assert!(respond(Some(&st), "GET", "/").csp.is_some());
}

#[test]
fn odd_requests() {
    let st = base3();
    assert_eq!(respond(Some(&st), "POST", "/api/info").status, 405);
    assert_eq!(respond(Some(&st), "GET", "/api/show?path=%").status, 400);
    assert!(matches!(
        respond(Some(&st), "GET", "/../etc/passwd").status,
        400 | 404
    ));
    assert_eq!(respond(Some(&st), "GET", "/a b").status, 400);
    assert_eq!(respond(Some(&st), "GET", "/a\r\nHost: x").status, 400);
    assert_eq!(respond(None, "POST", "/").status, 405);
    let long = format!("/api/find?q={}", "a".repeat(100_000));
    assert!(matches!(respond(Some(&st), "GET", &long).status, 400 | 414));
}

#[test]
fn recent_list() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("recent.json");
    std::fs::write(&f, b"{not json").unwrap();
    let mut r = Recent::load(&f);
    assert!(r.items.is_empty());
    let a = d.path().join("a.cf");
    std::fs::write(&a, b"x").unwrap();
    for _ in 0..3 {
        r.touch(&a);
    }
    for i in 0..12 {
        r.touch(&d.path().join(format!("gone{i}.cf")));
    }
    assert_eq!(r.items.len(), 10);
    r.touch(&a);
    r.save(&f).unwrap();
    let r = Recent::load(&f);
    assert_eq!(r.items[0].path, dunce::canonicalize(&a).unwrap());
    assert!(r.items[0].exists && !r.items[1].exists);
}

#[test]
fn huge_recent_file_is_ignored() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("recent.json");
    let item = format!("{{\"items\":[\"/a\"],\"pad\":\"{}\"}}", " ".repeat(2 << 20));
    std::fs::write(&f, item).unwrap();
    assert!(Recent::load(&f).items.is_empty());
}

#[test]
fn unchecked_load_touches_no_listed_path() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("recent.json");
    let list = serde_json::json!({ "items": ["/no/such/a.cf", "/no/such/b.cf", "/no/such/a.cf"] });
    std::fs::write(&f, list.to_string()).unwrap();
    let r = Recent::load_unchecked(&f);
    assert_eq!(r.items.len(), 2);
    assert!(r.items.iter().all(|i| i.exists));
    let now = Recent::exists_now(&r.items);
    assert!(now.iter().all(|i| !i.exists));
    let mut r = r;
    r.remove(std::path::Path::new("/no/such/a.cf"));
    assert_eq!(r.items.len(), 1);
}

fn qr_main_state() -> Option<Viewer> {
    let b = std::env::var_os("QUERY_BASES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("out/query/bases"));
    let f = b.join("qr-main/1Cv8.1CD");
    if !f.is_file() {
        eprintln!("SKIP: {} not found", f.display());
        return None;
    }
    Some(Viewer::new(
        State::load(&f, None, yellowcode_core::viewer::Lang::Ru)
            .unwrap_or_else(|e| panic!("{}: {e}", f.display())),
    ))
}

const JSON: Option<&str> = Some("application/json");

#[test]
fn post_to_the_console_answers_like_the_server() {
    let Some(st) = qr_main_state() else { return };
    let body = r#"{"text":"ВЫБРАТЬ 1 ИЗ"}"#.as_bytes();
    let got = onec_desktop::bridge::respond_with_body(
        Some(&st),
        "POST",
        "/api/x/query/parameters",
        JSON,
        body,
    );
    let raw = format!(
        "POST /api/x/query/parameters HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        std::str::from_utf8(body).unwrap()
    );
    let req = read_request(&mut std::io::Cursor::new(raw.into_bytes())).unwrap();
    let want = handle_viewer(&st, &req);
    assert_eq!((got.status, got.body), (want.status, want.body));
    assert_eq!(got.status, 200);
}

#[test]
fn post_is_limited_like_on_the_socket() {
    let st = base3();
    let r = |m: &str, p: &str, c: Option<&str>, b: &[u8]| {
        onec_desktop::bridge::respond_with_body(Some(&st), m, p, c, b).status
    };
    assert_eq!(r("POST", "/api/info", JSON, b"{}"), 405);
    assert_eq!(
        r("POST", "/api/x/query/run", Some("text/plain"), b"{}"),
        415
    );
    assert_eq!(r("POST", "/api/x/query/run", None, b"{}"), 415);
    let big = vec![b' '; 300 * 1024];
    assert_eq!(r("POST", "/api/x/query/run", JSON, &big), 413);
    assert_eq!(r("PUT", "/api/x/query/run", JSON, b"{}"), 405);
    assert_eq!(r("POST", "/api/x/query/run", Some("a\r\nb: c"), b"{}"), 400);
    assert_eq!(r("POST", "/api/x/query/run", JSON, b"{}"), 409);
    assert_eq!(r("GET", "/api/x/query/info", JSON, b"{}"), 200);
}

#[test]
fn a_running_query_does_not_hold_info() {
    let Some(st) = qr_main_state() else { return };
    let from = (1..=9)
        .map(|i| format!("Справочник.Samples КАК Т{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let body = serde_json::json!({
        "text": format!("ВЫБРАТЬ КОЛИЧЕСТВО(*) КАК N ИЗ {from}"),
        "params": [], "mode": "batch", "lang": "ru"
    })
    .to_string();
    let run = onec_desktop::bridge::respond_with_body(
        Some(&st),
        "POST",
        "/api/x/query/run",
        JSON,
        body.as_bytes(),
    );
    assert_eq!(run.status, 200);
    let id = serde_json::from_slice::<serde_json::Value>(&run.body).unwrap()["id"]
        .as_u64()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let t = std::time::Instant::now();
    assert_eq!(respond(Some(&st), "GET", "/api/x/query/info").status, 200);
    let job = respond(Some(&st), "GET", &format!("/api/x/query/job?id={id}"));
    assert!(String::from_utf8(job.body).unwrap().contains("running"));
    assert!(t.elapsed() < std::time::Duration::from_secs(30));
    let c = onec_desktop::bridge::respond_with_body(
        Some(&st),
        "POST",
        &format!("/api/x/query/cancel?id={id}"),
        JSON,
        b"{}",
    );
    assert_eq!(c.status, 200);
}

#[test]
fn startup_routes_and_platform_art_are_available_with_and_without_a_model() {
    let state = base3();
    for model in [None, Some(&state)] {
        for path in [
            "/start.html",
            "/splash.html",
            "/start.js",
            "/startup/splash.svg",
            "/startup/progress-bar.png",
            "/startup/progress-border.png",
            "/skin/icon/pic/splash",
        ] {
            let response = respond(model, "GET", path);
            assert_eq!(response.status, 200, "{path}");
            assert!(!response.body.is_empty(), "{path}");
        }
    }
}

fn tree_paths(v: &Viewer) -> Vec<String> {
    fn rec(v: &Viewer, id: u64, out: &mut Vec<String>) {
        let r = respond(Some(v), "GET", &format!("/api/tree?id={id}"));
        let kids: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        for k in kids.as_array().unwrap() {
            if let Some(p) = k["path"].as_str() {
                out.push(p.to_string());
            }
            if k["children"].as_u64().unwrap_or(0) > 0 {
                rec(v, k["id"].as_u64().unwrap(), out);
            }
        }
    }
    let mut out = Vec::new();
    rec(v, 0, &mut out);
    out
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn a_part_loads_behind_a_loading_answer_over_the_scheme() {
    use std::sync::{Arc, Condvar, Mutex};
    let cf = cf_fixtures::fixture_dir("base3").join("p.cf");
    let full = base3();
    let form = tree_paths(&full)
        .into_iter()
        .find(|p| p.contains(".Form."))
        .unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let real = yellowcode_core::viewer::state::source_loader();
    let loader: yellowcode_core::viewer::state::Loader = Arc::new(move |p, s, e| {
        let (open, cv) = &*g;
        let mut o = open.lock().unwrap();
        while !*o {
            o = cv.wait(o).unwrap();
        }
        drop(o);
        real(p, s, e)
    });
    let st = State::load_stages_with_progress(
        &cf,
        None,
        yellowcode_core::viewer::Lang::Ru,
        Viewer::OPEN,
        &mut |_| Ok(()),
    )
    .unwrap();
    let lazy = Viewer::with_loader(st, loader);
    let (_alive, ended) = std::sync::mpsc::channel::<()>();
    {
        let gate = gate.clone();
        std::thread::spawn(move || {
            if ended.recv_timeout(std::time::Duration::from_secs(60))
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                let (open, cv) = &*gate;
                *open.lock().unwrap() = true;
                cv.notify_all();
            }
        });
    }
    let target = format!("/api/form-structure?path={}", enc(&form));
    let t0 = std::time::Instant::now();
    let r = respond(Some(&lazy), "GET", &target);
    assert!(t0.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(r.status, 503);
    let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["loading"], "forms, pictures", "{j}");
    assert!(j["retry_ms"].as_u64().unwrap() > 0);
    let page = respond(Some(&lazy), "GET", &format!("/form?path={}", enc(&form)));
    assert_eq!(page.status, 503);
    assert!(page.content_type.starts_with("text/html"));
    assert!(String::from_utf8_lossy(&page.body).contains("http-equiv=\"refresh\""));
    assert_eq!(respond(Some(&lazy), "GET", "/api/info").status, 200);
    {
        let (open, cv) = &*gate;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
    let t0 = std::time::Instant::now();
    let got = loop {
        let r = respond(Some(&lazy), "GET", &target);
        let loading = r.status == 503
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .is_ok_and(|j| j.get("loading").is_some());
        if !loading {
            break r;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let want = respond(Some(&full), "GET", &target);
    assert_eq!((got.status, got.body), (want.status, want.body));
    assert_eq!(lazy.loads(), 1);
}
