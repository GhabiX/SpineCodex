use super::*;

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

#[test]
fn root_compact_publish_length_mismatch_does_not_install_live_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "before root compact host publish");
    append_msg(
        &mut runtime,
        &mut raw,
        "more root compact host publish context",
    );
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before prepared root compact");

    let mut state = SpineSessionState::new();
    state
        .set_replayed(
            u64::try_from(raw.len()).expect("raw len fits u64"),
            Some(runtime),
        )
        .expect("install runtime into session state");
    let prepared = state
        .prepare_root_compact_commit_with_checkpoint(
            &rollout,
            "prepared root compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("prepare root compact through session");
    let wrong_published_len = prepared.materialized().len() + 1;

    let err = state
        .apply_root_compact_after_history_publish(prepared, wrong_published_len)
        .expect_err("mismatched host publication length should fail before install");
    assert!(
        err.to_string()
            .contains("does not match materialized history length"),
        "unexpected publish length mismatch error: {err}"
    );
    let after_snapshot = state
        .runtime()
        .expect("runtime remains valid after pre-install validation failure")
        .build_tree_snapshot()
        .expect("snapshot after failed publish length validation");
    assert_eq!(
        after_snapshot.active_node_id, before_snapshot.active_node_id,
        "failed host publication length validation must not install the prepared root compact PS"
    );
}
