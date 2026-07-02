use super::*;

#[test]
fn ledger_cache_uses_sparse_max_seq_on_load_and_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 0,
            event: SpineLedgerEvent::Init { raw_start: 0 },
        })
        .expect("append sparse init");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 7,
            event: root_child_open_event("root"),
        })
        .expect("append sparse root open");

    let mut runtime = SpineRuntime::load_for_rollout(&rollout, 0)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(runtime.ledger.next_event_seq, 8);
    assert_eq!(
        runtime
            .build_tree_snapshot()
            .expect("snapshot")
            .snapshot_seq,
        8
    );

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("after sparse ledger"))
        .expect("append msg");

    assert_eq!(runtime.ledger.next_event_seq, 9);
    let events = logged_events(&runtime);
    assert!(matches!(
        events.last(),
        Some(LoggedSpineLedgerEvent {
            seq: 8,
            event: SpineLedgerEvent::Msg { raw_ordinal: 0, .. }
        })
    ));
}

#[test]
fn memory_body_write_preserves_store_level_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");

    let rel = store
        .write_memory_body("mem-feature-off", "memory body")
        .expect("write memory body");
    let body_path = store.root.join(&rel);

    assert_eq!(
        std::fs::read_to_string(&body_path).expect("read memory body"),
        "memory body"
    );
    assert!(
        !std::fs::metadata(&body_path)
            .expect("memory body metadata")
            .permissions()
            .readonly(),
        "plain sidecar memory writes must not be made readonly by the store layer"
    );

    let retry_rel = store
        .write_memory_body("mem-feature-off", "memory body")
        .expect("same-content retry");
    assert_eq!(retry_rel, rel);
    assert!(
        !std::fs::metadata(&body_path)
            .expect("memory body metadata after retry")
            .permissions()
            .readonly(),
        "same-content retry must not change store-level permissions"
    );
}

#[test]
fn canonical_rollout_sidecar_lives_outside_sessions_tree() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let rollout = codex_home
        .path()
        .join("sessions")
        .join("26")
        .join("07")
        .join("02")
        .join("rollout-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc.jsonl");
    let expected_root = codex_home
        .path()
        .join("spine-session")
        .join("26")
        .join("07")
        .join("02")
        .join("sidecar-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc");
    let legacy_locator = rollout
        .parent()
        .expect("rollout parent")
        .join("rollout-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc.spine.json");

    let store = SpineStore::create_for_rollout(&rollout).expect("create store");

    assert_eq!(store.root, expected_root);
    assert!(expected_root.is_dir());
    assert!(expected_root.join("locator.json").is_file());
    assert!(!legacy_locator.exists());
    assert!(SpineStore::has_for_rollout(&rollout).expect("has sidecar"));
    assert_eq!(
        SpineStore::for_rollout(&rollout).expect("load store").root,
        expected_root
    );
    assert!(
        !rollout.parent().expect("rollout parent").exists(),
        "creating the Spine sidecar must not create or populate the native rollout day dir"
    );
}

#[test]
fn canonical_rollout_store_apis_read_and_write_after_sidecar_move() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let rollout = canonical_rollout_path(
        codex_home.path(),
        "rollout-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc.jsonl",
    );
    let expected_root = codex_home
        .path()
        .join("spine-session")
        .join("26")
        .join("07")
        .join("02")
        .join("sidecar-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc");
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 0,
            event: SpineLedgerEvent::Init { raw_start: 0 },
        })
        .expect("append via API");

    let loaded = SpineStore::for_rollout(&rollout).expect("load through locator API");

    assert_eq!(loaded.root, expected_root);
    assert_eq!(loaded.events().expect("read events through API").len(), 1);
    assert_eq!(
        SpineStore::debug_request_dir_for_rollout(&rollout).expect("debug dir through API"),
        expected_root.join("debug_request")
    );
    assert!(
        !rollout.parent().expect("rollout parent").exists(),
        "store APIs must not require a sidecar sibling under sessions"
    );
}

#[test]
fn canonical_rollout_can_read_legacy_locator_for_resume_filtering() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let day_dir = codex_home
        .path()
        .join("sessions")
        .join("26")
        .join("07")
        .join("02");
    std::fs::create_dir_all(&day_dir).expect("create legacy day dir");
    let stem = "rollout-2026-07-02T15-04-05-12345678-1234-1234-1234-123456789abc";
    let rollout = day_dir.join(format!("{stem}.jsonl"));
    let legacy_root = day_dir.join(format!("spine-{stem}"));
    std::fs::create_dir_all(&legacy_root).expect("create legacy sidecar");
    std::fs::write(
        day_dir.join(format!("{stem}.spine.json")),
        format!("{{\n  \"version\": 1,\n  \"base\": \"spine-{stem}\"\n}}\n"),
    )
    .expect("write legacy locator");

    assert!(SpineStore::has_for_rollout(&rollout).expect("has legacy sidecar"));
    assert_eq!(
        SpineStore::for_rollout(&rollout)
            .expect("load legacy sidecar")
            .root,
        legacy_root
    );
}

#[test]
fn canonical_clone_publishes_final_sidecar_root() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let source_rollout = canonical_rollout_path(
        codex_home.path(),
        "rollout-2026-07-02T15-04-05-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa.jsonl",
    );
    let target_rollout = canonical_rollout_path(
        codex_home.path(),
        "rollout-2026-07-02T15-04-06-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb.jsonl",
    );
    let target_root = codex_home
        .path()
        .join("spine-session")
        .join("26")
        .join("07")
        .join("02")
        .join("sidecar-2026-07-02T15-04-06-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");

    SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 0)
        .expect("capture clone boundary")
        .expect("source sidecar exists");
    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[])
        .expect("clone sidecar");

    assert!(target_root.is_dir());
    assert!(target_root.join("locator.json").is_file());
    assert_eq!(
        SpineStore::for_rollout(&target_rollout)
            .expect("load target store")
            .root,
        target_root
    );
    assert!(
        !target_rollout
            .parent()
            .expect("target rollout parent")
            .join("rollout-2026-07-02T15-04-06-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb.spine.json")
            .exists()
    );
}

fn canonical_rollout_path(codex_home: &std::path::Path, file_name: &str) -> std::path::PathBuf {
    codex_home
        .join("sessions")
        .join("26")
        .join("07")
        .join("02")
        .join(file_name)
}
