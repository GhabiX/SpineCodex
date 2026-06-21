use super::*;

#[test]
fn spine_error_classifies_missing_raw_coverage_as_sidecar_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    let raw = vec![Some(text_item("uncovered durable item"))];

    let err = runtime
        .validate_raw_coverage(&raw)
        .expect_err("missing durable raw coverage must fail closed");
    assert_eq!(err.class(), SpineErrorClass::SidecarCorruption);
    assert!(err.should_invalidate_runtime());
    assert!(
        err.to_string()
            .contains("spine sidecar is missing token coverage for raw ordinal 0"),
        "unexpected coverage error: {err}"
    );
    assert!(err.to_string().contains("token_seq="));
}

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
        .commit_markers_for_test()
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

#[test]
fn resume_rejects_missing_memory_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before missing memory",
    );
    close_task(&mut runtime, &mut raw, "close-missing-memory", "1.1");

    let marker = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let memory = marker
        .memory_refs
        .first()
        .expect("close marker should reference memory");
    std::fs::remove_file(runtime.store.root.join(&memory.body_path))
        .expect("remove committed memory body");

    SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("missing committed memory artifact must fail closed");
}
