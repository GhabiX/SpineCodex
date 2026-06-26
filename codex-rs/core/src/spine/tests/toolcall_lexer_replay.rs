use super::*;

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
            .materialize_history_for_test(&raw)
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
            .materialize_history_for_test(&raw)
            .expect("response alone still waits for completed toolcall hook"),
        Vec::<ResponseItem>::new()
    );
    runtime
        .observe_completed_toolcall(single_request_response_toolcall(
            "ordinary-tool",
            0,
            0,
            1,
            1,
        ))
        .expect("observe completed toolcall");

    let rendered = runtime
        .materialize_history_for_test(&raw)
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
            .materialize_history_for_test(&raw)
            .expect("replayed toolcall renders"),
        vec![request, output]
    );
    assert_eq!(replayed.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(replayed.parse_stack_toolcall_leaf_count_for_test(), 1);
}
