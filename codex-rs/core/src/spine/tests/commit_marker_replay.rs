use super::*;

#[test]
fn close_marker_does_not_replay_structural_close_without_live_toolcall_carrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-carrier-live", "1.1");
    let full_history = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize closed history");
    assert_eq!(full_history.len(), 3);

    let err = SpineRuntime::load_with_raw_live_and_event_limit(
        SpineStore::for_rollout(&rollout).expect("source store"),
        vec![true, false, false],
        None,
    )
    .expect_err("replay with stale close carrier raw must fail closed");
    assert!(
        err.to_string().contains("raw-backed event at token_seq"),
        "unexpected stale close carrier replay error: {err}"
    );
}
