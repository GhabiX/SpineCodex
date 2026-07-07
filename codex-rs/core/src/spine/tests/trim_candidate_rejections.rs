use super::*;

#[test]
fn completed_toolcall_does_not_tag_content_items_tool_response_for_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "content-items-tool");
    let output =
        function_output_content_items("content-items-tool", &trim_candidate_text("content item "));
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
            completed_toolcall("content-items-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw)
            .expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}
