use super::*;

pub(super) fn spine_call_with_args(name: &str, call_id: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    }
}

pub(super) fn observe_spine_request_with_args(
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
fn raw_tool_request_blocks_variable_context_until_toolcall_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "tool-1");
    let output = function_output("tool-1");
    let mut raw = vec![Some(request.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record request raw");
    assert!(
        runtime.has_uncommitted_tool_request_in_raw_items(&raw),
        "raw request not yet reduced into a completed toolcall should block h(PS) publication"
    );

    raw.push(Some(output.clone()));
    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "tool-1".to_string(),
                request_call_ids: vec!["tool-1".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: 1,
                        context_index: 1,
                    },
                ],
            },
            &raw,
        )
        .expect("commit completed toolcall");

    assert!(
        !runtime.has_uncommitted_tool_request_in_raw_items(&raw),
        "committed toolcall request should no longer block h(PS) publication"
    );
    assert_eq!(
        runtime
            .materialize_variable_context(&raw)
            .expect("materialize after completed toolcall"),
        vec![request, output]
    );
}
