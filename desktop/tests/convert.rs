use onec_desktop::convert::{Input, Jobs, source_info};
use std::{fs, path::Path};

fn input(source: &Path, output: &Path, to: &str) -> Input {
    serde_json::from_value(serde_json::json!({
        "source": source, "output": output, "to": to,
        "withoutUsers": false, "memoryLimit": null, "usersLayout": null
    }))
    .unwrap()
}

#[test]
fn detects_content_and_reports_only_available_targets() {
    let dir = tempfile::tempdir().unwrap();
    let dt = dir.path().join("выгрузка ёжий.bin");
    fs::write(&dt, b"1CIBDmpF").unwrap();
    let info = source_info(&dt).unwrap();
    assert_eq!(info.kind, "dt");
    assert_eq!(info.targets, ["1cd", "cf"]);
    let cf = cf_fixtures::fixture_dir("types").join("p.cf");
    assert!(source_info(&cf).unwrap().targets.is_empty());
}

#[test]
fn real_conversion_preserves_source_and_existing_output_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let base = yellowcode_testkit::base::create(dir.path());
    let source = base.join("1Cv8.1CD");
    let original = fs::read(&source).unwrap();
    let output = dir.path().join("конфигурация ёжий.cf");
    fs::write(&output, b"another owner").unwrap();
    let jobs = Jobs::default();
    jobs.start(input(&source, &output, "cf"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "error");
    assert_eq!(fs::read(&output).unwrap(), b"another owner");
    jobs.start(input(&source, &source, "cf"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "error");
    fs::remove_file(&output).unwrap();
    jobs.start(input(&source, &output, "cf"))
        .unwrap()
        .join()
        .unwrap();
    let status = jobs.status();
    assert_eq!(status.state, "done", "{:?}", status.error);
    assert_eq!(status.result.unwrap()["complete"], true);
    assert!(fs::metadata(output).unwrap().len() > 0);
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn unsupported_direction_and_missing_layout_do_not_create_output() {
    let dir = tempfile::tempdir().unwrap();
    let base = yellowcode_testkit::base::create(dir.path());
    let output = dir.path().join("result.dt");
    let jobs = Jobs::default();
    jobs.start(input(&base, &output, "dt"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "error");
    assert!(!output.exists());
    let cf = cf_fixtures::fixture_dir("types").join("p.cf");
    jobs.start(input(&cf, &output, "1cd"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "error");
    assert!(!output.exists());
}

#[test]
fn real_dt_restore_round_trip_and_long_unicode_output() {
    let dir = tempfile::tempdir().unwrap();
    let base = yellowcode_testkit::base::create_empty(dir.path());
    let jobs = Jobs::default();
    let dump = dir.path().join("выгрузка ёй.dt");
    let mut request = serde_json::to_value(serde_json::json!({
        "source": base, "output": dump, "to": "dt", "usersLayout": "users-spr11"
    }))
    .unwrap();
    jobs.start(serde_json::from_value(request.take()).unwrap())
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "done", "{:?}", jobs.status().error);
    let dump_bytes = fs::read(&dump).unwrap();
    let mut parent = dir.path().to_path_buf();
    for _ in 0..6 {
        parent = parent.join("длинный_каталог_ёжий_1234567890");
    }
    fs::create_dir_all(&parent).unwrap();
    let restored = parent.join("новая ёй.1CD");
    jobs.start(input(&dump, &restored, "1cd"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "done", "{:?}", jobs.status().error);
    assert_eq!(fs::read(&dump).unwrap(), dump_bytes);
    assert!(jobs.status().result.unwrap()["tables"].as_u64().unwrap() > 0);
    assert!(jobs.status().progress.is_some());
    let info = source_info(&restored).unwrap();
    assert_eq!(info.kind, "1cd");
    assert_eq!(
        serde_json::to_value(jobs.status()).unwrap()["targets"],
        serde_json::json!(["1cd", "cf"])
    );
}
