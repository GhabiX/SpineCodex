use super::*;

#[test]
fn pending_ordinary_tool_response_flushes_before_next_non_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = ordinary_call("shell_command", "interrupted-tool");
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record request raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");

    let output = failed_function_output_text("interrupted-tool", "aborted by user");
    raw.push(Some(output.clone()));
    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe aborted output");

    let next = text_item("next user message after interrupt");
    raw.push(Some(next));
    runtime.observe_raw_items(1).expect("record next raw");
    runtime
        .observe_context_item(2, 2, raw[2].as_ref().expect("next item"))
        .expect("pending output must flush before non-toolcall");

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
                    SpineTreeNode::MsgAsLeafNode { .. },
                ] if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
            )
        ),
        "unexpected parse stack: {:#?}",
        runtime.parse_stack()
    );

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("replay interrupted ordinary toolcall")
        .expect("sidecar exists");
    assert_eq!(
        materialized_trace_signature(&replayed, &raw),
        vec![
            "tool-call:shell_command:interrupted-tool".to_string(),
            "tool-output:interrupted-tool:aborted by user".to_string(),
            "user:next user message after interrupt".to_string(),
        ]
    );
}
