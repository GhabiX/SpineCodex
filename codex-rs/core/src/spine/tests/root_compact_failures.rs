use super::*;

// Clone and fork sidecar behavior.

#[test]
fn native_compact_failure_leaves_parse_stack_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before failed compact"))
        .expect("observe context item");
    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();

    let err = runtime
        .root_compact(
            "   \n\t".to_string(),
            &[Some(text_item("before failed compact"))],
        )
        .expect_err("empty native compact body must fail closed");
    assert!(
        err.to_string()
            .contains("spine root compact memory body must not be empty"),
        "unexpected empty compact error: {err}"
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
}
