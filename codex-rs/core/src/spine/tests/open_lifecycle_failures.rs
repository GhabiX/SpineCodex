use super::*;

#[test]
fn open_append_failure_does_not_publish_parse_stack_or_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let parse_stack_before = runtime.parse_stack().clone();
    let ledger_events_before = runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>();
    let next_event_seq_before = runtime.ledger.next_event_seq;

    let request = spine_call(SPINE_TOOL_OPEN, "open-fails");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open-fails".to_string(), "unpublished child".to_string())
        .expect("stage open");

    let blocked_root = dir.path().join("not-a-dir");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .maybe_commit_output("open-fails", None)
        .expect_err("open append should fail");
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime
            .ledger
            .events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>(),
        ledger_events_before
    );
    assert_eq!(runtime.ledger.next_event_seq, next_event_seq_before);
}
