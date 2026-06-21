use super::*;

fn spine_call_with_args(name: &str, call_id: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    }
}

fn observe_spine_request_with_args(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    tool_name: &str,
    call_id: &str,
    arguments: &str,
) -> (ResponseItem, u64, usize) {
    let request = spine_call_with_args(tool_name, call_id, arguments);
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(runtime, raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record spine request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe spine request");
    (request, request_ordinal, request_context_index)
}

#[test]
fn control_request_raw_args_stage_pending_without_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request_with_args(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close",
        r#"{"memory":"  test node memory  "}"#,
    );

    assert!(
        runtime
            .has_close_like_control_request("close", &raw)
            .expect("classify raw close request")
    );
    runtime
        .ensure_pending_from_toolcall_request("close", &raw)
        .expect("stage close from raw request");
    assert!(matches!(
        runtime.pending_commit("close").expect("raw pending view"),
        Some(SpinePendingCommit::Close { memory, .. }) if memory == "test node memory"
    ));
}

#[test]
fn open_commit_uses_raw_request_without_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, req_raw, req_context) = observe_spine_request_with_args(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_OPEN,
        "open",
        r#"{"summary":"raw child"}"#,
    );
    let (_, resp_raw, resp_context) = observe_function_output(&mut runtime, &mut raw, "open");
    let completed = completed_toolcall(
        "open",
        vec![
            tool_req(req_raw, req_context),
            tool_resp(resp_raw, resp_context),
        ],
    );

    let parse_stack_before = runtime.parse_stack().clone();
    runtime
        .maybe_commit_output_with_toolcall_and_raw_items(
            "open",
            None,
            SpineTokenBaselines::default(),
            completed,
            &raw,
        )
        .expect("commit raw-request open");

    assert_ne!(runtime.parse_stack(), &parse_stack_before);
    assert!(
        runtime
            .pending_commit("open")
            .expect("raw request consumed")
            .is_none()
    );
}

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
