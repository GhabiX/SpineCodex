use super::*;
use crate::spine::render::render_parse_stack_to_context;

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
