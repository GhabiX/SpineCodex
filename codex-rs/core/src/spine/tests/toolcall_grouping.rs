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
