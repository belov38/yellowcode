#[path = "../../crates/platform-catalog/tests/support/installed.rs"]
mod installed;
use std::{fs, path::PathBuf, process::Command};

#[test]
fn package_payload_and_relocated_cli_use_the_native_resource_directory() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("Жёлтый код");
    let (exe_dir, resources) = if cfg!(target_os = "macos") {
        (
            base.join("Yellowcode.app").join("Contents").join("MacOS"),
            base.join("Yellowcode.app")
                .join("Contents")
                .join("Resources"),
        )
    } else if cfg!(target_os = "linux") {
        (
            base.join("usr").join("bin"),
            base.join("usr").join("lib").join("Yellowcode"),
        )
    } else {
        (base.clone(), base.clone())
    };
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let mappings = config["bundle"]["resources"].as_object().unwrap();
    let platform = resources.join("resources").join("platform");
    let mut count = 0;
    for (source, target) in mappings {
        let target = target.as_str().unwrap();
        if !target.starts_with("resources/platform/") {
            continue;
        }
        let dest = resources.join(target);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(source),
            &dest,
        )
        .unwrap();
        count += 1;
    }
    assert_eq!(
        count, 6,
        "both build triplets must ship; extraction provenance stays in the source tree"
    );
    let shipped = platform.join("8.5.1.1529");
    assert_eq!(
        fs::read(shipped.join("catalog.bin")).unwrap(),
        installed::CATALOG
    );
    assert_eq!(
        fs::read_to_string(shipped.join("messages.tsv")).unwrap(),
        installed::MESSAGES
    );
    assert_eq!(
        fs::read_to_string(shipped.join("categories.tsv")).unwrap(),
        installed::CATEGORIES
    );
    assert!(
        !platform
            .join("8.3.27.1859")
            .join("provenance.json")
            .exists()
    );
    installed::write(&platform, installed::BUILD);
    fs::create_dir_all(&exe_dir).unwrap();
    let exe = exe_dir.join(if cfg!(windows) {
        "yellowcode.exe"
    } else {
        "yellowcode"
    });
    fs::copy(env!("CARGO_BIN_EXE_yellowcode"), &exe).unwrap();
    let fixture = cf_fixtures::fixture_dir("types").join("p.cf");
    let run = |build: &str| {
        Command::new(&exe)
            .current_dir(temp.path())
            .args(["--platform-build", build, "metadata", "tree"])
            .arg(&fixture)
            .env_remove(installed::ENV)
            .output()
            .unwrap()
    };
    let default = run("8.5.1.1529");
    let files = run(installed::BUILD);
    let actual_8327 = run("8.3.27.1859");
    for out in [&default, &files, &actual_8327] {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(!files.stdout.is_empty());
    assert_eq!(actual_8327.stdout, default.stdout);
    assert_eq!(files.stdout, default.stdout);
    assert_eq!(files.stderr, default.stderr);
}

#[test]
fn gui_reports_corrupt_resource_before_tauri_or_source_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let dir = installed::write(temp.path(), installed::BUILD);
    fs::remove_file(dir.join("messages.tsv")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .args(["--platform-build", installed::BUILD, "open"])
        .env(installed::ENV, temp.path())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{error}");
    assert!(error.contains("broken platform resource"), "{error}");
    assert!(error.contains("messages.tsv"), "{error}");
}
