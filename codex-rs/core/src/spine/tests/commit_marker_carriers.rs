use super::*;

#[test]
fn close_commit_without_completed_toolcall_evidence_does_not_write_marker_or_clear_anchor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "live suffix before close");
    observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close-missing-carrier",
    );
    runtime
        .stage_close(
            "close-missing-carrier".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-missing-carrier")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };

    let before_events = event_log_debug(&runtime);
    let before_stack = runtime.parse_stack().clone();
    let before_tree = runtime.render_tree().expect("render before failure");
    let err = runtime
        .maybe_commit_output(
            "close-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
        )
        .expect_err("close must not commit without completed toolcall evidence");
    assert!(
        err.to_string()
            .contains("spine.close commit requires completed toolcall evidence"),
        "unexpected close error: {err}"
    );
    assert!(
        runtime
            .store
            .commit_markers()
            .expect("read markers")
            .is_empty(),
        "failed close must not publish a commit marker"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &before_stack,
        &before_tree,
        &before_events,
    );

    let (_output, output_raw, output_index) =
        observe_function_output(&mut runtime, &mut raw, "close-missing-carrier");
    runtime
        .maybe_commit_output_with_toolcall(
            "close-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "close-missing-carrier",
                vec![tool_req(1, 1), tool_resp(output_raw, output_index)],
            ),
        )
        .expect("retry with durable carrier commits")
        .expect("commit kind");

    let markers = runtime
        .store
        .commit_markers()
        .expect("read markers after retry");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::Close);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 2);
    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![event_tool_req(1, 1), event_tool_resp(output_raw, output_index as u64)]
    ));
}
