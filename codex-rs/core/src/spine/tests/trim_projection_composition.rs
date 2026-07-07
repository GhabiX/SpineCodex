use super::*;

#[test]
fn trim_repeated_slice_applies_to_current_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!(
        "{}abcdef{}",
        trim_candidate_text("prefix "),
        trim_candidate_text(" suffix")
    );
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
        .slice_tool_response_anchor("trim_0", "abcdef", 0, 0, &raw)
        .expect("first slice succeeds");
    runtime
        .slice_tool_response_head("trim_0", 3, &raw)
        .expect("second slice succeeds");
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_variable_context_for_test(&raw)
        .expect("materialize replay");
    assert_eq!(function_output_text_content(&replayed_rendered[1]), "abc");
}
