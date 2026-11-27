use std::process::{Command, Output};

fn window(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yellowcode-desktop"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap()
}

#[test]
fn console_commands_and_unknown_options_are_usage_errors_with_the_hint() {
    for args in [
        vec!["config", "metadata", "tree", "x.cf"],
        vec!["open", "x.cf"],
        vec!["--bogus"],
        vec!["--help"],
        vec!["x.cf", "-bad"],
        vec!["--lang", "en", "-bad"],
    ] {
        let out = window(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(
            err.starts_with("yellowcode-desktop: ")
                && err.contains("; консольные команды — в yellowcode (yellowcode --help; в AppImage: <файл>.AppImage cli --help)\n"),
            "{args:?}: {err}"
        );
        assert!(
            err.contains("Usage: yellowcode-desktop [<source>]"),
            "{err}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn window_arguments_are_parsed() {
    let parse = |args: &[&str]| {
        onec_desktop::parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    for args in [
        vec!["some.cf"],
        vec!["some.cf", "--lang", "en", "--smoke"],
        vec!["--lang", "ru", "--names", "dump", "some.cf"],
    ] {
        assert_eq!(parse(&args).unwrap().source.as_deref(), Some("some.cf"));
    }
    let opts = parse(&["--names", "-dump", "source.cf", "--smoke"]).unwrap();
    assert_eq!(opts.names.as_deref(), Some("-dump"));
    assert!(opts.smoke);
    assert_eq!(parse(&["open"]).unwrap().source.as_deref(), Some("open"));
    assert!(parse(&["open", "x.cf"]).is_err());
}
