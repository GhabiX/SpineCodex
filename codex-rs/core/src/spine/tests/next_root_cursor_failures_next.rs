use super::*;

#[test]
fn next_at_root_cursor_fails_without_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let before = runtime.parse_stack().clone();
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-root");
    let err = runtime
        .stage_next(
            "next-root".to_string(),
            "must not open sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect_err("root cursor next should fail at stage time");
    assert!(
        err.to_string().contains("cannot close root epoch cursor 1"),
        "unexpected root next error: {err}"
    );
    assert!(
        runtime
            .pending_commit("next-root")
            .expect("pending lookup after rejected next")
            .is_none(),
        "rejected root next must not install pending close/open intent"
    );
    assert_eq!(runtime.parse_stack(), &before);
}
