use super::*;

#[test]
fn ordinary_tool_items_shift_as_toolcall_token_and_render_full_transaction() {
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
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut parse_stack = runtime.parse_stack().clone();

    parse_stack
        .shift(
            SpineToken::ToolCall {
                segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
            },
            &runtime.archive(),
        )
        .expect("shift completed toolcall");

    assert_eq!(
        parse_stack.symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        }])
    );
    assert_eq!(
        render_parse_stack_to_context(&parse_stack, &raw).expect("render ordinary toolcall"),
        vec![request, output_1, output_2]
    );
}

#[test]
fn ordinary_tool_transaction_observes_toolcall_leaf_and_replays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ResponseItem::FunctionCall {
        id: None,
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{\"command\":\"pwd\"}".to_string(),
        call_id: "ordinary-tool".to_string(),
    };
    let output = function_output("ordinary-tool");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record request raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("request alone is not a completed toolcall"),
        Vec::<ResponseItem>::new()
    );
    assert_eq!(runtime.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 0);
    assert!(!matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::Msg { .. })
    ));

    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output raw/context");
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("response alone still waits for completed toolcall hook"),
        Vec::<ResponseItem>::new()
    );
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "ordinary-tool",
            vec![tool_req(0, 0), tool_resp(1, 1)],
        ))
        .expect("observe completed toolcall");

    let rendered = runtime
        .materialize_history(&raw)
        .expect("toolcall renders full transaction");
    assert_eq!(rendered, vec![request.clone(), output.clone()]);
    assert_eq!(runtime.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 1);
    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![event_tool_req(0, 0), event_tool_resp(1, 1)]
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    assert_eq!(
        replayed
            .materialize_history(&raw)
            .expect("replayed toolcall renders"),
        vec![request, output]
    );
    assert_eq!(replayed.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(replayed.parse_stack_toolcall_leaf_count_for_test(), 1);
}
