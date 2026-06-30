use super::*;

#[test]
fn close_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-marker", "1.1");

    let markers = runtime.store.commit_markers().expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::Close);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 2);
    assert_eq!(markers[0].memory_refs.len(), 1);

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}
