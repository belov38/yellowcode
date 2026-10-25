use http::base;
use std::process::{Command, Output};
fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap()
}
#[test]
fn help_version_and_open_help_do_not_start_gui() {
    let help = cli(&["--help"]);
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(
        text.contains("query") && text.contains("metadata") && text.contains("open"),
        "{text}"
    );
    let version = cli(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        format!("yellowcode {}", yellowcode_cli::VERSION)
    );
    let open = cli(&["open", "--help"]);
    assert!(
        open.status.success(),
        "{}",
        String::from_utf8_lossy(&open.stderr)
    );
    assert!(String::from_utf8_lossy(&open.stdout).contains("source"));
}
#[test]
fn metadata_and_query_route_without_display() {
    let source = cf_fixtures::fixture_dir("types").join("p.cf");
    let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .args(["metadata", "tree"])
        .arg(source)
        .args(["--depth", "0"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["schemaVersion"],
        1
    );
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .arg("query")
        .arg(tmp.path().join("absent"))
        .args(["--text", "SELECT 42"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such file"));
}
#[test]
fn invalid_command_and_flags_are_usage_errors() {
    for args in [
        vec!["not-a-command"],
        vec!["--bogus"],
        vec!["query"],
        vec!["open", "--bogus"],
        vec!["--lang", "en", "-bad"],
        vec!["source.cf", "-bad"],
    ] {
        let out = cli(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("Usage"));
    }
}
#[test]
fn legacy_source_and_options_remain_gui_arguments() {
    for args in [
        vec!["some.cf"],
        vec!["some.cf", "--lang", "en", "--smoke"],
        vec!["--lang", "ru", "--names", "dump", "some.cf"],
    ] {
        let opts =
            onec_desktop::parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        assert_eq!(opts.source.as_deref(), Some("some.cf"));
    }
    let source = cf_fixtures::fixture_dir("types").join("p.cf");
    assert!(!yellowcode_cli::is_cli(&[
        "yellowcode".into(),
        source.into_os_string()
    ]));
    assert!(!yellowcode_cli::is_cli(&[
        "yellowcode".into(),
        "--lang".into(),
        "en".into()
    ]));
}

#[test]
fn redirected_stdin_and_streams_execute_real_query() {
    use std::io::Write;
    use std::process::Stdio;
    let tmp = tempfile::tempdir().unwrap();
    let base = base::create(tmp.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .arg("query")
        .arg(base)
        .arg("--stdin")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("ВЫБРАТЬ 42 КАК Answer".as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["statements"][0]["rows"][0][0]["t"], "42");
    check_query_stderr(
        &String::from_utf8_lossy(&result.stderr),
        cfg!(target_os = "linux"),
    );
}

#[test]
fn legacy_flags_reject_unknown_short_options_but_keep_names_path_values() {
    for args in [vec!["--lang", "en", "-bad"], vec!["source.cf", "-bad"]] {
        assert!(
            onec_desktop::parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .is_err()
        );
    }
    let opts = onec_desktop::parse_args(&[
        "--names".into(),
        "-dump".into(),
        "source.cf".into(),
        "--smoke".into(),
    ])
    .unwrap();
    assert_eq!(opts.names.as_deref(), Some("-dump"));
    assert!(opts.smoke);
    for args in [
        vec!["yellowcode", "--lang", "en", "-bad"],
        vec!["yellowcode", "source.cf", "-bad"],
    ] {
        assert!(yellowcode_cli::is_cli(
            &args.iter().map(|s| s.into()).collect::<Vec<_>>()
        ));
    }
    for args in [
        vec!["yellowcode", "--names", "-dump", "source.cf"],
        vec!["yellowcode", "open", "--names", "-dump", "source.cf"],
    ] {
        assert!(!yellowcode_cli::is_cli(
            &args.iter().map(|s| s.into()).collect::<Vec<_>>()
        ));
    }
}

#[path = "../../crates/yellowcode-cli/tests/public_http.rs"]
mod http;

fn check_query_stderr(stderr: &str, finite: bool) {
    const FALLBACK: [&str; 3] = [
        "yellowcode: memory mode=fallback B=1073741824 ",
        "yellowcode: memory warning: external envelope discovery is partial or unknown; set YELLOWCODE_CONTAINER_MEMORY_BYTES or an explicit --memory-limit",
        "yellowcode: memory warning: no finite external envelope; fixed 1GiB fallback (UNF-class bases need --memory-limit >= 1792MiB, ERP-class about 3GiB)",
    ];
    let mut lines: Vec<&str> = stderr.lines().collect();
    if !finite
        && lines
            .first()
            .is_some_and(|l| l.starts_with("yellowcode: memory "))
    {
        let summary = lines.remove(0);
        assert!(summary.starts_with(FALLBACK[0]), "{stderr}");
        assert!(
            summary.contains(" envelope=none ") && summary.contains(" sources=none "),
            "{stderr}"
        );
        assert_eq!(lines.get(..2), Some(&FALLBACK[1..]), "{stderr}");
        lines.drain(..2);
    }
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].starts_with("memory_request id="), "{stderr}");
}

#[test]
fn query_stderr_of_a_host_without_a_finite_envelope() {
    let fallback = "yellowcode: memory mode=fallback B=1073741824 A=1069547520 E=4194304 cache=213909504 query=428867584 result=160432128 response=67108864 row=33554432 envelope=none visibility=unknown sources=none policy=70% settings=memory:default,cache:default,query:default,result:default,response:default\n\
yellowcode: memory warning: external envelope discovery is partial or unknown; set YELLOWCODE_CONTAINER_MEMORY_BYTES or an explicit --memory-limit\n\
yellowcode: memory warning: no finite external envelope; fixed 1GiB fallback (UNF-class bases need --memory-limit >= 1792MiB, ERP-class about 3GiB)\n";
    let request = "memory_request id=1 status=0 reason=ok transport=sent wire_bytes=317\n";
    let fails = |s: String, finite: bool| {
        std::panic::catch_unwind(|| check_query_stderr(&s, finite)).is_err()
    };
    check_query_stderr(request, true);
    check_query_stderr(request, false);
    check_query_stderr(&format!("{fallback}{request}"), false);
    assert!(
        fails(format!("{fallback}{request}"), true),
        "finite: the request line only"
    );
    assert!(fails(fallback.to_string(), false), "no request line");
    assert!(
        fails(format!("{fallback}{request}{request}"), false),
        "two request lines"
    );
    assert!(
        fails(format!("{fallback}extra\n{request}"), false),
        "an extra line"
    );
    let explicit = fallback.replace("mode=fallback", "mode=explicit");
    assert!(
        fails(format!("{explicit}{request}"), false),
        "not the fallback"
    );
    let envelope = fallback.replace("envelope=none", "envelope=4294967296");
    assert!(
        fails(format!("{envelope}{request}"), false),
        "a finite envelope"
    );
    let one_warning: String = fallback.lines().take(2).map(|l| format!("{l}\n")).collect();
    assert!(
        fails(format!("{one_warning}{request}"), false),
        "one warning missing"
    );
}
