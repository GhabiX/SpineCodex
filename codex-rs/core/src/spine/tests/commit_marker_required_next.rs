use super::*;

#[test]
fn next_commit_marker_covers_close_then_open_without_next_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before next");
    next_task(&mut runtime, &mut raw, "next-marker", "1.1", "next sibling");

    let markers = runtime.store.commit_markers().expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::CloseThenOpen);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 3);
    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::ToolCall { .. },
        ]
    ));

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close+Open ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}
