use super::pending_control_raw_requests::observe_spine_request_with_args;
use super::*;
use codex_protocol::models::FunctionCallOutputBody;

fn observe_failed_output(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
) -> (ResponseItem, u64, usize) {
    let output = failed_function_output_text(
        call_id,
        "SPINE_TOOL_USE_FAILED: failed to parse function arguments. No Spine control action was applied. Retry with valid Spine tool arguments.",
    );
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let context_index = current_context_len(runtime, raw)
        .checked_add(1)
        .expect("output context index fits usize");
    raw.push(Some(output.clone()));
    runtime.observe_raw_items(1).expect("record failed output");
    runtime
        .observe_context_item(raw_ordinal, context_index, &output)
        .expect("observe failed output");
    (output, raw_ordinal, context_index)
}

fn assert_single_ordinary_toolcall(
    runtime: &SpineRuntime,
    request_raw: u64,
    request_context: usize,
    response_raw: u64,
    response_context: usize,
) {
    assert!(
        matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
        ] if root.summary == "root" && matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ] if segments == &vec![
                tool_req(request_raw, request_context),
                tool_resp(response_raw, response_context),
            ]
        )),
        "unexpected parse stack: {:#?}",
        runtime.parse_stack()
    );
}

#[test]
fn failed_spine_open_args_then_retry_records_failed_toolcall_as_ordinary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "before failed open");
    let (failed_request, failed_req_raw, failed_req_ctx) = observe_spine_request_with_args(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_OPEN,
        "bad-open",
        r#"{"summary":"child","memory":"wrong field"}"#,
    );
    let (_, failed_resp_raw, failed_resp_ctx) =
        observe_failed_output(&mut runtime, &mut raw, "bad-open");
    assert_eq!(failed_req_ctx, 1);
    assert_eq!(failed_resp_ctx, 2);
    let ResponseItem::FunctionCall {
        arguments: failed_arguments,
        ..
    } = &failed_request
    else {
        panic!("expected failed open request");
    };
    assert!(failed_arguments.contains(r#""memory""#));
    runtime
        .observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &[("bad-open".to_string(), failed_resp_raw, failed_resp_ctx)],
            &raw,
        )
        .expect("failed open output should be observed as ordinary");

    append_msg(&mut runtime, &mut raw, "retry now");
    assert_eq!(current_context_len(&runtime, &raw), 4);
    let (_, retry_req_raw, retry_req_ctx) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "retry-open");
    runtime
        .stage_open("retry-open".to_string(), "child".to_string())
        .expect("retry open stages");
    let (_, retry_resp_raw, retry_resp_ctx) =
        observe_function_output(&mut runtime, &mut raw, "retry-open");
    runtime
        .maybe_commit_output("retry-open", None)
        .expect("retry open commits");

    assert!(
        matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(child_nodes),
        ] if root.summary == "root" && matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::MsgAsLeafNode { .. },
                SpineTreeNode::ToolCallAsLeafNode { segments: failed_segments },
                SpineTreeNode::MsgAsLeafNode { .. },
            ] if failed_segments == &vec![
                tool_req(failed_req_raw, failed_req_ctx),
                tool_resp(failed_resp_raw, failed_resp_ctx),
            ]
        ) && matches!(
            child_nodes.as_slice(),
            [
                SpineTreeNode::ToolCallAsLeafNode { segments: retry_segments },
            ] if retry_segments == &vec![
                tool_req(retry_req_raw, retry_req_ctx),
                tool_resp(retry_resp_raw, retry_resp_ctx),
            ]
        )
        ),
        "unexpected parse stack: {:#?}",
        runtime.parse_stack()
    );
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw)
            .expect("failed open and retry materialize"),
        vec![
            anchored_text_item(1, "before failed open"),
            raw[1].clone().expect("failed request"),
            raw[2].clone().expect("failed response"),
            anchored_text_item(2, "retry now"),
            raw[4].clone().expect("retry request"),
            raw[5].clone().expect("retry response"),
        ]
    );
}

#[test]
fn failed_spine_tool_outputs_are_not_skipped_for_all_spine_tools() {
    for (tool_name, call_id) in [
        (SPINE_TOOL_OPEN, "failed-open"),
        (SPINE_TOOL_CLOSE, "failed-close"),
        (SPINE_TOOL_NEXT, "failed-next"),
        (SPINE_TOOL_TREE, "failed-tree"),
        (SPINE_TOOL_TRIM, "failed-trim"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut raw = Vec::new();
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        let (_, request_raw, request_context) =
            observe_spine_request(&mut runtime, &mut raw, tool_name, call_id);
        let (_, response_raw, response_context) =
            observe_failed_output(&mut runtime, &mut raw, call_id);
        runtime
            .observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
                &[(call_id.to_string(), response_raw, response_context)],
                &raw,
            )
            .expect("failed Spine tool output should be ordinary");

        assert_single_ordinary_toolcall(
            &runtime,
            request_raw,
            request_context,
            response_raw,
            response_context,
        );
    }
}

#[test]
fn failed_close_next_trim_and_tree_are_ordinary_toolcalls_without_control_tokens() {
    for (tool_name, call_id, bad_args) in [
        (SPINE_TOOL_CLOSE, "bad-close", r#"{"memory":""}"#),
        (SPINE_TOOL_NEXT, "bad-next", r#"{"summary":"next"}"#),
        (
            SPINE_TOOL_TRIM,
            "bad-trim",
            r#"{"trim_id":"missing","op":"bad"}"#,
        ),
        (SPINE_TOOL_TREE, "bad-tree", r#"{}"#),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut raw = Vec::new();
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        let (_, request_raw, request_context) =
            observe_spine_request_with_args(&mut runtime, &mut raw, tool_name, call_id, bad_args);
        let (failed_output, response_raw, response_context) =
            observe_failed_output(&mut runtime, &mut raw, call_id);
        runtime
            .observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
                &[(call_id.to_string(), response_raw, response_context)],
                &raw,
            )
            .expect("failed Spine tool output should be ordinary");

        let ResponseItem::FunctionCallOutput { output, .. } = failed_output else {
            panic!("expected failed function output for {tool_name}");
        };
        assert_eq!(output.success, Some(false));
        assert_single_ordinary_toolcall(
            &runtime,
            request_raw,
            request_context,
            response_raw,
            response_context,
        );
        assert!(
            runtime
                .pending_commit(call_id)
                .expect("pending query")
                .is_none(),
            "failed {tool_name} should not leave pending transition"
        );
        assert!(
            !runtime.control_call_ids.contains(call_id),
            "failed {tool_name} should clear control classification"
        );
    }
}

#[test]
fn failed_pending_control_is_aborted_and_recorded_as_ordinary_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-fails");
    runtime
        .stage_close("close-fails".to_string(), "test node memory".to_string())
        .expect("stage close");
    assert!(runtime.control_call_ids.contains("close-fails"));
    assert!(matches!(
        runtime
            .pending_commit("close-fails")
            .expect("pending close before failure"),
        Some(SpinePendingCommit::Close { .. })
    ));
    let (_, response_raw, response_context) =
        observe_failed_output(&mut runtime, &mut raw, "close-fails");

    let (aborted_pending, trim_body_updates) = runtime
        .commit_completed_toolcall_as_ordinary_with_raw_items(
            "close-fails",
            CompletedToolCall {
                call_id: "close-fails".to_string(),
                request_call_ids: vec!["close-fails".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: request_raw,
                        context_index: request_context,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: response_raw,
                        context_index: response_context,
                    },
                ],
            },
            &raw,
        )
        .expect("failed pending close should become ordinary");

    assert!(aborted_pending);
    assert!(
        trim_body_updates.is_empty(),
        "failed close output should not create trim body updates"
    );
    assert!(
        runtime
            .pending_commit("close-fails")
            .expect("pending close after failure")
            .is_none()
    );
    assert!(!runtime.control_call_ids.contains("close-fails"));
    assert_single_ordinary_toolcall(
        &runtime,
        request_raw,
        request_context,
        response_raw,
        response_context,
    );
}

#[test]
fn aborted_pending_control_is_recorded_as_ordinary_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-aborted");
    runtime
        .stage_close("close-aborted".to_string(), "test node memory".to_string())
        .expect("stage close");
    assert!(runtime.control_call_ids.contains("close-aborted"));

    let aborted_output = failed_function_output_text("close-aborted", "aborted by user after 1.0s");
    let ResponseItem::FunctionCallOutput { output, .. } = &aborted_output else {
        panic!("expected aborted function output");
    };
    assert_eq!(
        output.success,
        Some(false),
        "aborted durable control requests must be failure outputs so lexer treats them as ordinary"
    );
    let (_, response_raw, response_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, aborted_output, 1);

    let (aborted_pending, trim_body_updates) = runtime
        .commit_completed_toolcall_as_ordinary_with_raw_items(
            "close-aborted",
            CompletedToolCall {
                call_id: "close-aborted".to_string(),
                request_call_ids: vec!["close-aborted".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: request_raw,
                        context_index: request_context,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: response_raw,
                        context_index: response_context,
                    },
                ],
            },
            &raw,
        )
        .expect("aborted pending close should become ordinary");

    assert!(aborted_pending);
    assert!(
        trim_body_updates.is_empty(),
        "aborted close output should not create trim body updates"
    );
    assert!(
        runtime
            .pending_commit("close-aborted")
            .expect("pending close after abort")
            .is_none()
    );
    assert!(!runtime.control_call_ids.contains("close-aborted"));
    assert_single_ordinary_toolcall(
        &runtime,
        request_raw,
        request_context,
        response_raw,
        response_context,
    );
}

#[test]
fn conflicting_spine_controls_are_one_ordinary_toolcall_with_retry_outputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, open_raw, open_context) = observe_item_at_context_index(
        &mut runtime,
        &mut raw,
        spine_call(SPINE_TOOL_OPEN, "open-conflict"),
        0,
    );
    let (_, close_raw, close_context) = observe_item_at_context_index(
        &mut runtime,
        &mut raw,
        spine_call(SPINE_TOOL_CLOSE, "close-conflict"),
        1,
    );
    let open_failed_output = failed_function_output_text(
        "open-conflict",
        "SPINE_TOOL_USE_FAILED: multiple Spine control tool requests in one assistant message. No Spine control action was applied. Retry with valid Spine tool arguments.",
    );
    let (open_output, open_resp_raw, open_resp_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, open_failed_output, 2);
    let close_failed_output = failed_function_output_text(
        "close-conflict",
        "SPINE_TOOL_USE_FAILED: multiple Spine control tool requests in one assistant message. No Spine control action was applied. Retry with valid Spine tool arguments.",
    );
    let (close_output, close_resp_raw, close_resp_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, close_failed_output, 3);

    for output in [&open_output, &close_output] {
        let ResponseItem::FunctionCallOutput { output, .. } = output else {
            panic!("expected conflict rejection output");
        };
        let FunctionCallOutputBody::Text(text) = &output.body else {
            panic!("expected conflict rejection text");
        };
        assert!(text.starts_with("SPINE_TOOL_USE_FAILED:"));
        assert_eq!(output.success, Some(false));
    }

    runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "open-conflict".to_string(),
            request_call_ids: vec!["open-conflict".to_string(), "close-conflict".to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: open_raw,
                    context_index: open_context,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: close_raw,
                    context_index: close_context,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: open_resp_raw,
                    context_index: open_resp_context,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: close_resp_raw,
                    context_index: close_resp_context,
                },
            ],
        })
        .expect("conflicting controls should be ordinary");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ] if segments == &vec![
                tool_req(open_raw, open_context),
                tool_req(close_raw, close_context),
                tool_resp(open_resp_raw, open_resp_context),
                tool_resp(close_resp_raw, close_resp_context),
            ]
        )
    ));
    assert!(!runtime.control_call_ids.contains("open-conflict"));
    assert!(!runtime.control_call_ids.contains("close-conflict"));
}

#[test]
fn failed_spine_control_batched_with_ordinary_tool_stays_one_ordinary_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, spine_raw, spine_context) = observe_item_at_context_index(
        &mut runtime,
        &mut raw,
        spine_call(SPINE_TOOL_OPEN, "bad-open"),
        0,
    );
    let ordinary_request = ordinary_call("shell_command", "shell-1");
    let ordinary_raw = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let ordinary_context = 1;
    raw.push(Some(ordinary_request.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record ordinary request");
    runtime
        .observe_context_item(ordinary_raw, ordinary_context, &ordinary_request)
        .expect("observe ordinary request");

    let failed_output = failed_function_output_text(
        "bad-open",
        "SPINE_TOOL_USE_FAILED: failed to parse function arguments. No Spine control action was applied. Retry with valid Spine tool arguments.",
    );
    let (_, spine_resp_raw, spine_resp_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, failed_output, 2);
    let ordinary_output = function_output("shell-1");
    let ordinary_resp_raw = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let ordinary_resp_context = 3;
    raw.push(Some(ordinary_output.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record ordinary output");
    runtime
        .observe_context_item(ordinary_resp_raw, ordinary_resp_context, &ordinary_output)
        .expect("observe ordinary output");

    runtime
        .observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &[
                ("bad-open".to_string(), spine_resp_raw, spine_resp_context),
                (
                    "shell-1".to_string(),
                    ordinary_resp_raw,
                    ordinary_resp_context,
                ),
            ],
            &raw,
        )
        .expect("failed Spine control batched with ordinary tool should be ordinary");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ] if segments == &vec![
                tool_req(spine_raw, spine_context),
                tool_req(ordinary_raw, ordinary_context),
                tool_resp(spine_resp_raw, spine_resp_context),
                tool_resp(ordinary_resp_raw, ordinary_resp_context),
            ]
        )
    ));
    assert!(!runtime.control_call_ids.contains("bad-open"));
}

#[test]
fn grouped_spine_failure_forces_ordinary_even_when_commit_output_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, open_raw, open_context) = observe_item_at_context_index(
        &mut runtime,
        &mut raw,
        spine_call(SPINE_TOOL_OPEN, "bad-open"),
        0,
    );
    let shell_request = ordinary_call("shell_command", "shell-commit");
    let shell_raw = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let shell_context = 1;
    raw.push(Some(shell_request));
    runtime.observe_raw_items(1).expect("record shell request");
    runtime
        .observe_context_item(
            shell_raw,
            shell_context,
            raw[1].as_ref().expect("shell request"),
        )
        .expect("observe shell request");

    let failed_open_output = failed_function_output_text(
        "bad-open",
        "SPINE_TOOL_USE_FAILED: failed to parse function arguments. No Spine control action was applied. Retry with valid Spine tool arguments.",
    );
    let (_, open_resp_raw, open_resp_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, failed_open_output, 2);
    let shell_output = function_output("shell-commit");
    let shell_resp_raw = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let shell_resp_context = 3;
    raw.push(Some(shell_output));
    runtime.observe_raw_items(1).expect("record shell output");
    runtime
        .observe_context_item(
            shell_resp_raw,
            shell_resp_context,
            raw[3].as_ref().expect("shell output"),
        )
        .expect("observe shell output");

    runtime
        .observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &[
                ("bad-open".to_string(), open_resp_raw, open_resp_context),
                (
                    "shell-commit".to_string(),
                    shell_resp_raw,
                    shell_resp_context,
                ),
            ],
            &raw,
        )
        .expect("failed Spine output should force ordinary grouped toolcall");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
        ] if root.summary == "root" && matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ] if segments == &vec![
                tool_req(open_raw, open_context),
                tool_req(shell_raw, shell_context),
                tool_resp(open_resp_raw, open_resp_context),
                tool_resp(shell_resp_raw, shell_resp_context),
            ]
        )
    ));
    assert!(
        !runtime.control_call_ids.contains("bad-open"),
        "failed grouped Spine control output must not leave control classification"
    );
}
