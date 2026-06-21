use super::*;

#[test]
fn try_commit_internal_failure_does_not_silently_abort_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work to compact");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let mem_path = runtime.store.mem_path();
    std::fs::create_dir_all(&mem_path).expect("block mem ledger append with directory");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1",
                suffix_start..raw.len(),
            )),
        )
        .expect_err("append_mem failure should fail commit");
    assert!(
        !err.to_string().is_empty(),
        "expected append_mem failure to surface"
    );
    assert!(matches!(
        runtime.pending_commit("close").expect("pending retained"),
        Some(SpinePendingCommit::Close { .. })
    ));
    assert!(
        runtime
            .stage_next(
                "new-next".to_string(),
                "blocked sibling".to_string(),
                "test node memory".to_string(),
            )
            .expect_err("pending must still block new transition")
            .to_string()
            .contains("another spine transition is already pending")
    );
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
}
