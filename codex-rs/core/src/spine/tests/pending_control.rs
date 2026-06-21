use super::*;

#[test]
fn control_tool_receipt_defers_spine_transition_until_tool_output_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let parse_stack_before_receipt = runtime.parse_stack().clone();
    let event_log_before_receipt = event_log_debug(&runtime);

    runtime
        .record_close_tool_receipt("close".to_string(), "test node memory".to_string())
        .expect("record close receipt");

    assert_eq!(runtime.parse_stack(), &parse_stack_before_receipt);
    assert_eq!(event_log_debug(&runtime), event_log_before_receipt);
    assert!(runtime.has_close_like_control_receipt("close"));
    assert!(matches!(
        runtime
            .pending_commit("close")
            .expect("receipt pending view"),
        Some(SpinePendingCommit::Close { .. })
    ));

    let memory_assembly = close_memory_assembly_from_source_plan(&runtime, &raw, "close", "1.1.1");
    observe_function_output(&mut runtime, &mut raw, "close");
    runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("commit receipt-backed close");

    assert!(!runtime.has_close_like_control_receipt("close"));
    assert!(
        runtime
            .pending_commit("close")
            .expect("receipt consumed")
            .is_none()
    );
    assert_ne!(runtime.parse_stack(), &parse_stack_before_receipt);
}

#[test]
fn duplicate_control_tool_receipt_preserves_original_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "first memory".to_string())
        .expect("record first receipt");

    let err = runtime
        .record_close_tool_receipt("close".to_string(), "second memory".to_string())
        .expect_err("duplicate receipt must fail");
    assert!(err.to_string().contains("duplicate Spine control receipt"));
    assert!(matches!(
        runtime.pending_commit("close").expect("receipt pending view"),
        Some(SpinePendingCommit::Close { memory, .. }) if memory == "first memory"
    ));
}

#[test]
fn abort_pending_clears_receipt_before_it_becomes_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "test node memory".to_string())
        .expect("record close receipt");

    assert!(runtime.has_close_like_control_receipt("close"));
    assert!(runtime.abort_pending("close"));
    assert!(!runtime.has_close_like_control_receipt("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(
        runtime
            .pending_commit("close")
            .expect("cleared receipt")
            .is_none()
    );
}

#[test]
fn abort_matching_pending_clears_control_call_without_durable_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before interrupted next");
    let request = spine_call(SPINE_TOOL_NEXT, "stale-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "stale-next".to_string(),
            "interrupted sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");

    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    assert!(runtime.control_call_ids.contains("stale-next"));
    assert!(matches!(
        runtime
            .pending_commit("stale-next")
            .expect("pending commit"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert!(runtime.abort_pending("stale-next"));
    assert!(
        runtime
            .pending_commit("stale-next")
            .expect("pending should be cleared")
            .is_none()
    );
    assert!(!runtime.control_call_ids.contains("stale-next"));
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);

    let next_request = spine_call(SPINE_TOOL_NEXT, "fresh-next");
    let next_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let next_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(next_request.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record fresh next request");
    runtime
        .observe_context_item(next_ordinal, next_context_index, &next_request)
        .expect("observe fresh next request");
    runtime
        .stage_next(
            "fresh-next".to_string(),
            "fresh sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("fresh transition should stage after abort");
}

#[test]
fn abort_non_matching_pending_keeps_transition_until_stale_abort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before close");
    let request = spine_call(SPINE_TOOL_CLOSE, "close");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");

    assert!(!runtime.abort_pending("other-call"));
    assert!(runtime.control_call_ids.contains("close"));
    assert!(matches!(
        runtime.pending_commit("close").expect("pending close"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert_eq!(runtime.abort_any_pending().as_deref(), Some("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(runtime.pending_commit("close").expect("cleared").is_none());
}

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

#[test]
fn prepare_close_commit_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(
        &mut runtime,
        &mut raw,
        "open-staged-close",
        "staged close child",
    );
    append_msg(&mut runtime, &mut raw, "child work before staged close");
    let (request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "staged-close");
    runtime
        .stage_close(
            "staged-close".to_string(),
            "staged close memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "staged-close", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "staged-close");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared commit");

    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "staged-close",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "staged-close",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect("prepare close commit")
        .expect("prepared close commit");
    assert!(matches!(prepared.kind(), SpineCommitKind::Close { .. }));
    let publication_plan = prepared
        .publication_plan()
        .expect("close commit should carry publication plan");
    assert_eq!(publication_plan.operation(), "spine.close");
    assert_eq!(publication_plan.suffix_start(), 0);
    assert_eq!(publication_plan.replacement_prefix().len(), 1);
    assert_eq!(
        publication_plan.preserve_host_history_from(),
        request_context
    );
    assert!(
        publication_plan.append_current_tool_response_if_missing(),
        "close publication should append current output when host has not recorded it"
    );
    assert_eq!(
        runtime.render_tree().expect("render after prepared commit"),
        before_tree,
        "prepared close commit must not install the reduced ParseStack before host publication"
    );
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before installing prepared commit");
    let before_nodes = snapshot_nodes_by_id(&before_snapshot);
    assert_ne!(
        before_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "live tree must not expose closed-node publication before install"
    );

    runtime
        .persist_prepared_commit_side_effects(&prepared)
        .expect("persist prepared close side effects");
    runtime.install_prepared_commit(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared commit");
    let after_nodes = snapshot_nodes_by_id(&after_snapshot);
    assert_eq!(
        after_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "installing prepared close commit should advance the live ParseStack"
    );
    assert_eq!(request, spine_call(SPINE_TOOL_CLOSE, "staged-close"));
}
