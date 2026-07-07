use super::*;

#[test]
fn root_compact_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root visible work before compact");
    runtime
        .root_compact("root compact marker body".to_string(), &raw)
        .expect("root compact");

    let markers = runtime.store.commit_markers().expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::RootCompact);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 1);
    assert!(markers[0].raw_live_hash.is_some());

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("RootCompact ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for RootCompact ledger event"),
        "unexpected resume error: {err}"
    );
}
