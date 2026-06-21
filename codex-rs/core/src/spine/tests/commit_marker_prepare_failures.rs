use super::*;

#[test]
fn close_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-close-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before close failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-store-fail");
    runtime
        .stage_close(
            "close-store-fail".to_string(),
            "memory that will fail before commit".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "close-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-close");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "close-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "close-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("close prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}

#[test]
fn next_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-next-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before next failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-store-fail");
    runtime
        .stage_next(
            "next-store-fail".to_string(),
            "sibling that must not be installed".to_string(),
            "memory that will fail before next commit".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "next-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "next-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-next");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "next-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "next-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("next prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}
