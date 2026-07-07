use super::*;

#[test]
fn trim_tool_response_does_not_retry_old_id_after_missed_attempt_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("important raw output ");
    let output = function_output_text("long-tool", &long_text);
    let trim_request = spine_call(SPINE_TOOL_TRIM, "trim-miss");
    let trim_output = function_output_text("trim-miss", "Do not retry this trim id.");
    let raw = vec![
        Some(request.clone()),
        Some(output.clone()),
        Some(trim_request.clone()),
        Some(trim_output.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record target raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe target request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe target output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe target completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("unknown_trim")
            .expect("unknown trim id misses"),
        SpineTrimOutcome::Miss {
            trim_id: "unknown_trim".to_string()
        }
    );

    runtime
        .observe_raw_items(2)
        .expect("record committed trim attempt raw");
    runtime
        .observe_context_item(2, 2, &trim_request)
        .expect("observe trim attempt request");
    runtime
        .observe_context_item(3, 3, &trim_output)
        .expect("observe trim attempt output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("trim-miss", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe committed trim attempt as latest toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old target trim id is no longer in previous completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missed attempt commit must not make the older target retryable"
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "missed attempt commit must leave the older target body intact under the tag"
    );
}
