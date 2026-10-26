use onec_desktop::recent::{Origin, Recent};
use std::path::Path;

fn source(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"1CDBMSV8").unwrap();
    path
}

fn wait_success(child: &mut std::process::Child) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "worker failed: {status}");
            return;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("worker timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn stale_open_preserves_current_custom_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let path = source(dir.path(), "base.1cd");
    let mut first = Recent::default();
    first.set(None, &path, "Initial").unwrap();
    first.save(&file).unwrap();
    let mut stale = Recent::load_unchecked(&file);
    first.set(Some(&path), &path, "New custom name").unwrap();
    first.set_origin(&path, Origin::Yellowcode).unwrap();
    first.save(&file).unwrap();
    stale.touch(&path);
    stale.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources[0].name, "New custom name");
    assert_eq!(saved.items[0].origin, Origin::Yellowcode);
}

#[test]
fn independent_snapshots_preserve_additions_edits_and_removals() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let a = source(dir.path(), "a.1cd");
    let b = source(dir.path(), "b.1cd");
    let mut first = Recent::load_unchecked(&file);
    let mut second = Recent::load_unchecked(&file);
    first.set(None, &a, "Custom A").unwrap();
    first.touch(&a);
    first.save(&file).unwrap();
    second.set(None, &b, "Custom B").unwrap();
    second.touch(&b);
    second.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 2);
    assert_eq!(saved.items.len(), 2);
    first.remove(&a);
    first.save(&file).unwrap();
    second.set(Some(&b), &b, "Renamed B").unwrap();
    second.set_origin(&b, Origin::Yellowcode).unwrap();
    second.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 1);
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.sources[0].name, "Renamed B");
    assert_eq!(saved.items[0].origin, Origin::Yellowcode);
    assert!(a.exists());
    first.save(&file).unwrap();
    assert_eq!(Recent::load_unchecked(&file).sources[0].name, "Renamed B");
}

#[test]
fn registry_process_worker() {
    let Some(root) = std::env::var_os("V3N_REGISTRY_WORKER") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let id = std::env::var("V3N_REGISTRY_ID").unwrap();
    let file = root.join("recent.json");
    let mut recent = Recent::load_unchecked(&file);
    std::fs::write(root.join(format!("ready-{id}")), b"").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !root.join("go").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "worker barrier timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    recent.remove(&root.join(format!("old-{id}.1cd")));
    let path = root.join(format!("new-{id}.1cd"));
    recent.set(None, &path, &format!("Name {id}")).unwrap();
    recent.touch(&path);
    std::fs::write(root.join(format!("saving-{id}")), b"").unwrap();
    recent.save(&file).unwrap();
}

#[test]
fn simultaneous_processes_serialize_complete_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let mut initial = Recent::default();
    for id in 0..6 {
        let old = source(dir.path(), &format!("old-{id}.1cd"));
        source(dir.path(), &format!("new-{id}.1cd"));
        initial.set(None, &old, "Old").unwrap();
        initial.touch(&old);
    }
    initial.save(&file).unwrap();
    let mut children: Vec<_> = (0..6)
        .map(|id| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "registry_process_worker", "--nocapture"])
                .env("V3N_REGISTRY_WORKER", dir.path())
                .env("V3N_REGISTRY_ID", id.to_string())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !(0..6).all(|id| dir.path().join(format!("ready-{id}")).exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "snapshot barrier timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::write(dir.path().join("go"), b"").unwrap();
    for child in &mut children {
        wait_success(child);
    }
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 6);
    assert_eq!(saved.items.len(), 6);
    for id in 0..6 {
        assert!(saved.sources.iter().any(|s| s.name == format!("Name {id}")));
        assert!(dir.path().join(format!("old-{id}.1cd")).exists());
    }
    assert!(!dir.path().join("recent.json.tmp").exists());
}

#[test]
fn sidecar_lock_blocks_other_process_until_released() {
    let dir = tempfile::tempdir().unwrap();
    source(dir.path(), "new-0.1cd");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.path().join("recent.json.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "registry_process_worker", "--nocapture"])
        .env("V3N_REGISTRY_WORKER", dir.path())
        .env("V3N_REGISTRY_ID", "0")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(dir.path().join("go"), b"").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !dir.path().join("saving-0").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let blocked = child.try_wait().unwrap().is_none();
    drop(lock);
    wait_success(&mut child);
    assert!(blocked, "a save must wait for the cross-process lock");
    assert_eq!(
        Recent::load_unchecked(&dir.path().join("recent.json"))
            .sources
            .len(),
        1
    );
}

#[test]
fn failed_replace_retains_pending_changes_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let path = source(dir.path(), "base.1cd");
    let mut recent = Recent::default();
    recent.set(None, &path, "Retained").unwrap();
    std::fs::create_dir(&file).unwrap();
    assert!(recent.save(&file).is_err());
    std::fs::remove_dir(&file).unwrap();
    recent.save(&file).unwrap();
    assert_eq!(Recent::load_unchecked(&file).sources[0].name, "Retained");
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".recent-")
    }));
}

#[test]
fn direct_canonical_open_reconciles_migrated_named_alias() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = dir.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    #[cfg(windows)]
    let alias = {
        std::fs::create_dir(&alias).unwrap();
        alias.join("..").join("real")
    };
    let original = source(&alias, "base.1cd");
    let canonical = dunce::canonicalize(&original).unwrap();
    let file = dir.path().join("recent.json");
    std::fs::write(
        &file,
        serde_json::json!({"items":[{
            "path":original,"name":"My chosen йё name","kind":"infobase","origin":"yellowcode"
        }]})
        .to_string(),
    )
    .unwrap();
    let mut recent = Recent::load_unchecked(&file);
    recent.touch(&canonical);
    assert_eq!(recent.sources.len(), 1);
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].name, "My chosen йё name");
    assert_eq!(recent.items[0].origin, Origin::Yellowcode);
    assert_eq!(recent.items[0].path, canonical);
    recent.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 1);
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.sources[0].name, "My chosen йё name");
    assert_eq!(saved.sources[0].origin, Origin::Yellowcode);
}

#[test]
fn offline_migrated_alias_rename_survives_save_restart_and_retry() {
    for retry in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = dir.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        #[cfg(windows)]
        let alias = {
            std::fs::create_dir(&alias).unwrap();
            alias.join("..").join("real")
        };
        let original = source(&alias, "base.1cd");
        let file = dir.path().join("recent.json");
        std::fs::write(
            &file,
            serde_json::json!({"items":[{
                "path":original,"name":"Chosen name","kind":"infobase","origin":"yellowcode"
            }]})
            .to_string(),
        )
        .unwrap();
        std::fs::remove_file(&original).unwrap();
        let mut recent = Recent::load_unchecked(&file);
        recent
            .set(Some(&original), &original, "Offline йё")
            .unwrap();
        if retry {
            let before = std::fs::read(&file).unwrap();
            let mut oversized = recent.sources[0].clone();
            oversized.name = "x".repeat(2 << 20);
            recent.sources.push(oversized);
            assert!(recent.save(&file).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), before);
            recent.sources.pop();
        }
        recent.save(&file).unwrap();
        recent.save(&file).unwrap();
        let saved = Recent::load_unchecked(&file);
        assert_eq!(saved.sources.len(), 1);
        assert_eq!(saved.items.len(), 1);
        assert_eq!(saved.sources[0].name, "Offline йё");
        assert_eq!(saved.items[0].name, "Offline йё");
        assert_eq!(saved.items[0].origin, Origin::Yellowcode);
        assert_eq!(saved.items[0].kind, onec_desktop::recent::Kind::Infobase);
    }
}
