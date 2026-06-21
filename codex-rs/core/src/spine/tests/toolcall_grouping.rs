use super::*;

#[test]
fn completed_toolcall_groups_request_and_all_outputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ResponseItem::FunctionCall {
        id: None,
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{\"command\":\"pwd\"}".to_string(),
        call_id: "ordinary-tool".to_string(),
    };
    let output_1 = function_output("ordinary-tool");
    let output_2 = function_output("ordinary-tool");
    let raw = vec![
        Some(request.clone()),
        Some(output_1.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(3).expect("record tool raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    runtime
        .observe_context_item(2, 2, &output_2)
        .expect("observe second output");
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "ordinary-tool",
            vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        ))
        .expect("observe completed multi-response toolcall");

    assert_eq!(
        runtime.parse_stack().symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        }])
    );
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("render multi-response toolcall"),
        vec![request, output_1, output_2]
    );
}

#[test]
fn completed_toolcall_preserves_multiple_requests_and_clears_all_request_anchors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "tool-1");
    let request_2 = ordinary_call("tool_search", "tool-2");
    let output_1 = function_output("tool-1");
    let output_2 = function_output("tool-2");
    let raw = vec![
        Some(request_1.clone()),
        Some(request_2.clone()),
        Some(output_1.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(4).expect("record grouped raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(2, 2, &output_1)
        .expect("observe first response");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second response");
    runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "tool-1".to_string(),
            request_call_ids: vec!["tool-1".to_string(), "tool-2".to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 0,
                    context_index: 0,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 1,
                    context_index: 1,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 2,
                    context_index: 2,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 3,
                    context_index: 3,
                },
            ],
        })
        .expect("observe grouped completed toolcall");

    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![
                event_tool_req(0, 0),
                event_tool_req(1, 1),
                event_tool_resp(2, 2),
                event_tool_resp(3, 3),
            ]
    ));
    assert_eq!(
        runtime.parse_stack().symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![
                tool_req(0, 0),
                tool_req(1, 1),
                tool_resp(2, 2),
                tool_resp(3, 3),
            ],
        }])
    );
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("render grouped toolcall"),
        vec![request_1, request_2.clone(), output_1, output_2]
    );

    runtime.observe_raw_items(1).expect("record reused request");
    runtime
        .observe_context_item(4, 4, &request_2)
        .expect("completed grouped toolcall clears every request anchor");
}
