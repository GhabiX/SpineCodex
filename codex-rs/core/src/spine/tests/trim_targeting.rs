use super::*;

#[test]
fn trim_tool_response_only_matches_latest_completed_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "long-tool");
    let output_1 = function_output_text("long-tool", &trim_candidate_text("old output "));
    let request_2 = ordinary_call("shell_command", "short-tool");
    let output_2 = function_output("short-tool");
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record first raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    runtime.observe_raw_items(2).expect("record second raw");
    runtime
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old trim id misses after newer completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    assert!(matches!(
        runtime.store.trim_events().expect("trim events").as_slice(),
        [LoggedTrimEvent {
            event: TrimEvent::Candidate { trim_id, .. },
            ..
        }] if trim_id == "trim_0"
    ));
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "miss must not clear the old output"
    );
}
