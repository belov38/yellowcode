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

#[test]
fn gui_memory_values_are_not_platform_options() {
    for args in [
        vec!["--memory-limit", "--platform-build"],
        vec![
            "open",
            "--memory-limit",
            "--platform-build",
            "--platform-build",
            "8.5.1.1529",
        ],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_string).collect();
        let opts = onec_desktop::parse_args(&args).unwrap();
        assert_eq!(opts.memory_limit.as_deref(), Some("--platform-build"));
        assert!(opts.source.is_none());
        assert!(yellowcode_core::viewer::source::OpenMemory::resolve(opts.memory_limit).is_err());
    }
}

#[test]
fn gui_memory_and_build_options_preserve_each_other() {
    for args in [
        vec![
            "--platform-build",
            "8.5.1.1529",
            "open",
            "йё.cf",
            "--memory-limit",
            "1GiB",
        ],
        vec![
            "open",
            "йё.cf",
            "--memory-limit",
            "1GiB",
            "--platform-build",
            "8.5.1.1529",
        ],
        vec![
            "open",
            "йё.cf",
            "--names",
            "--platform-build",
            "--memory-limit",
            "1GiB",
            "--platform-build",
            "8.5.1.1529",
        ],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_string).collect();
        let opts = onec_desktop::parse_args(&args).unwrap();
        assert_eq!(opts.source.as_deref(), Some("йё.cf"));
        assert_eq!(opts.platform_build.as_deref(), Some("8.5.1.1529"));
        assert_eq!(opts.memory_limit.as_deref(), Some("1GiB"));
    }
    for args in [
        vec!["--memory-limit"],
        vec![
            "--memory-limit",
            "1GiB",
            "--platform-build",
            "8.5.1.1529",
            "--platform-build",
            "8.5.1.1529",
        ],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_string).collect();
        assert!(onec_desktop::parse_args(&args).is_err());
    }
}
