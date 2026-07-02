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
