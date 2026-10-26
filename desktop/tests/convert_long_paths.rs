use onec_desktop::convert::{Input, Jobs, source_info};
use std::{fs, path::Path};

fn request(source: &Path, output: &Path, to: &str) -> Input {
    serde_json::from_value(serde_json::json!({
        "source": source, "output": output, "to": to,
        "usersLayout": if to == "dt" { Some("users-spr11") } else { None }
    }))
    .unwrap()
}

#[test]
fn restore_beyond_max_path_preserves_display_paths_and_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let base = yellowcode_testkit::base::create_empty(dir.path());
    let dump = dir.path().join("выгрузка ёй.dt");
    let jobs = Jobs::default();
    jobs.start(request(&base, &dump, "dt"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "done", "{:?}", jobs.status().error);
    let original = fs::read(&dump).unwrap();
    let mut parent = dir.path().to_path_buf();
    while parent.as_os_str().to_string_lossy().chars().count() <= 300 {
        parent = parent.join("длинный_каталог_ёжий_1234567890");
    }
    fs::create_dir_all(&parent).unwrap();
    let restored = parent.join("новая ёй.1CD");
    jobs.start(request(&dump, &restored, "1cd"))
        .unwrap()
        .join()
        .unwrap();
    let status = jobs.status();
    assert_eq!(status.state, "done", "{:?}", status.error);
    assert_eq!(status.source, dump);
    assert_eq!(status.output, restored);
    let result = status.result.unwrap();
    assert_eq!(result["output"], serde_json::to_value(&restored).unwrap());
    assert!(result["tables"].as_u64().unwrap() > 0);
    assert!(!source_info(&restored).unwrap().path.starts_with(r"\\?\"));
    assert_eq!(fs::read(&dump).unwrap(), original);
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    let published = fs::read(&restored).unwrap();
    jobs.start(request(&dump, &restored, "1cd"))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(jobs.status().state, "error");
    assert_eq!(fs::read(&restored).unwrap(), published);
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
}
