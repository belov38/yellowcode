use std::process::Command;

#[test]
fn gui_rejects_missing_build_before_initializing_tauri() {
    for args in [
        vec!["--platform-build", "9.9.9.9999"],
        vec!["open", "--platform-build=9.9.9.9999"],
        vec!["missing-ar5t2.cf", "--platform-build", "9.9.9.9999"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
            .args(args)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{err}");
        assert!(
            err.contains("platform build 9.9.9.9999 is not installed"),
            "{err}"
        );
        assert!(!err.contains("already in use"), "{err}");
    }
}

#[test]
fn gui_build_arguments_are_parsed_without_becoming_source_paths() {
    for args in [
        vec!["--platform-build", "8.5.1.1529", "open", "x.cf"],
        vec!["open", "x.cf", "--platform-build=8.5.1.1529"],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_string).collect();
        let opts = onec_desktop::parse_args(&args).unwrap();
        assert_eq!(opts.source.as_deref(), Some("x.cf"));
    }
}
