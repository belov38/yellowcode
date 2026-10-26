use onec_desktop::recent::Recent;

#[test]
fn named_sources_survive_open_edit_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let source = dir.path().join("example.cf");
    std::fs::write(&source, b"fixture").unwrap();
    std::fs::write(&file, serde_json::json!({"items": [source]}).to_string()).unwrap();
    let mut recent = Recent::load(&file);
    assert_eq!(recent.items[0].name, "example");
    recent.set(Some(&source), &source, "БСП / SSL").unwrap();
    recent.touch(&source);
    recent.save(&file).unwrap();
    let mut recent = Recent::load(&file);
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].name, "БСП / SSL");
    recent.set(Some(&source), &source, "Renamed").unwrap();
    assert_eq!(recent.items[0].name, "Renamed");
    assert!(recent.set(Some(&source), &source, "  ").is_err());
    assert!(
        recent
            .set(Some(&source), &dir.path().join("missing.cf"), "Missing")
            .is_err()
    );
    assert_eq!(recent.items[0].name, "Renamed");
    recent.remove(&source);
    recent.save(&file).unwrap();
    assert!(Recent::load(&file).items.is_empty());
    assert!(source.exists());
}

#[test]
fn migrated_default_names_are_unique_and_keep_parent_context() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let entries = serde_json::json!({"items": [
        "/one/ssl32/bsp32.cf",
        "/two/ssl32/bsp32.cf",
        {"path": "/three/ssl31/bsp32.cf", "name": "bsp32"},
        "/one/qr-main/1Cv8.1CD",
        "/two/qr-copy/1Cv8.1CD",
        "/same/a.cf",
        "/same/a.cfe",
        {"path": "/custom/c.cf", "name": "My source"}
    ]});
    std::fs::write(&file, entries.to_string()).unwrap();
    let mut recent = Recent::load_unchecked(&file);
    let names: Vec<_> = recent.items.iter().map(|item| item.name.clone()).collect();
    assert_eq!(names[0], "bsp32 (ssl32)");
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(names[1], format!("bsp32 (two{sep}ssl32)"));
    assert_eq!(names[2], "bsp32 (ssl31)");
    assert_eq!(names[3], "1Cv8 (qr-main)");
    assert_eq!(names[4], "1Cv8 (qr-copy)");
    assert_eq!(names[7], "My source");
    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
    recent.save(&file).unwrap();
    let restarted = Recent::load_unchecked(&file);
    assert_eq!(
        names,
        restarted
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restarted.items[0].path,
        std::path::Path::new("/one/ssl32/bsp32.cf")
    );
}

#[cfg(windows)]
#[test]
fn listed_paths_drop_the_verbatim_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let source = dir.path().join("example.cf");
    std::fs::write(&source, b"fixture").unwrap();
    let mut recent = Recent::load_unchecked(&file);
    recent.touch(&source);
    let shown = recent.items[0].path.to_string_lossy().into_owned();
    assert!(!shown.starts_with(r"\\?\"), "{shown}");
    std::fs::write(
        &file,
        serde_json::json!({"items": [format!(r"\\?\{}", source.display())]}).to_string(),
    )
    .unwrap();
    let saved = Recent::load_unchecked(&file);
    assert!(!saved.items[0].path.to_string_lossy().starts_with(r"\\?\"));
}

#[test]
fn registry_survives_mru_eviction_and_registration_is_not_an_open() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let source = dir.path().join("Учёт йё.1cd");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let mut r = Recent::default();
    r.set(None, &source, "Учёт йё").unwrap();
    assert!(
        r.items.is_empty(),
        "registration must not add a history entry"
    );
    assert_eq!(r.sources.len(), 1);
    r.set_origin(&source, onec_desktop::recent::Origin::Yellowcode)
        .unwrap();
    r.touch(&source);
    for i in 0..12 {
        r.touch(&dir.path().join(format!("missing-{i}.cf")));
    }
    assert_eq!(r.items.len(), 10);
    assert_eq!(r.sources.len(), 13);
    r.save(&file).unwrap();
    let mut r = Recent::load_unchecked(&file);
    assert_eq!(r.sources.len(), 13);
    r.touch(&source);
    assert_eq!(r.items[0].name, "Учёт йё");
    assert_eq!(r.items[0].origin, onec_desktop::recent::Origin::Yellowcode);
    r.remove(&source);
    assert_eq!(r.sources.len(), 12);
    assert!(source.exists());
}

#[test]
fn legacy_migration_keeps_missing_sources_without_guessing_the_kind() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let paths: Vec<_> = (0..12)
        .map(|i| dir.path().join(format!("{i}.1cd")))
        .collect();
    std::fs::write(&file, serde_json::json!({"items": paths}).to_string()).unwrap();
    let mut r = Recent::load_unchecked(&file);
    assert_eq!(r.sources.len(), 12);
    assert_eq!(r.items.len(), 10);
    assert!(
        r.sources
            .iter()
            .all(|s| s.kind == onec_desktop::recent::Kind::Unknown)
    );
    r.save(&file).unwrap();
    assert_eq!(Recent::load_unchecked(&file).sources.len(), 12);
}

#[test]
fn kind_is_detected_by_content_and_origin_is_explicit() {
    use onec_desktop::recent::{Kind, Origin};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.cf");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let mut r = Recent::default();
    r.set(None, &source, "Native").unwrap();
    let checked = Recent::exists_now(&r.sources);
    assert_eq!(checked[0].kind, Kind::Infobase);
    assert_eq!(checked[0].origin, Origin::OneC);
    r.set_origin(&source, Origin::Yellowcode).unwrap();
    r.touch(&source);
    assert_eq!(r.items[0].origin, Origin::Yellowcode);
    let replacement = dir.path().join("new.1cd");
    std::fs::write(&replacement, b"1CDBMSV8").unwrap();
    r.set(Some(&source), &replacement, "New path").unwrap();
    assert_eq!(r.sources.len(), 1);
    assert_eq!(r.sources[0].origin, Origin::Yellowcode);
    assert_eq!(r.items[0].name, "New path");
    std::fs::remove_file(&replacement).unwrap();
    assert!(!Recent::exists_now(&r.sources)[0].exists);
    assert_eq!(r.sources[0].origin, Origin::Yellowcode);
    let invalid = dir.path().join("invalid.1cd");
    std::fs::write(&invalid, b"not an infobase").unwrap();
    r.set(None, &invalid, "Invalid").unwrap();
    assert!(r.set_origin(&invalid, Origin::Yellowcode).is_err());
}

#[test]
fn oversized_registry_save_keeps_the_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let source = dir.path().join("base.1cd");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let mut r = Recent::default();
    r.set(None, &source, "Base").unwrap();
    r.save(&file).unwrap();
    let before = std::fs::read(&file).unwrap();
    let mut large = r.sources[0].clone();
    large.name = "x".repeat(2 << 20);
    r.sources.push(large);
    assert!(r.save(&file).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn unavailable_registration_keeps_its_kind_and_can_be_renamed() {
    use onec_desktop::recent::{Kind, Origin};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("base.1cd");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let mut r = Recent::default();
    r.set(None, &source, "Base").unwrap();
    r.set_origin(&source, Origin::Yellowcode).unwrap();
    std::fs::write(&source, b"unreadable source").unwrap();
    let checked = Recent::exists_now(&r.sources);
    assert!(
        !checked[0].exists,
        "a source which cannot be read must be unavailable"
    );
    assert_eq!(checked[0].kind, Kind::Infobase);
    assert_eq!(checked[0].origin, Origin::Yellowcode);
    std::fs::remove_file(&source).unwrap();
    r.set(Some(&source), &source, "Renamed offline").unwrap();
    r.set_origin(&source, Origin::Yellowcode).unwrap();
    assert_eq!(r.sources[0].name, "Renamed offline");
    assert_eq!(r.sources[0].kind, Kind::Infobase);
    assert_eq!(r.sources[0].origin, Origin::Yellowcode);
}

#[cfg(unix)]
#[test]
fn editing_a_migrated_alias_replaces_the_legacy_history_path() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let source = alias.join("base.1cd");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let file = dir.path().join("recent.json");
    std::fs::write(&file, serde_json::json!({"items":[source]}).to_string()).unwrap();
    let mut r = Recent::load_unchecked(&file);
    let saved = r.set(Some(&source), &source, "Renamed").unwrap();
    assert_eq!(saved, dunce::canonicalize(&source).unwrap());
    r.touch(&source);
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.sources.len(), 1);
    assert_eq!(r.items[0].name, "Renamed");
}

#[cfg(unix)]
#[test]
fn missing_source_under_alias_keeps_registration_identity() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let source = alias.join("base.1cd");
    std::fs::write(&source, b"1CDBMSV8").unwrap();
    let mut r = Recent::default();
    let saved = r.set(None, &source, "Initial").unwrap();
    std::fs::remove_file(&source).unwrap();
    assert_eq!(r.set(Some(&source), &source, "Offline").unwrap(), saved);
    assert_eq!(r.sources.len(), 1);
    assert_eq!(r.sources[0].name, "Offline");
}
