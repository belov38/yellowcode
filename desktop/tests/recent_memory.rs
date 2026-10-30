use memory_budget::desktop::Facts;
use onec_desktop::recent::{MemorySetting, Recent};

fn facts() -> Facts {
    Facts {
        physical: Some(8 << 30),
        available: Some(4 << 30),
        external: None,
        ..Facts::default()
    }
}

#[test]
fn legacy_unicode_edit_touch_restart_preserves_selection() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("Первый йё.cf");
    let second = dir.path().join("Новый йё.cf");
    std::fs::write(&first, b"test").unwrap();
    std::fs::write(&second, b"test").unwrap();
    let file = dir.path().join("recent.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({"items": [first]})).unwrap(),
    )
    .unwrap();
    let mut recent = Recent::load_unchecked(&file);
    assert_eq!(recent.sources[0].memory, MemorySetting::Auto);
    recent
        .set_memory(
            Some(&first),
            &first,
            "База йё",
            MemorySetting::Limited { mib: 512 },
            facts(),
        )
        .unwrap();
    recent.touch(&first);
    recent.set(Some(&first), &second, "Новое имя йё").unwrap();
    recent.save(&file).unwrap();
    let reopened = Recent::load_unchecked(&file);
    assert_eq!(reopened.items.len(), 1);
    assert_eq!(reopened.items[0].name, "Новое имя йё");
    assert_eq!(
        reopened.items[0].memory,
        MemorySetting::Limited { mib: 512 }
    );
    assert_eq!(
        reopened.memory_policy(&second, facts()).unwrap().budget,
        512 << 20
    );
    assert_eq!(
        Recent::exists_now(&reopened.items)[0].memory,
        MemorySetting::Limited { mib: 512 }
    );
    assert!(
        reopened
            .memory_policy(
                &second,
                Facts {
                    physical: Some(512 << 20),
                    ..facts()
                }
            )
            .is_err()
    );
}

#[test]
fn old_named_records_default_to_auto() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    std::fs::write(&file, r#"{"items":[{"path":"named.cf","name":"йё"}]}"#).unwrap();
    assert_eq!(
        Recent::load_unchecked(&file).sources[0].memory,
        MemorySetting::Auto
    );
}

#[test]
fn invalid_budget_never_changes_memory_or_saved_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("йё.cf");
    std::fs::write(&path, b"test").unwrap();
    let file = dir.path().join("recent.json");
    let mut recent = Recent::default();
    recent
        .set_memory(None, &path, "йё", MemorySetting::Auto, facts())
        .unwrap();
    recent.save(&file).unwrap();
    assert!(recent.items.is_empty(), "registration is not an opening");
    let before = std::fs::read(&file).unwrap();
    assert!(
        recent
            .set_memory(
                Some(&path),
                &dir.path().join("missing.cf"),
                "changed",
                MemorySetting::Limited { mib: 512 },
                facts()
            )
            .is_err()
    );
    assert!(
        recent
            .set_memory(
                Some(&path),
                &path,
                "",
                MemorySetting::Limited { mib: 512 },
                facts()
            )
            .is_err()
    );
    assert_eq!(recent.sources[0].name, "йё");
    assert_eq!(recent.sources[0].memory, MemorySetting::Auto);
    for mib in [0, 255, 6000, u64::MAX] {
        assert!(
            recent
                .set_memory(
                    Some(&path),
                    &path,
                    "changed",
                    MemorySetting::Limited { mib },
                    facts()
                )
                .is_err()
        );
        assert_eq!(recent.sources[0].name, "йё");
        assert_eq!(recent.sources[0].memory, MemorySetting::Auto);
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }
    recent.save(&file).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn invalid_shapes_are_rejected_without_coercion() {
    for value in [
        r#"{}"#,
        r#"{"mode":"auto","mib":256}"#,
        r#"{"mode":"auto","mib":null}"#,
        r#"{"mode":"auto","extra":true}"#,
        r#"{"mode":"limited"}"#,
        r#"{"mode":"limited","mib":-1}"#,
        r#"{"mode":"limited","mib":256.5}"#,
        r#"{"mode":"limited","mib":"256"}"#,
        r#"{"mode":"limited","mib":18446744073709551616}"#,
        r#"{"mode":"limited","mib":256,"extra":true}"#,
    ] {
        assert!(
            serde_json::from_str::<MemorySetting>(value).is_err(),
            "{value}"
        );
    }
    assert_eq!(
        serde_json::to_value(MemorySetting::Auto).unwrap(),
        serde_json::json!({"mode":"auto"})
    );
    assert_eq!(
        serde_json::from_str::<MemorySetting>(r#"{"mode":"limited","mib":256}"#).unwrap(),
        MemorySetting::Limited { mib: 256 }
    );
}

#[test]
fn reset_to_auto_and_unsaved_source_use_auto() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("йё.cf");
    std::fs::write(&path, b"test").unwrap();
    let file = dir.path().join("recent.json");
    let mut recent = Recent::default();
    assert_eq!(
        recent.validated_memory_selection(&path, facts()).unwrap(),
        memory_budget::desktop::Selection::Unlimited
    );
    recent
        .set_memory(
            None,
            &path,
            "йё",
            MemorySetting::Limited { mib: 512 },
            facts(),
        )
        .unwrap();
    recent
        .set_memory(Some(&path), &path, "йё", MemorySetting::Auto, facts())
        .unwrap();
    recent.save(&file).unwrap();
    assert_eq!(
        Recent::load_unchecked(&file).sources[0].memory,
        MemorySetting::Auto
    );
}

#[test]
fn registry_budget_survives_more_than_ten_openings_and_non_mru_edit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let mut recent = Recent::default();
    let mut paths = Vec::new();
    for index in 0..14 {
        let path = dir.path().join(format!("База йё {index}.cf"));
        std::fs::write(&path, b"test").unwrap();
        let saved = recent
            .set_memory(
                None,
                &path,
                &format!("йё {index}"),
                MemorySetting::Limited { mib: 512 },
                facts(),
            )
            .unwrap();
        assert_eq!(saved, dunce::canonicalize(&path).unwrap());
        paths.push(saved);
    }
    assert_eq!(recent.sources.len(), 14);
    assert!(recent.items.is_empty());
    recent.save(&file).unwrap();
    assert_eq!(
        recent.memory_policy(&paths[0], facts()).unwrap().budget,
        512 << 20
    );
    for path in &paths {
        recent.touch(path);
    }
    recent.save(&file).unwrap();
    assert_eq!(recent.items.len(), 10);
    assert!(!recent.items.iter().any(|item| item.path == paths[0]));
    recent
        .set_memory(
            Some(&paths[0]),
            &paths[0],
            "Не в последних йё",
            MemorySetting::Limited { mib: 768 },
            facts(),
        )
        .unwrap();
    recent.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 14);
    assert_eq!(saved.items.len(), 10);
    assert!(!saved.items.iter().any(|item| item.path == paths[0]));
    assert_eq!(
        saved.memory_policy(&paths[0], facts()).unwrap().budget,
        768 << 20
    );
}

#[test]
fn stale_touch_and_rename_replay_preserve_newer_memory_in_both_lists() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let path = dir.path().join("йё.cf");
    std::fs::write(&path, b"test").unwrap();
    let mut original = Recent::default();
    original.set(None, &path, "Initial").unwrap();
    original.save(&file).unwrap();
    let mut stale_touch = Recent::load_unchecked(&file);
    let mut stale_rename = Recent::load_unchecked(&file);
    stale_touch.touch(&path);
    stale_rename.set(Some(&path), &path, "Renamed йё").unwrap();
    original
        .set_memory(
            Some(&path),
            &path,
            "Initial",
            MemorySetting::Limited { mib: 512 },
            facts(),
        )
        .unwrap();
    original.save(&file).unwrap();
    stale_touch.save(&file).unwrap();
    stale_rename.save(&file).unwrap();
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources[0].name, "Renamed йё");
    assert_eq!(saved.sources[0].memory, MemorySetting::Limited { mib: 512 });
    assert_eq!(saved.items[0].memory, saved.sources[0].memory);
    original.save(&file).unwrap();
    assert_eq!(
        Recent::load_unchecked(&file).sources[0].memory,
        MemorySetting::Limited { mib: 512 }
    );
}

#[test]
fn simultaneous_touch_and_memory_transactions_keep_the_explicit_selection() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recent.json");
    let path = dir.path().join("йё.cf");
    std::fs::write(&path, b"test").unwrap();
    let mut initial = Recent::default();
    initial.set(None, &path, "йё").unwrap();
    initial.save(&file).unwrap();
    let mut edit = Recent::load_unchecked(&file);
    let mut touch = Recent::load_unchecked(&file);
    edit.set_memory(
        Some(&path),
        &path,
        "йё",
        MemorySetting::Limited { mib: 768 },
        facts(),
    )
    .unwrap();
    touch.touch(&path);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        for mut snapshot in [edit, touch] {
            let file = &file;
            let barrier = barrier.clone();
            scope.spawn(move || {
                barrier.wait();
                snapshot.save(file).unwrap();
            });
        }
    });
    let saved = Recent::load_unchecked(&file);
    assert_eq!(saved.sources.len(), 1);
    assert_eq!(saved.items.len(), 1);
    assert_eq!(saved.sources[0].memory, MemorySetting::Limited { mib: 768 });
    assert_eq!(saved.items[0].memory, saved.sources[0].memory);
}
