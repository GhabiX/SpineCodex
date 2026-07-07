use super::*;

#[test]
fn resume_ambiguous_partial_commit_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before ambiguous marker",
    );
    close_task(&mut runtime, &mut raw, "close-ambiguous-marker", "1.1");

    let mut duplicate = runtime
        .store
        .commit_markers()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    duplicate.op_id = "duplicate-close-marker".to_string();
    runtime
        .store
        .append_commit_marker(&duplicate)
        .expect("append duplicate marker");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("ambiguous duplicate commit markers must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous Spine commit marker at token_seq"),
        "unexpected resume error: {err}"
    );
}
