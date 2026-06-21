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
