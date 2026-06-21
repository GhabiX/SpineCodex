use super::*;

#[test]
fn root_compact_prepare_store_failure_retains_retryable_compact_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(
        &mut runtime,
        &mut raw,
        "context before root compact failure",
    );
    append_msg(
        &mut runtime,
        &mut raw,
        "more context before root compact failure",
    );
    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-root-compact");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "root compact memory that will fail before commit".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("root compact prepare must fail while writing sidecar memory");
    assert_pending_compact_retry_state(&runtime, &before_events);
}

#[test]
fn prepare_root_compact_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "before staged root compact");
    append_msg(&mut runtime, &mut raw, "more staged root compact context");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared root compact");
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before prepared root compact");

    let prepared = runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "staged root compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("prepare root compact");

    assert_eq!(
        runtime
            .render_tree()
            .expect("render after prepared root compact"),
        before_tree,
        "prepared root compact must not install the reduced ParseStack before host publication"
    );
    let staged_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after prepared root compact");
    assert_eq!(
        staged_snapshot.active_node_id, before_snapshot.active_node_id,
        "prepared root compact must not advance the live active node"
    );

    runtime.install_prepared_root_compact(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared root compact");
    assert_ne!(
        after_snapshot.active_node_id, before_snapshot.active_node_id,
        "installing prepared root compact should advance the live ParseStack"
    );
}
