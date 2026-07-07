use super::*;

#[test]
fn trim_slice_tail_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}TAIL-END", trim_candidate_text("prefix "));
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_tail("trim_0", 8, &raw)
        .expect("slice succeeds");
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "TAIL-END");
}
