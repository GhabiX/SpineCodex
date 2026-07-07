use super::*;

#[test]
fn fork_after_trim_clear_preserves_projection_and_allocates_non_colliding_trim_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let request_1 = ordinary_call("shell_command", "first-long-tool");
    let output_1 =
        function_output_text("first-long-tool", &trim_candidate_text("first raw output "));
    let request_2 = ordinary_call("shell_command", "second-long-tool");
    let output_2 = function_output_text(
        "second-long-tool",
        &trim_candidate_text("second raw output "),
    );
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut source = SpineRuntime::load_or_create(&source_rollout, 0).expect("create source");

    source.observe_raw_items(2).expect("record source raw");
    source
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    source
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    source
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("first-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    source
        .trim_tool_response("trim_0")
        .expect("clear first long output");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true, true]);
    let target = SpineRuntime::load_for_rollout_items(&target_rollout, &raw[..2], &[])
        .expect("load target")
        .expect("target sidecar exists");
    let target_visible = target
        .materialize_variable_context_for_test(&raw[..2])
        .expect("materialize target");
    assert_eq!(
        function_output_text_content(&target_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    drop(target);

    let mut forked = SpineRuntime::load_or_create(&target_rollout, 2).expect("load fork writer");
    forked.observe_raw_items(2).expect("record second raw");
    forked
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    forked
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    forked
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("second-long-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    let fork_visible = forked
        .materialize_variable_context_for_test(&raw)
        .expect("materialize fork");
    assert_eq!(
        function_output_text_content(&fork_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        function_output_text_content(&fork_visible[3]).starts_with("[TRIM_ID: trim_2]\n"),
        "fork must continue after copied candidate+clear seqs without reusing trim_0"
    );
}
