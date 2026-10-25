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
    let recent = Recent::load_unchecked(&file);
    let names: Vec<_> = recent.items.iter().map(|item| item.name.as_str()).collect();
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
