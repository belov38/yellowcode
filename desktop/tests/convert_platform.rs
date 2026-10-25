#[path = "../../crates/platform-catalog/tests/support/installed.rs"]
mod installed;
use std::process::Command;

#[test]
fn conversion_reports_active_platform() {
    if std::env::var_os("CVUX_PLATFORM_CHILD").is_some() {
        yellowcode_core::platform::select(installed::BUILD).unwrap();
        let cf = cf_fixtures::fixture_dir("types").join("p.cf");
        let snapshot =
            serde_json::to_value(onec_desktop::convert::Jobs::default().status()).unwrap();
        assert_eq!(snapshot["platformBuild"], installed::BUILD);
        assert_eq!(
            onec_desktop::convert::source_info(&cf)
                .unwrap()
                .platform_build,
            installed::BUILD
        );
    } else {
        let dir = tempfile::tempdir().unwrap();
        installed::write(dir.path(), installed::BUILD);
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "conversion_reports_active_platform",
                "--nocapture",
            ])
            .env("CVUX_PLATFORM_CHILD", "1")
            .env(installed::ENV, dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
