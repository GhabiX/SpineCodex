use super::*;

#[test]
fn close_failure_does_not_mutate_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let err = runtime
        .maybe_commit_output("close", None)
        .expect_err("close without compact output must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires a validated source plan for memory assembly"),
        "unexpected close failure: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}
