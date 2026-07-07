use super::*;

#[test]
fn trim_slice_head_rewrites_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("abcdefg ");
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

    assert_eq!(
        runtime
            .slice_tool_response_head("trim_0", 7, &raw)
            .expect("slice succeeds"),
        SpineTrimOutcome::Sliced {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abcdefg");
    assert_eq!(function_output_text_content(&output), long_text);
    assert!(matches!(
        runtime.store.trim_events().expect("persisted trim events").as_slice(),
        [
            LoggedTrimEvent {
                event: TrimEvent::Candidate { trim_id, .. },
                ..
            },
            LoggedTrimEvent {
                event: TrimEvent::Sliced { trim_id: sliced_id, .. },
                ..
            }
        ] if trim_id == "trim_0" && sliced_id == "trim_0"
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_variable_context_for_test(&raw)
        .expect("materialize replay");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        "abcdefg"
    );
}
